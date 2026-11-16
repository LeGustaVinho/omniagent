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

pub(super) fn materialize(
    capture: &CandidateRevision,
    selected: &SelectedCandidate,
) -> Result<MaterializedCandidate> {
    let (git, tree) = replay(capture, &selected.selection)?;
    ensure!(
        tree == selected.tree,
        "the saved candidate no longer reproduces its selected tree"
    );
    let base = &selected.base_commit;
    let commit = git.object_with_input(
        &["commit-tree", &tree, "-p", base],
        Some(b"Selected candidate\n"),
    )?;
    let roots = format!("{commit}\n");
    let pack = git.root().join(".git/objects/pack/candidate");
    git.run(
        &[
            "pack-objects",
            "--quiet",
            "--revs",
            pack.to_str().context("invalid pack path")?,
        ],
        Some(roots.as_bytes()),
    )?;
    fs::remove_file(git.root().join(".git/objects/info/alternates"))?;
    git.run(&["update-ref", "--no-deref", "HEAD", &commit], None)?;
    // The candidate test sees canonical Git blob bytes with no checkout filters.
    let info = git.root().join(".git/info");
    fs::create_dir_all(&info)?;
    fs::write(
        info.join("attributes"),
        "* -text -eol -filter -ident -working-tree-encoding\n",
    )?;
    git.run(&["read-tree", &tree], None)?;
    git.run(&["checkout-index", "--all"], None)?;
    ensure!(
        git.object(&["write-tree"])? == tree,
        "materialized index has changed"
    );
    ensure!(
        git.run(&["status", "--porcelain=v1", "--untracked-files=all"], None)?
            .is_empty(),
        "materialized candidate checkout has unexpected working-tree changes"
    );
    Ok(MaterializedCandidate {
        scratch: git.scratch,
    })
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

struct ScratchGit {
    scratch: tempfile::TempDir,
    executable: PathBuf,
}

impl ScratchGit {
    fn new(common: &Path, base: &str) -> Result<Self> {
        let git = Self {
            scratch: tempfile::tempdir()?,
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

    fn root(&self) -> &Path {
        self.scratch.path()
    }

    fn apply(&self, patch: &[u8]) -> Result<()> {
        if !patch.is_empty() {
            self.run(
                &["apply", "--cached", "--binary", "--whitespace=nowarn", "-"],
                Some(patch),
            )?;
        }
        Ok(())
    }

    fn object(&self, args: &[&str]) -> Result<String> {
        self.object_with_input(args, None)
    }

    fn object_with_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        let result = String::from_utf8(self.run(args, input)?)?;
        let result = result.trim();
        validate_oid(result)?;
        Ok(result.into())
    }

    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
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
            ])
            .args(args);
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
