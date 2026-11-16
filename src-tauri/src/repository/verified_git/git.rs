use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Seek, Write},
    mem::size_of,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FileAttributeTagInfo,
    GetFileInformationByHandleEx,
};

use super::CommitIntent;
use crate::{
    providers::configuration::{open_locked_local_file, plain_metadata},
    repository::{
        CheckoutPaths, ManagedWorktree,
        candidates::read_status,
        documents::{lock_directories, relative_name},
        git_command, git_path,
        isolated_git::{IsolatedGit, check},
        verification::{CandidateAcceptance, SelectedCandidate},
        worktrees::verify_managed,
    },
};

pub(super) struct DeliveryCheckout {
    git: PathBuf,
    paths: CheckoutPaths,
    root: PathBuf,
    branch: String,
}

impl DeliveryCheckout {
    pub(super) fn open(
        worktree: &ManagedWorktree,
        git_dir: &str,
        common: &str,
        selected: &SelectedCandidate,
    ) -> Result<Self> {
        ensure!(
            worktree.checkout_id == selected.checkout_id && worktree.branch == selected.branch,
            "the checkout or working branch changed after candidate selection"
        );
        let paths = verify_managed(worktree, git_dir, common)?;
        Ok(Self {
            git: git_path()?,
            paths,
            root: worktree.root.clone(),
            branch: format!("refs/heads/{}", worktree.branch),
        })
    }

    pub(super) fn head(&self) -> Result<String> {
        let head = self.read(&["symbolic-ref", "--quiet", "HEAD"])?;
        ensure!(
            head.trim_ascii_end() == self.branch.as_bytes(),
            "the managed branch is no longer checked out"
        );
        let oid = text_oid(self.read(&["rev-parse", "--verify", &self.branch])?)?;
        Ok(oid)
    }

    pub(super) fn matches(&self, sha: &str, candidate: &SelectedCandidate) -> Result<()> {
        validate_oid(sha)?;
        validate_oid(&candidate.base_commit)?;
        validate_oid(&candidate.tree)?;
        ensure!(
            self.read(&["cat-file", "-t", sha])?.trim_ascii_end() == b"commit",
            "the supplied SHA must identify a commit"
        );
        let contents = self.read(&["cat-file", "commit", sha])?;
        let header = contents
            .split(|byte| *byte == b'\n')
            .take_while(|line| !line.is_empty());
        let mut tree = None;
        let mut parents = Vec::new();
        for line in header {
            if let Some(oid) = line.strip_prefix(b"tree ") {
                tree = Some(oid);
            } else if let Some(oid) = line.strip_prefix(b"parent ") {
                parents.push(oid);
            }
        }
        ensure!(
            tree == Some(candidate.tree.as_bytes()),
            "commit tree differs from the accepted candidate"
        );
        ensure!(
            parents == [candidate.base_commit.as_bytes()],
            "commit parent differs from the captured unit base"
        );
        let base_tree = format!("{}^{{tree}}", candidate.base_commit);
        ensure!(
            text_oid(self.read(&["rev-parse", "--verify", &base_tree])?)? != candidate.tree,
            "the accepted unit must contain repository changes"
        );
        Ok(())
    }

