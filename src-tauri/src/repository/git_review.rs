use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::windows::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_DELETE_ON_CLOSE;

use crate::{providers::configuration::open_locked_local_file, store::Store};

use super::{
    CheckoutPaths, base_commit, checkout_root,
    documents::lock_directories,
    git_command, git_path, inspect,
    isolated_git::{IsolatedGit, check},
    verify_checkout, with_managed_worktree_writer,
};

mod commit;
mod comparison;
mod content;
pub use comparison::{GitBranchComparison, GitBranchDiff, GitBranchRange, GitBranchRevisions};
pub use content::{GitDiffSource, GitFileSide, GitReviewDiff};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewRevision {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub index: Option<String>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPathStatus {
    pub path: String,
    pub staged: Option<char>,
    pub working: Option<char>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewBranch {
    pub name: String,
    pub reference: String,
    pub sha: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewWorktree {
    pub checkout_id: Option<String>,
    pub root: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub managed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheckoutReview {
    pub checkout_id: String,
    pub root: String,
    pub revision: GitReviewRevision,
    pub managed: bool,
    pub changes: Vec<GitPathStatus>,
    pub branches: Vec<GitReviewBranch>,
    pub worktrees: Vec<GitReviewWorktree>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewCommit {
    pub sha: String,
    pub author: String,
    pub committed_at: String,
    pub subject: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedGitAttempt {
    pub run_id: String,
    pub checkout_id: String,
    pub label: String,
    pub conversation_id: String,
    pub history_deleted: bool,
}

struct RegisteredWorktree {
    id: String,
    root: String,
    managed: bool,
}

struct ReviewScope {
    id: String,
    project: String,
    root: PathBuf,
    owner: Option<String>,
    git_dir: String,
    common: String,
}

impl Store {
    async fn git_review_scope(&self, id: String) -> Result<ReviewScope> {
        let root = checkout_root(self, id.clone()).await?;
        let root = verify_checkout(self, id.clone(), root).await?.root;
        let (project, owner, git_dir, common) = self.execute({
            let id = id.clone();
            move |db| Ok(db.query_row(
                "SELECT c.project_id, m.task_id, c.git_dir, p.git_common_dir FROM checkouts c
                 JOIN projects p ON p.id = c.project_id LEFT JOIN managed_task_worktrees m ON m.checkout_id = c.id WHERE c.id = ?1",
                [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            )?)
        }).await?;
        Ok(ReviewScope {
            id,
            project,
            root,
            owner,
            git_dir,
            common,
        })
    }

    pub async fn git_checkout_review(&self, checkout: String) -> Result<GitCheckoutReview> {
        let scope = self.git_review_scope(checkout).await?;
        let registered = self.execute({
            let project = scope.project.clone();
            move |db| {
                let mut query = db.prepare(
                    "SELECT c.id, c.root, m.checkout_id IS NOT NULL FROM checkouts c
                     LEFT JOIN managed_task_worktrees m ON m.checkout_id = c.id WHERE c.project_id = ?1",
                )?;
                Ok(query.query_map([project], |row| Ok(RegisteredWorktree {
                    id: row.get(0)?, root: row.get(1)?, managed: row.get(2)?,
                }))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }
        }).await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let changes = review.status()?;
            let branches = branches(&scope.root)?;
            let worktrees = worktrees(&scope.root, &registered)?;
            review.verify()?;
            Ok(GitCheckoutReview {
                checkout_id: scope.id,
                root: review.paths.checkout_root,
                revision: review.revision,
                managed: scope.owner.is_some(),
                changes,
                branches,
                worktrees,
            })
        })
        .await?
    }

    pub async fn git_checkout_history(
        &self,
        checkout: String,
        branch: Option<String>,
    ) -> Result<Vec<GitReviewCommit>> {
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let head = match branch {
                Some(selected) => Some(
                    branches(&scope.root)?
                        .into_iter()
                        .find(|branch| branch.reference == selected)
                        .context("the selected branch no longer exists")?
                        .sha,
                ),
                None => base_commit(&scope.root)?,
            };
            let Some(head) = head else {
                return Ok(Vec::new());
            };
            let format = "--format=%H%x00%an%x00%cI%x00%s";
            let bytes = passive(
                &scope.root,
                &[
                    "log",
                    "--no-notes",
                    "--no-show-signature",
                    "--max-count=100",
                    "-z",
                    format,
                    &head,
                    "--",
                ],
            )?;
            let decoded = String::from_utf8_lossy(&bytes);
            let fields: Vec<_> = decoded.split_terminator('\0').collect();
            ensure!(
                fields.len().is_multiple_of(4),
                "Git returned an incomplete history row"
            );
            fields
                .chunks_exact(4)
                .map(|row| {
                    validate_oid(row[0])?;
                    Ok(GitReviewCommit {
                        sha: row[0].into(),
                        author: row[1].into(),
                        committed_at: row[2].into(),
                        subject: row[3].into(),
                    })
                })
                .collect()
        })
        .await?
    }

    pub async fn git_checkout_diff(
        &self,
        checkout: String,
        path: String,
        source: GitDiffSource,
    ) -> Result<GitReviewDiff> {
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            validate_path(&path)?;
            ensure!(
                review
                    .status()?
                    .iter()
                    .any(|entry| entry.path == path && source.contains(entry)),
                "the selected path is no longer in this comparison"
            );
            let diff = content::read_diff(&review, path, source)?;
            review.verify()?;
            Ok(diff)
        })
        .await?
    }

    pub async fn set_git_path_staged(
        &self,
        checkout: String,
        path: String,
        staged: bool,
        expected: GitReviewRevision,
    ) -> Result<()> {
        let scope = self.git_review_scope(checkout).await?;
        validate_path(&path)?;
        let managed = self.execute({
            let checkout = scope.id.clone();
            move |db| Ok(db.query_row("SELECT project_id, task_id FROM managed_task_worktrees WHERE checkout_id = ?1", [checkout],
                |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?))).optional()?)
        }).await?;
        let checkout = scope.id.clone();
        let write = move || stage_path(&scope, &path, staged, &expected);
        match managed {
            Some((project, task)) => {
                with_managed_worktree_writer(self, project, checkout, task, move |_| write()).await
            }
            None => tokio::task::spawn_blocking(write).await?,
        }
    }

    pub async fn git_saved_attempts(&self, project: String) -> Result<Vec<SavedGitAttempt>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT r.id, r.checkout_id, coalesce(t.title, 'Repository attempt'), agent.conversation_id,
                   EXISTS(SELECT 1 FROM history_deleted_attempt_diffs WHERE run_id = r.id)
                 FROM job_worktree_candidates candidate
                 JOIN runs r ON r.id = candidate.run_id JOIN job_attempts a ON a.run_id = r.id
                 JOIN agent_attempts agent ON agent.run_id = r.id
                 JOIN jobs j ON j.id = a.job_id LEFT JOIN project_tasks t ON t.project_id = j.project_id AND t.id = j.task_id
                 WHERE j.project_id = ?1 AND candidate.capture_confirmed = 1
                 ORDER BY r.created_at DESC, r.id LIMIT 100",
            )?;
            Ok(query.query_map(params![project], |row| Ok(SavedGitAttempt {
                run_id: row.get(0)?, checkout_id: row.get(1)?, label: row.get(2)?,
                conversation_id: row.get(3)?, history_deleted: row.get(4)?,
            }))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        }).await
    }
}

