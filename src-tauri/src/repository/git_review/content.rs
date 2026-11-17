use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::providers::configuration::{open_locked_local_file, plain_metadata};

use super::super::{documents::lock_directories, isolated_git::IsolatedGit};
use super::{GitPathStatus, GitReviewHunk, GitReviewRevision, LiveReview, hunks, validate_oid};

const MAX_DIFF_SIDE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitDiffSource {
    Working,
    Staged,
}

impl GitDiffSource {
    pub(super) fn contains(self, file: &GitPathStatus) -> bool {
        match self {
            Self::Working => file.working.is_some(),
            Self::Staged => file.staged.is_some(),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GitFileSide {
    Missing,
    Text { content: String },
    Unavailable { reason: String },
}

impl GitFileSide {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewDiff {
    pub path: String,
    pub source: GitDiffSource,
    pub revision: GitReviewRevision,
    pub original: GitFileSide,
    pub modified: GitFileSide,
    pub hunks: Vec<GitReviewHunk>,
}

pub(super) fn read_diff(
    review: &LiveReview,
    entry: &GitPathStatus,
    source: GitDiffSource,
) -> Result<GitReviewDiff> {
    let path = &entry.path;
    let index = read_index(review, path)?;
    let (original, modified) = match source {
        GitDiffSource::Working => (index, read_worktree(&review.root, path)?),
        GitDiffSource::Staged => (read_head(review, path)?, index),
    };
    let hunks = if hunks::supports(entry, source)
        && matches!(
            (&original, &modified),
            (GitFileSide::Text { .. }, GitFileSide::Text { .. })
        ) {
        hunks::read(review, path, source)?
    } else {
        Vec::new()
    };
    Ok(GitReviewDiff {
        path: path.clone(),
        source,
        revision: review.revision.clone(),
        original,
        modified,
        hunks,
    })
}

fn read_index(review: &LiveReview, path: &str) -> Result<GitFileSide> {
    let bytes = review.view.read(&[
        "--literal-pathspecs",
        "ls-files",
        "--stage",
        "-z",
        "--",
        path,
    ])?;
    let entries = String::from_utf8(bytes)?;
    if entries.is_empty() {
        return Ok(GitFileSide::Missing);
    }
    for row in entries.split_terminator('\0') {
        let (metadata, candidate) = row
            .split_once('\t')
            .context("Git returned an invalid index entry")?;
        if candidate != path {
            continue;
        }
        let parts: Vec<_> = metadata.split_ascii_whitespace().collect();
        ensure!(parts.len() == 3, "Git returned invalid index metadata");
        if parts[2] == "0" {
            return read_blob(&review.view, parts[0], parts[1]);
        }
    }
    Ok(GitFileSide::unavailable(
        "The Git index has unresolved merge stages for this file.",
    ))
}

fn read_head(review: &LiveReview, path: &str) -> Result<GitFileSide> {
    let Some(head) = review.revision.head.as_deref() else {
        return Ok(GitFileSide::Missing);
    };
    read_tree(&review.view, head, path)
}

pub(super) fn read_tree(view: &IsolatedGit, revision: &str, path: &str) -> Result<GitFileSide> {
    let bytes = view.read(&["--literal-pathspecs", "ls-tree", "-z", revision, "--", path])?;
    let entries = String::from_utf8(bytes)?;
    for row in entries.split_terminator('\0') {
        let (metadata, candidate) = row
            .split_once('\t')
            .context("Git returned an invalid tree entry")?;
        if candidate != path {
            continue;
        }
        let parts: Vec<_> = metadata.split_ascii_whitespace().collect();
        ensure!(parts.len() == 3, "Git returned invalid tree metadata");
        return read_blob(view, parts[0], parts[2]);
    }
    Ok(GitFileSide::Missing)
}

fn read_blob(view: &IsolatedGit, mode: &str, oid: &str) -> Result<GitFileSide> {
    if !matches!(mode, "100644" | "100755") {
        return Ok(GitFileSide::unavailable(
            "This Git entry is not an ordinary text file.",
        ));
    }
    validate_oid(oid)?;
    let size = String::from_utf8(view.read(&["cat-file", "-s", oid])?)?
        .trim()
        .parse::<u64>()?;
    if size > MAX_DIFF_SIDE_BYTES {
        return Ok(GitFileSide::unavailable(
            "This Git file exceeds the text preview limit.",
        ));
    }
    Ok(classify(view.read(&["cat-file", "blob", oid])?))
}

fn read_worktree(root: &Path, path: &str) -> Result<GitFileSide> {
    let path: PathBuf = root.join(path).components().collect();
    let file = match plain_metadata(&path) {
        Ok(None) => return Ok(GitFileSide::Missing),
        Ok(Some(metadata)) if metadata.is_file() => open_locked_local_file(&path),
        _ => {
            return Ok(GitFileSide::unavailable(
                "This checkout entry is not an ordinary local file.",
            ));
        }
    };
    match file {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(MAX_DIFF_SIDE_BYTES + 1).read_to_end(&mut bytes)?;
            Ok(classify(bytes))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(GitFileSide::Missing),
        Err(error) => Ok(GitFileSide::unavailable(format!(
            "The worktree file could not be opened safely: {error}"
        ))),
    }
}

fn classify(bytes: Vec<u8>) -> GitFileSide {
    if bytes.len() as u64 > MAX_DIFF_SIDE_BYTES {
        return GitFileSide::unavailable("This Git file exceeds the text preview limit.");
    }
    match String::from_utf8(bytes) {
        Ok(content) if !content.contains('\0') => GitFileSide::Text { content },
        _ => GitFileSide::unavailable("Binary file. Text comparison is unavailable."),
    }
}

pub(super) fn lock_staging_file(root: &Path, relative: &str) -> Result<Option<(Vec<File>, File)>> {
    let path: PathBuf = root.join(relative).components().collect();
    let Some(metadata) = plain_metadata(&path)? else {
        return Ok(None);
    };
    ensure!(metadata.is_file(), "select an ordinary local file to stage");
    let parents = lock_directories(path.parent().context("the staged file has no parent")?)?;
    let file = open_locked_local_file(&path)?;
    Ok(Some((parents, file)))
}