    pub(super) fn prepare(
        &self,
        materialized: &Path,
        acceptance: &CandidateAcceptance,
        purpose: &str,
        captured_index_digest: String,
    ) -> Result<CommitIntent> {
        let candidate = &acceptance.candidate;
        validate_oid(&candidate.base_commit)?;
        validate_oid(&candidate.tree)?;
        let date = OffsetDateTime::parse(&acceptance.accepted_at, &Rfc3339)?;
        let date = format!("@{} +0000", date.unix_timestamp());
        let mut create = command(&self.git, materialized);
        create
            .env("GIT_AUTHOR_NAME", "OmniAgent")
            .env("GIT_AUTHOR_EMAIL", "automation@omniagent.invalid")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_NAME", "OmniAgent")
            .env("GIT_COMMITTER_EMAIL", "automation@omniagent.invalid")
            .env("GIT_COMMITTER_DATE", date)
            .args([
                "commit-tree",
                "--no-gpg-sign",
                &candidate.tree,
                "-p",
                &candidate.base_commit,
            ]);
        let message = format!("{purpose}\n");
        let sha = text_oid(with_input(
            create,
            message.as_bytes(),
            "create approved commit object",
        )?)?;

        // The pack is built in the app-owned candidate checkout. Exclude the
        // captured base because its objects already belong to the repository.
        let roots = format!("{sha}\n^{}\n", candidate.base_commit);
        let mut pack = tempfile::tempfile()?;
        let mut produce = command(&self.git, materialized);
        produce
            .args(["pack-objects", "--stdout", "--revs", "--quiet"])
            .stdin(Stdio::from(input_file(roots.as_bytes())?))
            .stdout(Stdio::from(pack.try_clone()?));
        check(produce.output()?, "pack the accepted changes")?;
        pack.rewind()?;
        check(
            command(&self.git, Path::new(&self.paths.git_common_dir))
                .env("GIT_DIR", &self.paths.git_common_dir)
                .args(["index-pack", "--stdin"])
                .stdin(Stdio::from(pack))
                .output()?,
            "import approved Git objects",
        )?;
        self.matches(&sha, candidate)?;
        Ok(CommitIntent {
            sha,
            captured_index_digest,
            acceptance_artifact_id: None,
        })
    }

    /// Recheck only entries contributed by the approved tree. The synthetic index
    /// and Git's passive options apply the same worktree comparison as capture.
    /// Keep existing inputs locked through ref publication.
    pub(super) fn lock_current_selection(
        &self,
        candidate: &SelectedCandidate,
    ) -> Result<Vec<File>> {
        let diff = self.read(&[
            "diff-tree",
            "--raw",
            "--no-abbrev",
            "--no-commit-id",
            "--no-renames",
            "-r",
            "-z",
            &candidate.base_commit,
            &candidate.tree,
        ])?;
        let changes = tree_changes(&diff)?;
        let view = IsolatedGit::open(&self.paths, &self.root, &candidate.base_commit)?;
        let is_untracked = |path: &[u8]| {
            candidate
                .selection
                .untracked_paths
                .iter()
                .any(|name| name.as_bytes() == path)
        };
        let check_index_modes = changes
            .iter()
            .any(|change| change.has_regular_mode_change() && !is_untracked(change.path))
            && view
                .read(&["config", "--type=bool", "--get", "core.filemode"])?
                .trim_ascii_end()
                == b"false";
        let mut locks = Vec::new();
        if check_index_modes {
            // With filemode disabled Git derives the worktree mode from the
            // index. Pin the real index through ref publication so a concurrent
            // writer cannot replace the mode after its staged value is checked.
            locks.push(open_locked_local_file(
                &Path::new(&self.paths.git_dir).join("index"),
            )?);
        }
        view.copy_index(&self.paths)?;
        let index = String::from_utf8(view.read(&["ls-files", "--stage", "-z"])?)?;
        let tracked = indexed_modes(&index)?;
        fs::remove_file(view.index_path())?;
        view.read(&["read-tree", &candidate.tree])?;
        let mut parents = HashSet::new();
        let mut paths = Vec::new();
        let mut deleted = Vec::new();
        for change in changes {
            let name = std::str::from_utf8(change.path)?;
            let path = self.root.join(name);
            ensure!(
                relative_name(&self.root, &path)? == name,
                "invalid approved path: {name}"
            );
            // A staged removal may deliberately leave a local untracked file.
            if change.mode == "000000" && !tracked.contains_key(name) {
                continue;
            }
            let addition = is_untracked(change.path);
            ensure!(
                addition || tracked.contains_key(name),
                "selected path is no longer tracked: {name}"
            );
            if check_index_modes && !addition && change.has_regular_mode_change() {
                ensure!(
                    tracked.get(name).copied().flatten() == Some(change.mode),
                    "selected file mode changed in the index: {name}"
                );
            }
            let parent = approved_parent(&path)?;
            if parents.insert(parent.to_owned()) {
                locks.extend(lock_directories(parent)?);
            }
            if change.mode == "000000" {
                deleted.push(path);
                continue;
            }
            if change.mode == "160000" {
                locks.extend(lock_directories(&path)?);
                paths.push(format!(":(top,literal){name}"));
                continue;
            }
            let file = if change.mode == "120000"
                && fs::symlink_metadata(&path)?.file_type().is_symlink()
            {
                lock_symlink(&path)?
            } else {
                open_locked_local_file(&path)
                    .with_context(|| format!("open approved path {name}"))?
            };
            if addition {
                // Untracked selections are stored as raw blobs, with no filters.
                let actual = text_oid(check(
                    view.command()
                        .args(["hash-object", "--stdin"])
                        .stdin(Stdio::from(file.try_clone()?))
                        .output()?,
                    "read selected addition",
                )?)?;
                ensure!(actual == change.oid, "selected addition changed: {name}");
            } else {
                paths.push(format!(":(top,literal){name}"));
            }
            locks.push(file);
        }
        for batch in paths.chunks(32) {
            let mut args = vec![
                "diff",
                "--name-only",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=dirty",
                "-z",
                "--",
            ];
            args.extend(batch.iter().map(String::as_str));
            let changed = view.read(&args)?;
            let listed = String::from_utf8(changed)?;
            ensure!(
                listed.is_empty(),
                "selected worktree paths changed: {}",
                listed.trim_end_matches('\0').replace('\0', ", ")
            );
        }
        for path in deleted {
            match fs::symlink_metadata(&path) {
                // A directory may contain unrelated files at a formerly tracked file's path.
                Ok(entry)
                    if entry.is_dir()
                        && entry.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 =>
                {
                    locks.extend(lock_directories(&path)?);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) => {}
                Err(error) => return Err(error.into()),
                _ => anyhow::bail!("selected deletion was replaced: {}", path.display()),
            }
        }
        Ok(locks)
    }