struct LiveReview {
    paths: CheckoutPaths,
    root: PathBuf,
    revision: GitReviewRevision,
    view: IsolatedGit,
}

impl LiveReview {
    fn open(scope: &ReviewScope) -> Result<Self> {
        let root = &scope.root;
        let paths = inspect(root)?;
        ensure!(
            paths.git_dir == scope.git_dir && paths.git_common_dir == scope.common,
            "checkout identity changed during Git review"
        );
        let revision = observed_revision(root, Path::new(&paths.git_dir))?;
        let head = revision
            .head
            .as_deref()
            .unwrap_or("ref: refs/heads/omniagent-review-unborn");
        let view = IsolatedGit::open(&paths, root, head)?;
        if revision.index.is_some() {
            view.copy_index(&paths)?;
        }
        Ok(Self {
            paths,
            root: root.to_owned(),
            revision,
            view,
        })
    }

    fn status(&self) -> Result<Vec<GitPathStatus>> {
        let entries = self.view.read(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=all",
            "--ignore-submodules=dirty",
        ])?;
        let mut paths = BTreeMap::new();
        for row in entries
            .split(|byte| *byte == 0)
            .filter(|row| !row.is_empty())
        {
            ensure!(
                row.len() > 3 && row[2] == b' ',
                "Git returned an invalid status entry"
            );
            let path = String::from_utf8(row[3..].to_vec())?;
            let entry = paths.entry(path.clone()).or_insert(GitPathStatus {
                path,
                staged: None,
                working: None,
            });
            entry.staged = entry
                .staged
                .or((row[0] != b' ' && row[0] != b'?').then(|| char::from(row[0])));
            entry.working = entry
                .working
                .or((row[1] != b' ').then(|| char::from(row[1])));
        }
        Ok(paths.into_values().collect())
    }

    fn verify(&self) -> Result<()> {
        let paths = inspect(&self.root)?;
        ensure!(
            paths.git_dir == self.paths.git_dir
                && paths.git_common_dir == self.paths.git_common_dir,
            "the Git worktree changed during review"
        );
        ensure!(
            observed_revision(&self.root, Path::new(&paths.git_dir))? == self.revision,
            "the Git branch, HEAD or index changed; refresh the checkout and review it again"
        );
        Ok(())
    }
}

