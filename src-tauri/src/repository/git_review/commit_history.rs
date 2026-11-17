use std::process::Stdio;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{
    GitFileSide, IsolatedGit, ReviewScope, base_commit, branch_reference, check, content,
    history::require_commit,
    inspect, passive_output,
    patch::{self, GitReviewPatch},
    validate_oid, validate_path,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryCommitRequest {
    pub branch: Option<String>,
    pub expected_head: String,
    pub sha: String,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitCommitRange {
    pub base: String,
    pub head: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryCommitDetails {
    pub message: String,
    pub parents: Vec<String>,
    pub parent: Option<String>,
    pub files: Vec<GitHistoryCommitFile>,
    pub comparison_unavailable: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryCommitFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub status: char,
}

#[derive(Debug, Serialize)]
pub struct GitHistoryCommitDiff {
    pub path: String,
    pub original: GitFileSide,
    pub modified: GitFileSide,
    pub patch: GitReviewPatch,
}

impl Store {
    pub async fn git_checkout_commit_details(
        &self,
        checkout: String,
        request: GitHistoryCommitRequest,
    ) -> Result<GitHistoryCommitDetails> {
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let (_, _, details) = inspect_commit(&scope, &request)?;
            Ok(details)
        })
        .await?
    }

    pub async fn git_checkout_commit_diff(
        &self,
        checkout: String,
        request: GitHistoryCommitRequest,
        path: String,
    ) -> Result<GitHistoryCommitDiff> {
        validate_path(&path)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let (view, base, details) = inspect_commit(&scope, &request)?;
            if let Some(reason) = details.comparison_unavailable {
                anyhow::bail!("{reason}");
            }
            compared_file(&view, &base, &request.sha, path, &details.files)
        })
        .await?
    }

    pub async fn git_compare_commits(
        &self,
        checkout: String,
        range: GitCommitRange,
    ) -> Result<Vec<GitHistoryCommitFile>> {
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let view = open_commit_range(&scope, &range)?;
            changed_files(&view, &range.base, &range.head)
        })
        .await?
    }

    pub async fn git_compare_commit_file(
        &self,
        checkout: String,
        range: GitCommitRange,
        path: String,
    ) -> Result<GitHistoryCommitDiff> {
        validate_path(&path)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let view = open_commit_range(&scope, &range)?;
            let files = changed_files(&view, &range.base, &range.head)?;
            compared_file(&view, &range.base, &range.head, path, &files)
        })
        .await?
    }
}

fn open_commit_range(scope: &ReviewScope, range: &GitCommitRange) -> Result<IsolatedGit> {
    validate_oid(&range.base)?;
    validate_oid(&range.head)?;
    let view = open_comparison_view(scope, &range.head)?;
    require_commit(&view, &range.base)?;
    require_commit(&view, &range.head)?;
    Ok(view)
}

fn open_comparison_view(scope: &ReviewScope, head: &str) -> Result<IsolatedGit> {
    let paths = inspect(&scope.root)?;
    ensure!(
        paths.git_dir == scope.git_dir && paths.git_common_dir == scope.common,
        "Checkout identity changed during commit review."
    );
    IsolatedGit::open(&paths, &scope.root, head)
}

fn compared_file(
    view: &IsolatedGit,
    base: &str,
    head: &str,
    path: String,
    files: &[GitHistoryCommitFile],
) -> Result<GitHistoryCommitDiff> {
    let file = files
        .iter()
        .find(|file| file.path == path)
        .context("This path is outside the selected commit comparison.")?;
    let previous_path = file.previous_path.as_deref();
    let original = content::read_tree(view, base, previous_path.unwrap_or(&path))?;
    let modified = content::read_tree(view, head, &path)?;
    let patch = patch::history_file(view, base, head, &path, previous_path);
    Ok(GitHistoryCommitDiff {
        path,
        original,
        modified,
        patch,
    })
}