    pub(super) fn advance(&self, base: &str, sha: &str) -> Result<()> {
        let git_dir = Path::new(&self.paths.git_dir);
        for operation in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
            "sequencer",
            "index.lock",
        ] {
            ensure!(
                plain_metadata(&git_dir.join(operation))?.is_none(),
                "the managed worktree has a Git operation in progress"
            );
        }
        ensure!(
            self.head()? == base,
            "the working branch moved after acceptance"
        );
        self.read(&[
            "update-ref",
            "-m",
            "OmniAgent verified delivery",
            &self.branch,
            sha,
            base,
        ])?;
        Ok(())
    }

    pub(super) fn align_index(&self, intent: &CommitIntent, base: &str) -> Result<()> {
        let git_dir = Path::new(&self.paths.git_dir);
        let index = git_dir.join("index");
        // Observe and replace the real index under Git's own lock name.
        // Windows removes this marker if OmniAgent exits while holding it.
        let _lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(index.with_extension("lock"))
            .context("lock the worktree index for delivery")?;
        // If staging changed since capture, retain it. The verified tree and
        // the ownership of remaining changes are independent of current staging.
        let mut digest = Sha256::new();
        std::io::copy(&mut open_locked_local_file(&index)?, &mut digest)?;
        if format!("{:x}", digest.finalize()) != intent.captured_index_digest {
            return Ok(());
        }
        let diff = self.read(&[
            "diff-tree",
            "--raw",
            "--no-abbrev",
            "--no-commit-id",
            "--no-renames",
            "-r",
            "-z",
            base,
            &intent.sha,
        ])?;
        let updates = index_updates(&diff)?;
        let scratch = tempfile::tempdir_in(git_dir)?;
        let replacement = scratch.path().join("index");
        fs::copy(&index, &replacement)?;
        let mut command = self.command();
        command.env("GIT_INDEX_FILE", &replacement).args([
            "-c",
            "core.splitIndex=false",
            "update-index",
            "-z",
            "--index-info",
        ]);
        with_input(
            command,
            &updates,
            "update verified paths in the worktree index",
        )?;
        fs::rename(replacement, index).context("install the updated worktree index")?;
        Ok(())
    }

    pub(super) fn clean(&self, sha: &str) -> bool {
        let result = (|| -> Result<bool> {
            let view = IsolatedGit::open(&self.paths, &self.root, sha)?;
            view.copy_index(&self.paths)?;
            Ok(read_status(&view)?.is_empty() && self.head()? == sha)
        })();
        result.unwrap_or(false)
    }

    pub(super) fn repository(&self) -> &str {
        &self.paths.git_common_dir
    }

    fn command(&self) -> Command {
        command(&self.git, &self.root)
    }

    fn read(&self, args: &[&str]) -> Result<Vec<u8>> {
        check(self.command().args(args).output()?, args[0])
    }
}

