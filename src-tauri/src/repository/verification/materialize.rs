use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

use super::{
    super::{CandidateRevision, git_command, git_path, isolated_git::check},
    CandidateSelection, SelectedCandidate,
};

/// A disposable Git checkout containing the immutable selected tree.
pub struct MaterializedCandidate {
    scratch: tempfile::TempDir,
}

impl MaterializedCandidate {
    pub fn path(&self) -> &Path {
        self.scratch.path()
    }

    pub(crate) fn preserve(self) {
        let _ = self.scratch.keep();
    }
}

pub(super) fn selected_tree(
    capture: &CandidateRevision,
    selection: &CandidateSelection,
) -> Result<String> {
    let (git, tree) = replay(capture, selection)?;
    drop(git);
    Ok(tree)
}

pub(super) fn saved_patch(
    capture: &CandidateRevision,
    selected: &SelectedCandidate,
) -> Result<Vec<u8>> {
    let (git, tree) = replay(capture, &selected.selection)?;
    ensure!(
        tree == selected.tree,
        "the saved candidate no longer reproduces its selected tree"
    );
    git.run(
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &selected.base_commit,
            &tree,
            "--",
        ],
        None,
    )
}

pub(super) fn materialize(
    capture: &CandidateRevision,
    selected: &SelectedCandidate,
) -> Result<MaterializedCandidate> {
    let (git, tree) = replay(capture, &selected.selection)?;
    ensure!(
        tree == selected.tree,
        "the saved candidate no longer reproduces its selected tree"
    );
    git.materialize_tree(&tree, &[&selected.base_commit], b"Selected candidate\n")
}

pub(super) fn materialize_test_snapshot(
    capture: &CandidateRevision,
    selected: &SelectedCandidate,
    test_paths: &[String],
    test_only: bool,
    expected_patch: Option<&str>,
) -> Result<Result<(MaterializedCandidate, String), &'static str>> {
    let (git, tree) = replay(capture, &selected.selection)?;
    ensure!(
        tree == selected.tree,
        "The saved test snapshot differs from its selected Git tree"
    );
    let changed = git.run(
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &selected.base_commit,
            &tree,
            "--",
        ],
        None,
    )?;
    let changed = String::from_utf8(changed)?;
    let names = changed
        .split_terminator('\0')
        .collect::<std::collections::HashSet<_>>();
    if !test_paths.iter().all(|path| names.contains(path.as_str())) {
        return Ok(Err(
            "The selected tree lost part of the required test patch",
        ));
    }
    if test_only && names.len() != test_paths.len() {
        return Ok(Err(
            "The red candidate contains changes outside the declared test patch",
        ));
    }
    let literal: Vec<_> = test_paths
        .iter()
        .map(|path| format!(":(top,literal){path}"))
        .collect();
    let mut args = vec![
        "diff",
        "--binary",
        "--full-index",
        "--no-renames",
        "--no-textconv",
        "--no-ext-diff",
        "--no-color",
        &selected.base_commit,
        &tree,
        "--",
    ];
    args.extend(literal.iter().map(String::as_str));
    let patch = git.run(&args, None)?;
    if patch.is_empty() {
        return Ok(Err("A test phase requires a non-empty test patch"));
    }
    let revision = format!("{:x}", Sha256::digest(&patch));
    if expected_patch.is_some_and(|expected| expected != revision) {
        return Ok(Err(
            "The tests changed after red. Verify the new test patch in red.",
        ));
    }
    // Recreate red explicitly from the original base and the authorized test diff.
    let tree = if test_only {
        git.run(&["read-tree", &selected.base_commit], None)?;
        git.apply(&patch)?;
        let reproduced = git.object(&["write-tree"])?;
        ensure!(
            reproduced == tree,
            "The test patch did not reproduce the red candidate"
        );
        reproduced
    } else {
        tree
    };
    Ok(Ok((
        git.materialize_tree(&tree, &[&selected.base_commit], b"Workflow test phase\n")?,
        revision,
    )))
}