fn stage_path(
    scope: &ReviewScope,
    path: &str,
    staged: bool,
    expected: &GitReviewRevision,
) -> Result<()> {
    let review = LiveReview::open(scope)?;
    ensure!(
        &review.revision == expected,
        "the Git branch, HEAD or index changed; refresh the checkout and review it again"
    );
    let index_parent = Path::new(&review.paths.git_dir);
    let _directories = lock_directories(index_parent)?;
    let index = index_parent.join("index");
    let _lock = lock_git_file(&index.with_extension("lock")).context("lock the worktree index")?;
    review.verify()?;
    let status = review
        .status()?
        .into_iter()
        .find(|entry| entry.path == path)
        .context("the reviewed path no longer has a change")?;
    if staged {
        ensure!(
            status.working.is_some(),
            "the path has no working change to stage"
        );
        if let Some(_locks) = content::lock_staging_file(&scope.root, path)? {
            review.view.read(&[
                "-c",
                "core.splitIndex=false",
                "--literal-pathspecs",
                "add",
                "--",
                path,
            ])?;
        } else {
            review.view.read(&[
                "-c",
                "core.splitIndex=false",
                "update-index",
                "--force-remove",
                "--",
                path,
            ])?;
        }
    } else {
        ensure!(status.staged.is_some(), "the path has no staged change");
        if review.revision.head.is_some() {
            review.view.read(&[
                "-c",
                "core.splitIndex=false",
                "--literal-pathspecs",
                "reset",
                "-q",
                "HEAD",
                "--",
                path,
            ])?;
        } else {
            review.view.read(&[
                "-c",
                "core.splitIndex=false",
                "update-index",
                "--force-remove",
                "--",
                path,
            ])?;
        }
    }
    let mut replacement = tempfile::NamedTempFile::new_in(index_parent)?;
    std::io::copy(
        &mut open_locked_local_file(&review.view.index_path())?,
        replacement.as_file_mut(),
    )?;
    replacement.as_file().sync_all()?;
    review.verify()?;
    let replacement = replacement.into_temp_path();
    fs::rename(&replacement, index).context("install the reviewed worktree index")?;
    Ok(())
}

fn lock_git_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
}