pub(super) fn validate_oid(value: &str) -> Result<()> {
    ensure!(
        matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "a full Git object SHA is required"
    );
    Ok(())
}

fn text_oid(bytes: Vec<u8>) -> Result<String> {
    let oid = String::from_utf8(bytes)?;
    let oid = oid.trim_end();
    validate_oid(oid)?;
    Ok(oid.into())
}

fn command(git: &Path, root: &Path) -> Command {
    let mut command = git_command(git, root);
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "NUL")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.hooksPath=NUL", "-c", "core.fsmonitor=false"]);
    command
}

fn input_file(input: &[u8]) -> Result<File> {
    let mut file = tempfile::tempfile()?;
    file.write_all(input)?;
    file.rewind()?;
    Ok(file)
}

fn with_input(mut command: Command, bytes: &[u8], operation: &str) -> Result<Vec<u8>> {
    check(
        command.stdin(Stdio::from(input_file(bytes)?)).output()?,
        operation,
    )
}

fn index_updates(diff: &[u8]) -> Result<Vec<u8>> {
    let mut clears = Vec::new();
    let mut entries = Vec::new();
    for change in tree_changes(diff)? {
        let TreeChange {
            path,
            before,
            mode,
            oid,
        } = change;
        // Deletions precede additions, including replacement of a file by a directory.
        if before != "000000" {
            write!(clears, "0 {:0<width$}\t", "", width = oid.len())?;
            clears.extend_from_slice(path);
            clears.push(0);
        }
        if mode != "000000" {
            write!(entries, "{mode} {oid}\t")?;
            entries.extend_from_slice(path);
            entries.push(0);
        }
    }
    clears.extend(entries);
    Ok(clears)
}

struct TreeChange<'a> {
    path: &'a [u8],
    before: &'a str,
    mode: &'a str,
    oid: &'a str,
}

impl TreeChange<'_> {
    fn has_regular_mode_change(&self) -> bool {
        matches!(self.mode, "100644" | "100755") && self.mode != self.before
    }
}

fn indexed_modes(index: &str) -> Result<HashMap<&str, Option<&str>>> {
    let mut tracked = HashMap::new();
    for line in index.split_terminator('\0') {
        let (entry, path) = line.split_once('\t').context("missing Git index path")?;
        let mut fields = entry.split_ascii_whitespace();
        let mode = fields.next().context("missing Git index mode")?;
        let _oid = fields.next().context("missing Git index object")?;
        let stage = fields.next().context("missing Git index stage")?;
        let current = tracked.entry(path).or_insert(None);
        if stage == "0" {
            *current = Some(mode);
        }
    }
    Ok(tracked)
}

fn tree_changes(diff: &[u8]) -> Result<Vec<TreeChange<'_>>> {
    let mut changes = Vec::new();
    let fields = diff.split(|byte| *byte == 0);
    let mut fields = fields.take_while(|field| !field.is_empty());
    while let Some(metadata) = fields.next() {
        let path = fields.next().context("missing changed Git path")?;
        let details = std::str::from_utf8(metadata)?;
        let mut details = details
            .strip_prefix(':')
            .context("invalid Git tree change")?
            .split(' ');
        let before = details.next().context("missing previous Git file mode")?;
        let mode = details.next().context("missing new Git file mode")?;
        let _previous = details.next().context("missing previous Git object")?;
        let oid = details.next().context("missing new Git object")?;
        validate_oid(oid)?;
        changes.push(TreeChange {
            path,
            before,
            mode,
            oid,
        });
    }
    Ok(changes)
}