pub(crate) fn materialize_maintenance_commit(
    git_common_dir: &Path,
    commit: &str,
    expected_tree: &str,
) -> Result<MaterializedCandidate> {
    let (checkout, tree) = ScratchGit::new(git_common_dir, commit)?.materialize_commit(commit)?;
    ensure!(
        tree == expected_tree,
        "The verified commit no longer matches the accepted tree"
    );
    Ok(checkout)
}

fn replay(
    capture: &CandidateRevision,
    selection: &CandidateSelection,
) -> Result<(ScratchGit, String)> {
    let base = &capture.start.starting.head;
    validate_oid(base)?;
    let changes = capture
        .candidate
        .changes
        .as_ref()
        .context("capture does not contain replayable changes")?;
    let git = ScratchGit::new(&capture.start.git_common_dir, base)?;
    git.run(&["read-tree", base], None)?;
    for patch in [
        &capture.committed_patch_base64,
        &changes.staged_patch_base64,
        &changes.unstaged_patch_base64,
    ] {
        let bytes = STANDARD.decode(patch)?;
        git.apply(&bytes)?;
    }
    let full = git.object(&["write-tree"])?;
    if let Some(paths) = &selection.tracked_paths {
        let diff = git.run(
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                base,
                &full,
                "--",
            ],
            None,
        )?;
        let names = String::from_utf8(diff)?;
        let available = names
            .split_terminator('\0')
            .collect::<std::collections::HashSet<_>>();
        let mut args = vec![
            "diff",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            base,
            &full,
            "--",
        ];
        let mut selected = Vec::new();
        for path in paths {
            ensure!(
                available.contains(path.as_str()),
                "selected tracked path was not changed in the confirmed capture: {path}"
            );
            selected.push(format!(":(top,literal){path}"));
        }
        git.run(&["read-tree", base], None)?;
        if !paths.is_empty() {
            args.extend(selected.iter().map(String::as_str));
            git.apply(&git.run(&args, None)?)?;
        }
    }
    for path in &selection.untracked_paths {
        let file = changes
            .untracked
            .iter()
            .find(|file| file.path == *path)
            .context("selected addition was not in the confirmed capture")?;
        let bytes = STANDARD.decode(
            file.content_base64
                .as_ref()
                .context("addition has no captured bytes")?,
        )?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == file.sha256,
            "selected addition changed since capture"
        );
        let tracked = git.run(
            &["ls-files", "-z", "--", &format!(":(top,literal){path}")],
            None,
        )?;
        ensure!(
            tracked.is_empty(),
            "selected addition collides with a tracked file"
        );
        let blob = git.object_with_input(&["hash-object", "-w", "--stdin"], Some(&bytes))?;
        git.run(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{path}"),
            ],
            None,
        )?;
    }
    let tree = git.object(&["write-tree"])?;
    Ok((git, tree))
}

pub(in crate::repository) struct ScratchGit {
    scratch: tempfile::TempDir,
    executable: PathBuf,
}

impl ScratchGit {
    pub(in crate::repository) fn new(common: &Path, base: &str) -> Result<Self> {
        Self::initialize(common, base, tempfile::tempdir()?)
    }

    pub(in crate::repository) fn new_in(common: &Path, base: &str, storage: &Path) -> Result<Self> {
        Self::initialize(
            common,
            base,
            tempfile::Builder::new()
                .prefix("comparison-")
                .tempdir_in(storage)?,
        )
    }

    fn initialize(common: &Path, base: &str, scratch: tempfile::TempDir) -> Result<Self> {
        let git = Self {
            scratch,
            executable: git_path()?,
        };
        let object_format = if base.len() == 64 { "sha256" } else { "sha1" };
        git.run(
            &[
                "init",
                "--quiet",
                "--template=",
                &format!("--object-format={object_format}"),
            ],
            None,
        )?;
        let objects = common.join("objects");
        let objects = objects.to_str().context("invalid source object path")?;
        ensure!(
            !objects.contains(['\r', '\n']),
            "invalid source object path"
        );
        fs::write(
            git.root().join(".git/objects/info/alternates"),
            format!("{}\n", objects.replace('\\', "/")),
        )?;
        Ok(git)
    }