fn inspect_commit(
    scope: &ReviewScope,
    request: &GitHistoryCommitRequest,
) -> Result<(IsolatedGit, String, GitHistoryCommitDetails)> {
    validate_oid(&request.sha)?;
    validate_oid(&request.expected_head)?;
    let head = match request.branch.as_deref() {
        Some(reference) => branch_reference(&scope.root, reference)?.sha,
        None => base_commit(&scope.root)?.context("The selected checkout has no HEAD commit.")?,
    };
    ensure!(
        head == request.expected_head,
        "The selected history changed. Refresh the Git history and try again."
    );
    let view = open_comparison_view(scope, &head)?;
    let sha = request.sha.as_str();
    require_commit(&view, sha)?;
    let reachable = passive_output(&scope.root, &["merge-base", "--is-ancestor", sha, &head])?;
    ensure!(
        reachable.status.code() != Some(1),
        "This commit is outside the selected Git history."
    );
    check(reachable, "verify commit history")?;

    let output = String::from_utf8(view.read(&[
        "log",
        "-1",
        "--no-walk",
        "--no-patch",
        "--no-show-signature",
        "--no-notes",
        "--encoding=UTF-8",
        "--format=%P%x00%B%x00",
        sha,
        "--",
    ])?)?;
    let (parents, message) = output
        .split_once('\0')
        .context("Git returned an incomplete commit.")?;
    let message = message
        .strip_suffix("\0\n")
        .context("Git returned an incomplete commit message.")?;
    let parents = parents
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for parent in &parents {
        validate_oid(parent)?;
    }
    let parent = match &request.parent {
        Some(parent) => {
            ensure!(parents.contains(parent), "Select a parent of this commit.");
            Some(parent.clone())
        }
        None => parents.first().cloned(),
    };
    let base = match &parent {
        Some(parent) => parent.clone(),
        None => {
            let output = view
                .command()
                .args(["hash-object", "-t", "tree", "--stdin"])
                .stdin(Stdio::null())
                .output()?;
            let tree = String::from_utf8(check(output, "identify the empty Git tree")?)?
                .trim()
                .to_owned();
            validate_oid(&tree)?;
            tree
        }
    };
    let (files, comparison_unavailable) = if parent.is_some()
        && require_commit(&view, &base).is_err()
    {
        (Vec::new(), Some("The parent commit is unavailable in this checkout. Its file changes cannot be compared.".into()))
    } else {
        (changed_files(&view, &base, sha)?, None)
    };
    Ok((
        view,
        base,
        GitHistoryCommitDetails {
            message: message.into(),
            parents,
            parent,
            files,
            comparison_unavailable,
        },
    ))
}

fn changed_files(view: &IsolatedGit, base: &str, sha: &str) -> Result<Vec<GitHistoryCommitFile>> {
    let output = String::from_utf8(view.read(&[
        "diff",
        "--name-status",
        "-z",
        "--find-renames",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
        base,
        sha,
        "--",
    ])?)?;
    let mut fields = output.split_terminator('\0');
    let mut files = Vec::new();
    while let Some(code) = fields.next() {
        let status = code
            .chars()
            .next()
            .filter(|status| matches!(status, 'A' | 'C' | 'D' | 'M' | 'R' | 'T'))
            .context("Git returned an invalid commit file status.")?;
        let first = fields
            .next()
            .context("Git returned a commit change without a path.")?;
        let (path, previous_path) = if matches!(status, 'R' | 'C') {
            (
                fields
                    .next()
                    .context("Git returned a rename without a new path.")?,
                Some(first),
            )
        } else {
            (first, None)
        };
        validate_path(path)?;
        if let Some(previous_path) = previous_path {
            validate_path(previous_path)?;
        }
        files.push(GitHistoryCommitFile {
            path: path.into(),
            previous_path: previous_path.map(str::to_owned),
            status,
        });
    }
    Ok(files)
}