fn observed_revision(root: &Path, git_dir: &Path) -> Result<GitReviewRevision> {
    let branch = read_branch(root)?;
    let head = base_commit(root)?;
    let index = match open_locked_local_file(&git_dir.join("index")) {
        Ok(mut file) => {
            let mut digest = Sha256::new();
            io::copy(&mut file, &mut digest)?;
            Some(format!("{:x}", digest.finalize()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(GitReviewRevision {
        branch,
        head,
        index,
    })
}

fn read_branch(root: &Path) -> Result<Option<String>> {
    let output = git_command(&git_path()?, root)
        .args(["symbolic-ref", "--quiet", "--no-recurse", "HEAD"])
        .output()?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8(check(output, "resolve the working branch")?)?
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
    ))
}

fn passive_command(root: &Path) -> Result<Command> {
    let mut command = git_command(&git_path()?, root);
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "NUL")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.hooksPath=NUL", "-c", "core.fsmonitor=false"]);
    Ok(command)
}

fn passive(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    // The caller verifies the checkout using Git's normal trust settings.
    // Carry that decision into the isolated command for this checkout only.
    let directory = root.to_string_lossy().replace('\\', "/");
    let output = passive_command(root)?
        .arg("-c")
        .arg(format!("safe.directory={directory}"))
        .args(args)
        .output()?;
    check(output, args[0])
}

fn branches(root: &Path) -> Result<Vec<GitReviewBranch>> {
    let result = String::from_utf8(passive(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(symref)",
            "refs/heads/",
            "refs/remotes/",
        ],
    )?)?;
    result
        .lines()
        .filter_map(|line| {
            let (branch, symbolic) = match line.rsplit_once('\0') {
                Some(parts) => parts,
                None => return Some(Err(anyhow::anyhow!("Git returned an incomplete branch"))),
            };
            symbolic.is_empty().then_some(Ok(branch))
        })
        .map(|branch| {
            let (reference, sha) = branch?
                .split_once('\0')
                .context("Git returned an incomplete branch")?;
            let name = reference
                .strip_prefix("refs/heads/")
                .or_else(|| reference.strip_prefix("refs/remotes/"))
                .context("Git returned an unexpected ref")?;
            validate_oid(sha)?;
            Ok(GitReviewBranch {
                name: name.into(),
                reference: reference.into(),
                sha: sha.into(),
            })
        })
        .collect()
}

pub(super) fn branch_reference(root: &Path, reference: &str) -> Result<GitReviewBranch> {
    branches(root)?
        .into_iter()
        .find(|branch| branch.reference == reference)
        .context("the selected branch no longer exists")
}

fn worktrees(root: &Path, registered: &[RegisteredWorktree]) -> Result<Vec<GitReviewWorktree>> {
    let result = String::from_utf8(passive(root, &["worktree", "list", "--porcelain", "-z"])?)?;
    result
        .split("\0\0")
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let mut fields = entry.split('\0');
            let path = fields
                .next()
                .and_then(|field| field.strip_prefix("worktree "))
                .context("Git returned an incomplete worktree path")?;
            let head = fields
                .clone()
                .find_map(|field| field.strip_prefix("HEAD "))
                .filter(|oid| !oid.bytes().all(|byte| byte == b'0'));
            if let Some(head) = head {
                validate_oid(head)?;
            }
            let branch = fields.find_map(|field| field.strip_prefix("branch refs/heads/"));
            let canonical = dunce::canonicalize(path).ok();
            let checkout = registered
                .iter()
                .find(|saved| canonical.as_deref() == Some(Path::new(&saved.root)));
            Ok(GitReviewWorktree {
                checkout_id: checkout.map(|saved| saved.id.clone()),
                root: path.into(),
                branch: branch.map(str::to_owned),
                head: head.map(str::to_owned),
                managed: checkout.is_some_and(|saved| saved.managed),
            })
        })
        .collect()
}

pub(super) fn validate_oid(oid: &str) -> Result<()> {
    ensure!(
        matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Git returned an invalid object ID"
    );
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    ensure!(!path.is_empty() && !path.contains(['\\', ':', '\0'])
        && Path::new(path).components().all(|component| matches!(component, Component::Normal(part) if !part.eq_ignore_ascii_case(".git"))),
        "select a checkout-relative Git file path");
    Ok(())
}

#[cfg(test)]
mod tests;