    pub(in crate::repository) fn root(&self) -> &Path {
        self.scratch.path()
    }

    pub(in crate::repository) fn materialize_tree(
        self,
        tree: &str,
        parents: &[&str],
        message: &[u8],
    ) -> Result<MaterializedCandidate> {
        let mut args = vec!["commit-tree", tree];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        let commit = self.object_with_input(&args, Some(message))?;
        self.checkout_commit(&commit, tree)
    }

    pub(in crate::repository) fn materialize_commit(
        self,
        commit: &str,
    ) -> Result<(MaterializedCandidate, String)> {
        validate_oid(commit)?;
        ensure!(
            self.object(&["rev-parse", "--verify", &format!("{commit}^{{commit}}")])? == commit,
            "The comparison context must be an exact Git commit"
        );
        let tree = self.object(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?;
        let checkout = self.checkout_commit(commit, &tree)?;
        Ok((checkout, tree))
    }

    fn checkout_commit(self, commit: &str, tree: &str) -> Result<MaterializedCandidate> {
        let roots = format!("{commit}\n");
        let pack = self.root().join(".git/objects/pack/candidate");
        self.run(
            &[
                "pack-objects",
                "--quiet",
                "--revs",
                pack.to_str().context("invalid pack path")?,
            ],
            Some(roots.as_bytes()),
        )?;
        fs::remove_file(self.root().join(".git/objects/info/alternates"))?;
        self.run(&["update-ref", "--no-deref", "HEAD", commit], None)?;
        // Isolated checkouts expose canonical Git blob bytes without checkout filters.
        let info = self.root().join(".git/info");
        fs::create_dir_all(&info)?;
        fs::write(
            info.join("attributes"),
            "* -text -eol -filter -ident -working-tree-encoding\n",
        )?;
        self.run(&["read-tree", tree], None)?;
        self.run(&["checkout-index", "--all"], None)?;
        ensure!(
            self.object(&["write-tree"])? == tree,
            "materialized index has changed"
        );
        ensure!(
            self.run(&["status", "--porcelain=v1", "--untracked-files=all"], None)?
                .is_empty(),
            "materialized candidate checkout has unexpected working-tree changes"
        );
        Ok(MaterializedCandidate {
            scratch: self.scratch,
        })
    }

    pub(in crate::repository) fn apply(&self, patch: &[u8]) -> Result<()> {
        if !patch.is_empty() {
            self.run(
                &["apply", "--cached", "--binary", "--whitespace=nowarn", "-"],
                Some(patch),
            )?;
        }
        Ok(())
    }

    pub(in crate::repository) fn object(&self, args: &[&str]) -> Result<String> {
        self.object_with_input(args, None)
    }

    fn object_with_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        let result = String::from_utf8(self.run(args, input)?)?;
        let result = result.trim();
        validate_oid(result)?;
        Ok(result.into())
    }

    pub(in crate::repository) fn command(&self) -> std::process::Command {
        let mut command = git_command(&self.executable, self.root());
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root().join(".git/empty-config"))
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "OmniAgent")
            .env("GIT_AUTHOR_EMAIL", "candidate@omniagent.invalid")
            .env("GIT_AUTHOR_DATE", "@0 +0000")
            .env("GIT_COMMITTER_NAME", "OmniAgent")
            .env("GIT_COMMITTER_EMAIL", "candidate@omniagent.invalid")
            .env("GIT_COMMITTER_DATE", "@0 +0000")
            .env_remove("GIT_EXTERNAL_DIFF")
            .args([
                "-c",
                "core.hooksPath=NUL",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.symlinks=false",
                "-c",
                "core.protectNTFS=true",
            ]);
        command
    }

    pub(in crate::repository) fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut command = self.command();
        command.args(args);
        if let Some(bytes) = input {
            use std::io::{Seek, Write};
            let mut data = tempfile::tempfile()?;
            data.write_all(bytes)?;
            data.rewind()?;
            command.stdin(Stdio::from(data));
        }
        check(command.output()?, args[0])
    }
}

fn validate_oid(oid: &str) -> Result<()> {
    ensure!(
        matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid Git object identifier"
    );
    Ok(())
}
