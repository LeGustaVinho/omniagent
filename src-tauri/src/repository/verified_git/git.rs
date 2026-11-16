use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_DELETE_ON_CLOSE;

use super::CommitIntent;
use crate::{
    providers::configuration::{open_locked_local_file, plain_metadata},
    repository::{
        CheckoutPaths, ManagedWorktree,
        candidates::read_status,
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
        })
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
        fs::write(live.join("selected.txt"), "base\n")?;
        fs::write(live.join("staged.txt"), "unselected base\n")?;
        git_query(&git, &live, &["add", "--", "selected.txt", "staged.txt"])?;
        let base = fixture_commit(&git, &live, "Initial checkout")?;
        let paths = inspect(&live)?;

        let isolated_path = isolated.to_str().context("fixture path is not Unicode")?;
        git_query(
            &git,
            &live,
            &["clone", "--quiet", "--no-local", ".", isolated_path],
        )?;
        git_query(&git, &isolated, &["config", "core.autocrlf", "false"])?;
        fs::write(isolated.join("selected.txt"), "accepted\n")?;
        fs::write(isolated.join("accepted.bin"), [0, 5, 255])?;
        git_query(
            &git,
            &isolated,
            &["add", "--", "selected.txt", "accepted.bin"],
        )?;
        let tree = git_query(&git, &isolated, &["write-tree"])?
            .trim()
            .to_owned();

        fs::write(live.join("selected.txt"), "accepted\n")?;
        fs::write(live.join("accepted.bin"), [0, 5, 255])?;
        fs::write(live.join("staged.txt"), "unrelated stage\n")?;
        fs::write(live.join("other.txt"), "other working copy change\n")?;
        git_query(&git, &live, &["add", "--", "staged.txt"])?;
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
            selection: CandidateSelection::default(),
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
        checkout.advance(&base, &intent.sha)?;
        checkout.align_index(&intent, &base)?;
        let status = git_query(&git, &live, &["status", "--porcelain"])?;
        assert_eq!(
            status.lines().collect::<Vec<_>>(),
            ["M  staged.txt", "?? other.txt"]
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
