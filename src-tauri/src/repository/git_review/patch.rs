use std::{io::Read, process::Stdio};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::providers::configuration::plain_metadata;

use super::{GitDiffSource, GitFileSide, GitPathStatus, IsolatedGit, LiveReview};

const MAX_CLIPBOARD_PATCH_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GitReviewPatch {
    Text { content: String },
    Unavailable { reason: String },
}

impl GitReviewPatch {
    pub(super) fn result(result: Result<String>) -> Self {
        match result {
            Ok(content) => Self::Text { content },
            Err(error) => Self::Unavailable {
                reason: format!("{error:#}"),
            },
        }
    }
}

pub(super) fn live(
    review: &LiveReview,
    entry: &GitPathStatus,
    source: GitDiffSource,
    modified: &GitFileSide,
) -> Result<String> {
    ensure!(
        !entry.unmerged(),
        "Review the merge conflict before copying a patch."
    );
    let path = entry.path.as_str();
    let content = match (source, entry.working) {
        (GitDiffSource::Staged, _) => read(&review.view, &["--cached", "--", path], false)?,
        (GitDiffSource::Working, Some('?')) => {
            read(&review.view, &["--no-index", "--", "/dev/null", path], true)?
        }
        (GitDiffSource::Working, _) => read(&review.view, &["--", path], false)?,
    };
    if source == GitDiffSource::Working && matches!(modified, GitFileSide::Missing) {
        ensure!(
            entry.working == Some('D')
                && content
                    .lines()
                    .nth(1)
                    .is_some_and(|line| line.starts_with("deleted file mode "))
                && plain_metadata(&review.root.join(path))?.is_none(),
            "The working file changed during review. Refresh the comparison and try again."
        );
    }
    Ok(content)
}

pub(super) fn between(view: &IsolatedGit, base: &str, head: &str, path: &str) -> GitReviewPatch {
    GitReviewPatch::result(read(view, &[base, head, "--", path], false))
}

fn read(view: &IsolatedGit, tail: &[&str], no_index: bool) -> Result<String> {
    let mut diagnostics = tempfile::tempfile()?;
    let mut child = view
        .command()
        .args([
            "--literal-pathspecs",
            "diff",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--no-indent-heuristic",
            "--inter-hunk-context=0",
            "--unified=3",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ])
        .args((!no_index).then_some("--ignore-submodules=dirty"))
        .args(tail)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(diagnostics.try_clone()?))
        .spawn()
        .context("Start Git to generate the patch")?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(MAX_CLIPBOARD_PATCH_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > MAX_CLIPBOARD_PATCH_BYTES {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read?;
    ensure!(
        bytes.len() <= MAX_CLIPBOARD_PATCH_BYTES,
        "This Git patch exceeds the 16 MiB clipboard limit."
    );
    if !status.success() && !(no_index && status.code() == Some(1)) {
        use std::io::{Seek, SeekFrom};
        diagnostics.seek(SeekFrom::Start(0))?;
        let mut message = Vec::new();
        diagnostics.take(4096).read_to_end(&mut message)?;
        anyhow::bail!(
            "Git could not generate the patch: {}",
            String::from_utf8_lossy(&message).trim()
        );
    }
    ensure!(
        !bytes.is_empty(),
        "No Git patch is available for this path in the comparison."
    );
    let content =
        String::from_utf8(bytes).context("This Git patch cannot be copied as UTF-8 text")?;
    ensure!(
        !content.contains('\0'),
        "This Git patch contains binary text that cannot be copied through the clipboard."
    );
    Ok(content)
}
