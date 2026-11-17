use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Seek, Write},
    os::windows::fs::MetadataExt,
    path::Path,
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{providers::configuration::plain_metadata, store::Store};

use super::{
    GitDiffSource, GitFileSide, GitHistoryCommitDiff, GitReviewRevision, LiveReview, ReviewScope,
    check, content,
    history::require_commit,
    patch::{self, GitReviewPatch},
    validate_oid, validate_path,
};

#[derive(Debug, Clone, Deserialize)]
pub struct GitCommitCheckoutRequest {
    pub base: String,
    pub source: GitDiffSource,
}

#[derive(Debug, Serialize)]
pub struct GitCommitCheckoutComparison {
    pub revision: GitReviewRevision,
    pub files: Vec<GitCommitCheckoutFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitCheckoutFile {
    pub path: String,
    pub status: char,
    pub untracked: bool,
    pub evidence: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitCommitCheckoutFileRequest {
    pub comparison: GitCommitCheckoutRequest,
    pub revision: GitReviewRevision,
    pub file: GitCommitCheckoutFile,
}

impl Store {
    pub async fn git_compare_commit_checkout(
        &self,
        checkout: String,
        request: GitCommitCheckoutRequest,
    ) -> Result<GitCommitCheckoutComparison> {
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let mut review = open(&scope, &request)?;
            let untracked = include_untracked(&mut review, &request, None)?;
            let files = changed_paths(&review, &request, None)?
                .into_iter()
                .map(|(path, status)| {
                    let evidence = if request.source == GitDiffSource::Working {
                        WorkingFile::capture(&review.root, &path, status)?
                            .evidence()
                            .map(str::to_owned)
                    } else {
                        None
                    };
                    Ok(GitCommitCheckoutFile {
                        status,
                        untracked: untracked.contains(&path),
                        path,
                        evidence,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            review.verify()?;
            Ok(GitCommitCheckoutComparison {
                revision: review.revision,
                files,
            })
        })
        .await?
    }

    pub async fn git_compare_commit_checkout_file(
        &self,
        checkout: String,
        request: GitCommitCheckoutFileRequest,
    ) -> Result<GitHistoryCommitDiff> {
        validate_path(&request.file.path)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || compared_file(&scope, request)).await?
    }
}

fn open(scope: &ReviewScope, request: &GitCommitCheckoutRequest) -> Result<LiveReview> {
    validate_oid(&request.base)?;
    let review = LiveReview::open(scope)?;
    require_commit(&review.view, &request.base)?;
    Ok(review)
}

fn include_untracked(
    review: &mut LiveReview,
    request: &GitCommitCheckoutRequest,
    path: Option<&str>,
) -> Result<BTreeSet<String>> {
    if request.source == GitDiffSource::Staged {
        return Ok(BTreeSet::new());
    }
    let mut args = vec![
        "--literal-pathspecs",
        "ls-files",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
    ];
    args.extend(path);
    let rows = String::from_utf8(review.view.read(&args)?)?;
    let paths = rows
        .split_terminator('\0')
        .filter(|candidate| path.is_none_or(|path| path == *candidate))
        .filter(|candidate| validate_path(candidate).is_ok())
        .filter(|candidate| {
            plain_metadata(&review.root.join(candidate))
                .is_ok_and(|metadata| metadata.is_some_and(|item| item.is_file()))
        })
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if !paths.is_empty() {
        // Intent-to-add lets Git compare untracked bytes with the historical tree, including
        // a path that existed in the commit and was later removed from the real index.
        review.view.keep_object_writes_in_scratch()?;
        let mut input = tempfile::tempfile()?;
        for path in &paths {
            input.write_all(path.as_bytes())?;
            input.write_all(b"\0")?;
        }
        input.rewind()?;
        let output = review
            .view
            .command()
            .args([
                "--literal-pathspecs",
                "add",
                "--intent-to-add",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ])
            .stdin(Stdio::from(input))
            .output()?;
        check(output, "prepare untracked paths for comparison")?;
    }
    Ok(paths)
}

fn changed_paths(
    review: &LiveReview,
    request: &GitCommitCheckoutRequest,
    path: Option<&str>,
) -> Result<BTreeMap<String, char>> {
    let mut args = vec![
        "--literal-pathspecs",
        "diff",
        "--name-status",
        "-z",
        "--no-renames",
        "--ita-invisible-in-index",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
    ];
    args.extend((request.source == GitDiffSource::Staged).then_some("--cached"));
    args.extend([request.base.as_str(), "--"]);
    args.extend(path);
    let rows = String::from_utf8(review.view.read(&args)?)?;
    let mut fields = rows.split_terminator('\0');
    let mut files = BTreeMap::new();
    while let Some(code) = fields.next() {
        let status = code
            .chars()
            .next()
            .filter(|status| matches!(status, 'A' | 'D' | 'M' | 'T' | 'U'))
            .context("Git returned an invalid checkout change status.")?;
        let path = fields
            .next()
            .context("Git returned a checkout change without a path.")?;
        validate_path(path)?;
        let prior = files.entry(path.into()).or_insert(status);
        if status == 'U' {
            *prior = status;
        }
    }
    Ok(files)
}

fn compared_file(
    scope: &ReviewScope,
    request: GitCommitCheckoutFileRequest,
) -> Result<GitHistoryCommitDiff> {
    let GitCommitCheckoutFileRequest {
        comparison,
        revision,
        file,
    } = request;
    let GitCommitCheckoutFile {
        path,
        status,
        untracked,
        evidence,
    } = file;
    let mut review = open(scope, &comparison)?;
    ensure!(
        review.revision == revision,
        "The checkout index or HEAD changed. Refresh the comparison."
    );
    let working = (comparison.source == GitDiffSource::Working)
        .then(|| WorkingFile::capture(&review.root, &path, status))
        .transpose()?;
    ensure!(
        working.as_ref().and_then(WorkingFile::evidence) == evidence.as_deref(),
        "The working file changed. Refresh the comparison."
    );
    let prepared = include_untracked(&mut review, &comparison, Some(&path))?;
    let files = changed_paths(&review, &comparison, Some(&path))?;
    ensure!(
        prepared.contains(&path) == untracked && files.get(&path) == Some(&status),
        "The selected change no longer matches. Refresh the comparison."
    );
    let original = content::read_tree(&review.view, &comparison.base, &path)?;
    let modified = match &working {
        Some(working) => working.read()?,
        None => content::read_index(&review, &path)?,
    };
    let patch = if working
        .as_ref()
        .is_some_and(|file| file.evidence().is_none())
        || status == 'U'
        || files.len() != 1
    {
        GitReviewPatch::Unavailable {
            reason: "A Git patch for this one path could not be isolated safely.".into(),
        }
    } else {
        patch::checkout_file(&review.view, &comparison.base, comparison.source, &path)
    };
    if matches!(working, Some(WorkingFile::Missing))
        && let GitReviewPatch::Text { content } = &patch
    {
        ensure!(
            content
                .lines()
                .nth(1)
                .is_some_and(|line| line.starts_with("deleted file mode ")),
            "The working file changed. Refresh the comparison."
        );
    }
    review.verify()?;
    Ok(GitHistoryCommitDiff {
        path,
        original,
        modified,
        patch,
    })
}

enum WorkingFile {
    Missing,
    Locked {
        _parents: Vec<File>,
        file: File,
        evidence: String,
    },
    Unavailable,
}

impl WorkingFile {
    fn capture(root: &Path, path: &str, status: char) -> Result<Self> {
        // An ignored file can occupy a path deleted from the index. It is absent
        // from this Git comparison and its working bytes must not be exposed.
        if status == 'D' {
            return Ok(Self::Missing);
        }
        match content::lock_staging_file(root, path) {
            Ok(Some((parents, mut file))) => {
                let mut digest = Sha256::new();
                digest.update(file.metadata()?.file_attributes().to_le_bytes());
                io::copy(&mut file, &mut digest)?;
                file.rewind()?;
                Ok(Self::Locked {
                    _parents: parents,
                    file,
                    evidence: format!("{:x}", digest.finalize()),
                })
            }
            Ok(None) => Ok(Self::Missing),
            Err(_) => Ok(Self::Unavailable),
        }
    }

    fn evidence(&self) -> Option<&str> {
        match self {
            Self::Locked { evidence, .. } => Some(evidence),
            Self::Missing => Some("missing"),
            Self::Unavailable => None,
        }
    }

    fn read(&self) -> Result<GitFileSide> {
        Ok(match self {
            Self::Locked { file, .. } => content::read_locked_worktree(file)?,
            Self::Missing => GitFileSide::Missing,
            Self::Unavailable => GitFileSide::Unavailable {
                reason: "The working path could not be opened as an ordinary local file.".into(),
            },
        })
    }
}
