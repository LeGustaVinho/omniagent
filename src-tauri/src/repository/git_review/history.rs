use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{
    GitFileSide, GitReviewRevision, IsolatedGit, LiveReview, ReviewScope, base_commit,
    branch_reference,
    comments::{GitReviewCommentScope, GitReviewCommentSource},
    content, inspect, validate_oid, validate_path,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileHistory {
    pub head: Option<String>,
    pub entries: Vec<GitFileHistoryEntry>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileHistoryEntry {
    pub sha: String,
    pub parent: Option<String>,
    pub author: String,
    pub committed_at: String,
    pub subject: String,
    pub path: String,
    pub previous_path: Option<String>,
    pub status: char,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitFileVersion {
    Head,
    Index,
    Worktree,
    Commit { sha: String, path: String },
}

impl GitFileVersion {
    fn path<'a>(&'a self, fallback: &'a str) -> &'a str {
        match self {
            Self::Commit { path, .. } => path,
            _ => fallback,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileVersionComparison {
    pub revision: GitReviewRevision,
    pub original: GitFileSide,
    pub modified: GitFileSide,
    pub comment_scope: Option<GitReviewCommentScope>,
}

impl Store {
    pub async fn git_checkout_file_history(
        &self,
        checkout: String,
        path: String,
        branch: Option<String>,
        pinned_head: Option<String>,
        offset: u32,
    ) -> Result<GitFileHistory> {
        validate_path(&path)?;
        ensure!(offset <= 100_000, "the file history offset is too large");
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            file_history(&scope, &path, branch, pinned_head, offset)
        })
        .await?
    }

    pub async fn git_compare_file_versions(
        &self,
        checkout: String,
        path: String,
        base: Option<GitFileVersion>,
        revised: GitFileVersion,
    ) -> Result<GitFileVersionComparison> {
        validate_path(&path)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let (original, modified, transcoded) = match (&base, &revised) {
                (
                    Some(GitFileVersion::Commit {
                        sha,
                        path: committed_path,
                    }),
                    GitFileVersion::Worktree,
                ) => {
                    require_file_revision(&review, sha, committed_path)?;
                    content::read_commit_and_worktree_preview(&review, sha, committed_path, &path)?
                }
                _ => {
                    let original = match base.as_ref() {
                        Some(version) => read_version(&review, &path, version)?,
                        None => GitFileSide::Missing,
                    };
                    (original, read_version(&review, &path, &revised)?, false)
                }
            };
            review.verify()?;
            // Review digests attest to the original file bytes. The UTF-16 preview is read-only.
            let comment_scope = if transcoded {
                None
            } else {
                let uses_live_revision = base
                    .as_ref()
                    .is_some_and(|version| !matches!(version, GitFileVersion::Commit { .. }))
                    || !matches!(revised, GitFileVersion::Commit { .. });
                let original_path = base
                    .as_ref()
                    .map_or(path.as_str(), |version| version.path(&path));
                let modified_path = revised.path(&path);
                GitReviewCommentScope::from_comparison(
                    &scope.id,
                    &path,
                    GitReviewCommentSource::Versions {
                        base: base.clone(),
                        revised: revised.clone(),
                        revision: uses_live_revision.then(|| review.revision.clone()),
                    },
                    (original_path, &original),
                    (modified_path, &modified),
                )?
            };
            Ok(GitFileVersionComparison {
                revision: review.revision,
                original,
                modified,
                comment_scope,
            })
        })
        .await?
    }
}

fn file_history(
    scope: &ReviewScope,
    path: &str,
    branch: Option<String>,
    pinned_head: Option<String>,
    offset: u32,
) -> Result<GitFileHistory> {
    const PAGE_SIZE: usize = 100;
    let head = match (pinned_head, branch) {
        (Some(head), _) => {
            validate_oid(&head)?;
            Some(head)
        }
        (None, Some(reference)) => Some(branch_reference(&scope.root, &reference)?.sha),
        (None, None) => base_commit(&scope.root)?,
    };
    let Some(head) = head else {
        return Ok(GitFileHistory {
            head: None,
            entries: Vec::new(),
            has_more: false,
        });
    };
    let paths = inspect(&scope.root)?;
    ensure!(
        paths.git_dir == scope.git_dir && paths.git_common_dir == scope.common,
        "checkout identity changed during file history review"
    );
    let view = IsolatedGit::open(&paths, &scope.root, &head)?;
    require_commit(&view, &head)?;
    // Git skips rename detection for rows hidden by --skip. Traverse the whole prefix
    // from the pinned commit so that later pages still follow the original filename.
    let count = format!("--max-count={}", offset as usize + PAGE_SIZE + 1);
    let bytes = view.read(&[
        "--literal-pathspecs",
        "log",
        "--follow",
        "--find-renames",
        "--no-notes",
        "--no-show-signature",
        "--no-ext-diff",
        "--no-textconv",
        "--encoding=UTF-8",
        "--name-status",
        "-z",
        &count,
        "--format=%x00%H%x00%P%x00%an%x00%cI%x00%s",
        &head,
        "--",
        path,
    ])?;
    let mut entries = parse_history(&String::from_utf8(bytes)?)?
        .into_iter()
        .skip(offset as usize)
        .collect::<Vec<_>>();
    let has_more = entries.len() > PAGE_SIZE && offset < 100_000;
    entries.truncate(PAGE_SIZE);
    Ok(GitFileHistory {
        head: Some(head),
        entries,
        has_more,
    })
}

fn parse_history(output: &str) -> Result<Vec<GitFileHistoryEntry>> {
    let mut fields = output.split_terminator('\0').peekable();
    let mut entries = Vec::new();
    while let Some(delimiter) = fields.next() {
        ensure!(
            delimiter.is_empty(),
            "Git returned an invalid file history entry"
        );
        let mut next = || {
            fields
                .next()
                .context("Git returned an incomplete file history entry")
        };
        let sha = next()?;
        validate_oid(sha)?;
        let parent = next()?.split_ascii_whitespace().next();
        if let Some(parent) = parent {
            validate_oid(parent)?;
        }
        let author = next()?;
        let committed_at = next()?;
        let subject = next()?;
        let mut change = None;
        while fields.peek().is_some_and(|field| !field.is_empty()) {
            let status = fields
                .next()
                .and_then(|field| field.trim_start_matches('\n').chars().next())
                .filter(|status| matches!(status, 'A' | 'C' | 'D' | 'M' | 'R' | 'T'))
                .context("Git returned an unknown file history status")?;
            let first = fields
                .next()
                .context("Git returned a history change without a path")?;
            let (path, previous_path) = if matches!(status, 'R' | 'C') {
                let destination = fields
                    .next()
                    .context("Git returned a rename without a destination")?;
                (destination, Some(first))
            } else {
                (first, None)
            };
            validate_path(path)?;
            if let Some(previous) = previous_path {
                validate_path(previous)?;
            }
            ensure!(
                change.replace((status, path, previous_path)).is_none(),
                "Git returned multiple changes for a single file history entry"
            );
        }
        if let Some((status, path, previous_path)) = change {
            entries.push(GitFileHistoryEntry {
                sha: sha.to_owned(),
                parent: parent.map(str::to_owned),
                author: author.to_owned(),
                committed_at: committed_at.to_owned(),
                subject: subject.to_owned(),
                path: path.to_owned(),
                previous_path: previous_path.map(str::to_owned),
                status,
            });
        }
    }
    Ok(entries)
}

fn read_version(review: &LiveReview, path: &str, version: &GitFileVersion) -> Result<GitFileSide> {
    match version {
        GitFileVersion::Head => content::read_head(review, path),
        GitFileVersion::Index => content::read_index(review, path),
        GitFileVersion::Worktree => content::read_worktree(&review.root, path),
        GitFileVersion::Commit { sha, path } => {
            require_file_revision(review, sha, path)?;
            content::read_tree(&review.view, sha, path)
        }
    }
}

fn require_file_revision(review: &LiveReview, sha: &str, path: &str) -> Result<()> {
    validate_oid(sha)?;
    validate_path(path)?;
    require_commit(&review.view, sha)
}

pub(super) fn require_commit(view: &IsolatedGit, sha: &str) -> Result<()> {
    let kind = view.read(&["cat-file", "-t", sha])?;
    ensure!(
        String::from_utf8_lossy(&kind).trim() == "commit",
        "select a Git commit"
    );
    Ok(())
}