fn approved_parent(path: &Path) -> Result<&Path> {
    for parent in path.ancestors().skip(1) {
        match plain_metadata(parent) {
            Ok(Some(entry)) if entry.is_dir() => return Ok(parent),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {}
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("approved path has no existing directory")
}

fn lock_symlink(path: &Path) -> Result<File> {
    // Pin the link itself, and reject junctions and other reparse points.
    const SYMLINK_REPARSE_TAG: u32 = 0xA000000C;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut tag).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error().into());
    }
    ensure!(
        tag.ReparseTag == SYMLINK_REPARSE_TAG,
        "the selected path is not a symbolic link"
    );
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{
        git_query, inspect,
        verification::{CandidateSelection, CandidateTarget, CandidateVerification},
    };

    #[test]
    fn automatic_commit_uses_the_approved_tree_and_keeps_other_changes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let live = temp.path().join("registered");
        let isolated = temp.path().join("candidate");
        let git = git_path()?;
        fs::create_dir(&live)?;
        git_query(&git, &live, &["init", "--quiet", "--initial-branch=work"])?;
        git_query(&git, &live, &["config", "core.autocrlf", "false"])?;
        git_query(&git, &live, &["config", "core.filemode", "false"])?;
        fs::write(live.join("selected.txt"), "base\n")?;
        fs::write(live.join("staged.txt"), "unselected base\n")?;
        fs::write(
            live.join("removed.txt"),
            "keep locally after removing from Git\n",
        )?;
        git_query(
            &git,
            &live,
            &["add", "--", "selected.txt", "staged.txt", "removed.txt"],
        )?;
        let base = fixture_commit(&git, &live, "Initial checkout")?;
        let paths = inspect(&live)?;

        let isolated_path = isolated.to_str().context("fixture path is not Unicode")?;
        git_query(
            &git,
            &live,
            &[
                "clone",
                "--quiet",
                "--no-local",
                "--config",
                "core.autocrlf=false",
                ".",
                isolated_path,
            ],
        )?;
        fs::write(isolated.join("selected.txt"), "accepted\n")?;
        fs::write(isolated.join("accepted.bin"), [0, 5, 255])?;
        git_query(
            &git,
            &isolated,
            &["add", "--", "selected.txt", "accepted.bin"],
        )?;
        git_query(
            &git,
            &isolated,
            &["update-index", "--chmod=+x", "--", "selected.txt"],
        )?;
        git_query(&git, &isolated, &["rm", "--", "removed.txt"])?;
        let tree = git_query(&git, &isolated, &["write-tree"])?
            .trim()
            .to_owned();

        fs::write(live.join("selected.txt"), "accepted\n")?;
        fs::write(live.join("accepted.bin"), [0, 5, 255])?;
        fs::write(live.join("staged.txt"), "unrelated stage\n")?;
        fs::write(live.join("other.txt"), "other working copy change\n")?;
        git_query(&git, &live, &["add", "--", "staged.txt"])?;
        git_query(
            &git,
            &live,
            &["update-index", "--chmod=+x", "--", "selected.txt"],
        )?;
        git_query(&git, &live, &["rm", "--cached", "--", "removed.txt"])?;
        let index = Path::new(&paths.git_dir).join("index");
        let index_digest = format!("{:x}", Sha256::digest(fs::read(index)?));

        let worktree = ManagedWorktree {
            owner_task_id: "task".into(),
            project_id: "project".into(),
            checkout_id: "checkout".into(),
            root: dunce::canonicalize(&live)?,
            branch: "work".into(),
            base_commit: base.clone(),
        };
        let candidate = SelectedCandidate {
            id: "candidate".into(),
            target: CandidateTarget {
                project_id: "project".into(),
                task_id: "task".into(),
                unit_id: "unit".into(),
                run_id: "attempt".into(),
                capture_revision: "capture".into(),
            },
            checkout_id: "checkout".into(),
            branch: "work".into(),
            base_commit: base.clone(),
            tree,
            selection: CandidateSelection {
                tracked_paths: None,
                untracked_paths: vec!["accepted.bin".into()],
            },
        };
        let acceptance = CandidateAcceptance {
            id: "acceptance".into(),
            candidate,
            verification: CandidateVerification {
                id: "verification".into(),
                candidate_id: "candidate".into(),
                policy_revision: "policy".into(),
                started_at: "2026-11-16T09:00:00.000Z".into(),
                finished_at: Some("2026-11-16T09:01:00.000Z".into()),
                gates: Vec::new(),
            },
            accepted_at: "2026-11-16T09:02:00.000Z".into(),
        };
        let checkout = DeliveryCheckout::open(
            &worktree,
            &paths.git_dir,
            &paths.git_common_dir,
            &acceptance.candidate,
        )?;
        assert!(checkout.matches(&base, &acceptance.candidate).is_err());
        let intent = checkout.prepare(&isolated, &acceptance, "Implement unit", index_digest)?;
        assert_eq!(checkout.head()?, base);
        let attributes = live.join(".gitattributes");
        fs::write(&attributes, "selected.txt filter=unapproved\n")?;
        git_query(
            &git,
            &live,
            &[
                "config",
                "filter.unapproved.clean",
                "echo unexpected > filter-ran; cat",
            ],
        )?;
        let selected = checkout.lock_current_selection(&acceptance.candidate)?;
        let selected_blob = git_query(&git, &live, &["rev-parse", ":selected.txt"])?;
        let staged_mode_change = format!("100644,{},selected.txt", selected_blob.trim());
        assert!(
            git_query(
                &git,
                &live,
                &["update-index", "--cacheinfo", &staged_mode_change]
            )
            .is_err()
        );
        assert!(
            git_query(&git, &live, &["ls-files", "--stage", "--", "selected.txt"])?
                .starts_with("100755 ")
        );
        checkout.advance(&base, &intent.sha)?;
        assert!(!live.join("filter-ran").exists());
        drop(selected);
        fs::remove_file(attributes)?;
        checkout.align_index(&intent, &base)?;
        let status = git_query(&git, &live, &["status", "--porcelain"])?;
        assert_eq!(
            status.lines().collect::<Vec<_>>(),
            ["M  staged.txt", "?? other.txt", "?? removed.txt"]
        );
        assert_eq!(checkout.head()?, intent.sha);
        assert!(!checkout.clean(&intent.sha));
        let different_parent = git_query(
            &git,
            &live,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@omniagent.invalid",
                "commit-tree",
                "--no-gpg-sign",
                &acceptance.candidate.tree,
                "-p",
                &intent.sha,
                "-m",
                "Unrelated follow-up",
            ],
        )?;
        let wrong = checkout
            .matches(different_parent.trim(), &acceptance.candidate)
            .unwrap_err();
        assert!(wrong.to_string().contains("parent differs"));
        assert_eq!(
            git_query(&git, &live, &["show", "HEAD:staged.txt"])?,
            "unselected base\n"
        );
        assert_eq!(
            git_query(&git, &live, &["show", ":staged.txt"])?,
            "unrelated stage\n"
        );
        checkout.align_index(&intent, &base)?;
        assert_eq!(git_query(&git, &live, &["status", "--porcelain"])?, status);
        assert!(checkout.advance(&base, &intent.sha).is_err());
        Ok(())
    }

    fn fixture_commit(git: &Path, root: &Path, message: &str) -> Result<String> {
        git_query(
            git,
            root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@omniagent.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        )?;
        Ok(git_query(git, root, &["rev-parse", "HEAD"])?.trim().into())
    }
}
