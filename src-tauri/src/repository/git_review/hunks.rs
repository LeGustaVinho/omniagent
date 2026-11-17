use std::{
    io::{Seek, Write},
    path::Path,
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    GitDiffSource, GitPathStatus, GitReviewRevision, LiveReview, ReviewScope, check, content,
    install_index, lock_directories, lock_git_file,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReviewHunk {
    pub id: String,
    pub header: String,
    pub patch: String,
    pub original_line: u32,
    pub modified_line: u32,
}

pub(super) fn supports(entry: &GitPathStatus, source: GitDiffSource) -> bool {
    match source {
        GitDiffSource::Working => entry.working == Some('M'),
        GitDiffSource::Staged => entry.staged == Some('M'),
    }
}

pub(super) fn read(
    review: &LiveReview,
    path: &str,
    source: GitDiffSource,
) -> Result<Vec<GitReviewHunk>> {
    let mut args = vec![
        "--literal-pathspecs",
        "diff",
        "--patch",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--no-indent-heuristic",
        "--inter-hunk-context=0",
        "--unified=3",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ];
    if source == GitDiffSource::Staged {
        args.push("--cached");
    }
    args.extend(["--", path]);
    let bytes = review.view.read(&args)?;
    let Ok(patch) = String::from_utf8(bytes) else {
        return Ok(Vec::new());
    };
    let Some(first) = patch.find("\n@@ ").map(|offset| offset + 1) else {
        return Ok(Vec::new());
    };
    let metadata = &patch[..first];
    let file_header = metadata
        .lines()
        .filter(|line| {
            line.starts_with("diff --git ") || line.starts_with("--- ") || line.starts_with("+++ ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body = &patch[first..];
    let mut starts = vec![0];
    starts.extend(body.match_indices("\n@@ ").map(|(offset, _)| offset + 1));
    starts.push(body.len());
    let digest = Sha256::digest(patch.as_bytes());
    starts
        .windows(2)
        .map(|pair| {
            let body = &body[pair[0]..pair[1]];
            let header = body
                .lines()
                .next()
                .context("the patch has no hunk header")?;
            let mut ranges = header.split_ascii_whitespace().skip(1);
            let mut line = |sign| -> Result<u32> {
                Ok(ranges
                    .next()
                    .and_then(|range| range.strip_prefix(sign))
                    .and_then(|range| range.split(',').next())
                    .context("invalid hunk range")?
                    .parse()?)
            };
            Ok(GitReviewHunk {
                id: format!("{digest:x}:{}", pair[0]),
                original_line: line('-')?,
                modified_line: line('+')?,
                header: header.to_owned(),
                patch: format!("{file_header}\n{body}"),
            })
        })
        .collect()
}

pub(super) fn stage(
    scope: &ReviewScope,
    path: &str,
    source: GitDiffSource,
    expected: &GitReviewRevision,
    id: &str,
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
    let entry = review
        .status()?
        .into_iter()
        .find(|entry| entry.path == path && supports(entry, source))
        .context("the selected file no longer has a textual change in this comparison")?;
    let _worktree_lock = if source == GitDiffSource::Working {
        Some(
            content::lock_staging_file(&scope.root, path)?
                .context("the selected worktree file was removed")?,
        )
    } else {
        None
    };
    let hunk = content::read_diff(&review, &entry, source)?
        .hunks
        .into_iter()
        .find(|hunk| hunk.id == id)
        .context("the selected hunk changed; review the current diff")?;
    let mut input = tempfile::tempfile()?;
    input.write_all(hunk.patch.as_bytes())?;
    input.rewind()?;
    let mut command = review.view.command();
    command.args([
        "-c",
        "core.splitIndex=false",
        "apply",
        "--cached",
        "--whitespace=nowarn",
    ]);
    if source == GitDiffSource::Staged {
        command.arg("--reverse");
    }
    let output = command.arg("-").stdin(Stdio::from(input)).output()?;
    check(output, "apply the reviewed hunk to the index")?;
    install_index(&review, &index)
}
