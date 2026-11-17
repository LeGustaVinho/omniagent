use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{protection, store::Store};

use super::{
    GitBranchRange, GitBranchRevisions, GitDiffSource, GitFileSide, GitFileVersion,
    GitReviewRevision, validate_path,
};

const MAX_COMMENT_BYTES: usize = 16 * 1024;
const MAX_EXCERPT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum GitReviewCommentSource {
    Checkout {
        source: GitDiffSource,
        revision: GitReviewRevision,
    },
    Branch {
        range: GitBranchRange,
        revisions: GitBranchRevisions,
    },
    Versions {
        base: Option<GitFileVersion>,
        revised: GitFileVersion,
        revision: Option<GitReviewRevision>,
    },
}

impl GitReviewCommentSource {
    fn surface(&self) -> GitReviewCommentSurface {
        match self {
            Self::Checkout { source, .. } => match source {
                GitDiffSource::Working => GitReviewCommentSurface::Working,
                GitDiffSource::Staged => GitReviewCommentSurface::Staged,
            },
            Self::Branch { .. } => GitReviewCommentSurface::Branch,
            Self::Versions { .. } => GitReviewCommentSurface::Versions,
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReviewCommentSurface {
    Working,
    Staged,
    Branch,
    Versions,
}

impl GitReviewCommentSurface {
    fn key(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Staged => "staged",
            Self::Branch => "branch",
            Self::Versions => "versions",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewCommentSide {
    path: String,
    sha256: Option<String>,
    line_count: usize,
}

impl GitReviewCommentSide {
    fn from_file(path: &str, file: &GitFileSide) -> Self {
        let (sha256, line_count) = match file {
            GitFileSide::Text { content } => (
                Some(format!("{:x}", Sha256::digest(content.as_bytes()))),
                line_boundaries(content).count() - 1,
            ),
            _ => (None, 0),
        };
        Self {
            path: path.into(),
            sha256,
            line_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewCommentScope {
    pub snapshot: String,
    pub source: GitReviewCommentSource,
    original: GitReviewCommentSide,
    modified: GitReviewCommentSide,
}

impl GitReviewCommentScope {
    pub(super) fn from_comparison(
        checkout: &str,
        path: &str,
        source: GitReviewCommentSource,
        original: (&str, &GitFileSide),
        modified: (&str, &GitFileSide),
    ) -> Result<Option<Self>> {
        if matches!(original.1, GitFileSide::Unavailable { .. })
            || matches!(modified.1, GitFileSide::Unavailable { .. })
        {
            return Ok(None);
        }
        let original = GitReviewCommentSide::from_file(original.0, original.1);
        let modified = GitReviewCommentSide::from_file(modified.0, modified.1);
        if original.line_count == 0 && modified.line_count == 0 {
            return Ok(None);
        }
        let input = serde_json::to_vec(&(
            "git-review-comment-v1",
            checkout,
            path,
            &source,
            &original,
            &modified,
        ))?;
        let snapshot = format!("{:x}", Sha256::digest(input));
        Ok(Some(Self {
            snapshot,
            source,
            original,
            modified,
        }))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReviewSide {
    Original,
    Modified,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewCommentRequest {
    path: String,
    source: GitReviewCommentSource,
    snapshot: String,
    side: GitReviewSide,
    start_line: usize,
    end_line: usize,
    body: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentContent {
    scope: GitReviewCommentScope,
    side: GitReviewSide,
    start_line: usize,
    end_line: usize,
    excerpt: String,
    body: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewComment {
    id: String,
    path: String,
    created_at: i64,
    resolved: bool,
    #[serde(flatten)]
    content: CommentContent,
}

impl Store {
    pub async fn git_add_review_comment(
        &self,
        checkout: String,
        request: GitReviewCommentRequest,
    ) -> Result<GitReviewComment> {
        let GitReviewCommentRequest {
            path,
            source,
            snapshot,
            side,
            start_line,
            end_line,
            body,
        } = request;
        validate_path(&path)?;
        let body = body.trim().to_owned();
        ensure!(
            !body.is_empty() && body.len() <= MAX_COMMENT_BYTES && !body.contains('\0'),
            "enter a review comment of 1 to 16,384 UTF-8 bytes"
        );
        ensure!(
            start_line > 0 && end_line >= start_line,
            "select a valid review line range"
        );
        let surface = source.surface().key();
        let (scope, original, modified) = match source.clone() {
            GitReviewCommentSource::Checkout { source, .. } => {
                let diff = self
                    .git_checkout_diff(checkout.clone(), path.clone(), source)
                    .await?;
                (diff.comment_scope, diff.original, diff.modified)
            }
            GitReviewCommentSource::Branch { range, revisions } => {
                let diff = self
                    .git_branch_comparison_diff(checkout.clone(), range, revisions, path.clone())
                    .await?;
                (diff.comment_scope, diff.original, diff.modified)
            }
            GitReviewCommentSource::Versions { base, revised, .. } => {
                let diff = self
                    .git_compare_file_versions(checkout.clone(), path.clone(), base, revised)
                    .await?;
                (diff.comment_scope, diff.original, diff.modified)
            }
        };
        let scope = scope
            .filter(|scope| scope.snapshot == snapshot && scope.source == source)
            .context("the Git comparison changed; refresh it and select the lines again")?;
        let file = match side {
            GitReviewSide::Original => original,
            GitReviewSide::Modified => modified,
        };
        let GitFileSide::Text { content } = file else {
            anyhow::bail!("there are no text lines on this side of the Git comparison");
        };
        let excerpt = reviewed_lines(&content, start_line, end_line)?.to_owned();
        let content = CommentContent {
            scope,
            side,
            start_line,
            end_line,
            excerpt,
            body,
        };
        let payload = protection::protect(&serde_json::to_vec(&content)?)?;
        let id = Uuid::new_v4().to_string();
        self.execute(move |db| {
            let created_at = db.query_row(
                "INSERT INTO git_review_comments(id, checkout_id, surface, path, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5) RETURNING created_at",
                params![id, checkout, surface, path, payload],
                |row| row.get(0),
            )?;
            Ok(GitReviewComment {
                id,
                path,
                created_at,
                resolved: false,
                content,
            })
        })
        .await
    }

    pub async fn git_review_comments(
        &self,
        checkout: String,
        surface: GitReviewCommentSurface,
        path: Option<String>,
    ) -> Result<Vec<GitReviewComment>> {
        if let Some(path) = path.as_deref() {
            validate_path(path)?;
        }
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT id, path, payload, created_at, resolved FROM git_review_comments
                 WHERE checkout_id = ?1 AND surface = ?2 AND (?3 IS NULL OR path = ?3)
                 ORDER BY created_at DESC, id",
            )?;
            let mut rows = query.query(params![checkout, surface.key(), path])?;
            let mut comments = Vec::new();
            while let Some(row) = rows.next()? {
                let payload: Vec<u8> = row.get(2)?;
                comments.push(GitReviewComment {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    created_at: row.get(3)?,
                    resolved: row.get(4)?,
                    content: serde_json::from_slice(&protection::unprotect(&payload)?)?,
                });
            }
            Ok(comments)
        })
        .await
    }

    pub async fn git_set_review_comment_resolved(
        &self,
        checkout: String,
        comment: String,
        resolved: bool,
    ) -> Result<()> {
        self.execute(move |db| {
            let updated = db.execute(
                "UPDATE git_review_comments SET resolved = ?3 WHERE checkout_id = ?1 AND id = ?2",
                params![checkout, comment, resolved],
            )?;
            ensure!(
                updated == 1,
                "the review comment is unavailable in this worktree"
            );
            Ok(())
        })
        .await
    }
}

fn line_boundaries(content: &str) -> impl Iterator<Item = usize> + '_ {
    let bytes = content.as_bytes();
    std::iter::once(0)
        .chain(bytes.iter().enumerate().filter_map(|(index, byte)| {
            let line_end =
                *byte == b'\n' || (*byte == b'\r' && bytes.get(index + 1) != Some(&b'\n'));
            (line_end && index + 1 < bytes.len()).then_some(index + 1)
        }))
        .chain((!bytes.is_empty()).then_some(bytes.len()))
}

fn reviewed_lines(content: &str, start_line: usize, end_line: usize) -> Result<&str> {
    let mut boundaries = line_boundaries(content);
    let start = boundaries
        .nth(start_line - 1)
        .context("the start line is outside the reviewed text")?;
    let end = boundaries
        .nth(end_line - start_line)
        .context("the end line is outside the reviewed text")?;
    ensure!(
        end - start <= MAX_EXCERPT_BYTES,
        "select at most 32 KiB of review evidence"
    );
    Ok(&content[start..end])
}
