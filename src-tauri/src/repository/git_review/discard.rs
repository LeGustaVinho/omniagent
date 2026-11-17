use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

use crate::{providers::configuration::open_locked_local_file, store::Store};

use super::{
    GitDiffSource, GitReviewHunk, LiveReview, ReviewScope, check, hunks, lock_directories,
    lock_git_file, validate_oid, validate_path, with_managed_worktree_writer,
};
use crate::repository::{
    checkout_items::{checkout_directory, validate_name},
    documents::replace_reviewed_checkout_bytes,
};

mod batch;
mod untracked;
pub use batch::GitDiscardBatchPreview;
use untracked::ReviewedRecycle;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum PreviewFile {
    Missing,
    Text {
        content: String,
    },
    Bytes {
        size: u64,
        sha256: String,
        reason: &'static str,
    },
}

impl PreviewFile {
    fn from_bytes(bytes: Option<&[u8]>) -> Self {
        let Some(bytes) = bytes else {
            return Self::Missing;
        };
        let reason = if bytes.len() <= MAX_PREVIEW_BYTES {
            if let Ok(text) = std::str::from_utf8(bytes)
                && !text.contains('\0')
            {
                return Self::Text {
                    content: text.to_owned(),
                };
            }
            "The file is not UTF-8 text."
        } else {
            "The file exceeds the text preview limit."
        };
        Self::Bytes {
            size: bytes.len() as u64,
            sha256: digest(bytes),
            reason,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiscardPreview {
    path: String,
    snapshot: String,
    hunk: Option<String>,
    before: PreviewFile,
    after: PreviewFile,
}

impl Store {
    pub async fn git_discard_preview(
        &self,
        checkout: String,
        path: String,
        hunk_id: Option<String>,
    ) -> Result<GitDiscardPreview> {
        self.review_discard(checkout, path, DiscardSelection::Tracked(hunk_id))
            .await
    }

    pub async fn git_recycle_untracked_preview(
        &self,
        checkout: String,
        path: String,
    ) -> Result<GitDiscardPreview> {
        self.review_discard(checkout, path, DiscardSelection::Untracked)
            .await
    }

    async fn review_discard(
        &self,
        checkout: String,
        path: String,
        selection: DiscardSelection,
    ) -> Result<GitDiscardPreview> {
        verify_discard_path(&path)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let prepared = selection.prepare(&scope, &review, &path)?;
            review.verify()?;
            Ok(prepared.into_preview(path))
        })
        .await?
    }

    pub async fn git_discard_changes(
        &self,
        checkout: String,
        path: String,
        hunk_id: Option<String>,
        snapshot: String,
    ) -> Result<()> {
        self.apply_discard(checkout, path, DiscardSelection::Tracked(hunk_id), snapshot)
            .await
    }

    pub async fn git_recycle_untracked(
        &self,
        checkout: String,
        path: String,
        snapshot: String,
    ) -> Result<()> {
        self.apply_discard(checkout, path, DiscardSelection::Untracked, snapshot)
            .await
    }

    async fn apply_discard(
        &self,
        checkout: String,
        path: String,
        selection: DiscardSelection,
        snapshot: String,
    ) -> Result<()> {
        verify_discard_path(&path)?;
        ensure!(
            snapshot.len() == 64 && snapshot.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "review the discard before applying it"
        );
        let scope = self.git_review_scope(checkout.clone()).await?;
        let (project, owner) = (scope.project.clone(), scope.owner.clone());
        let store = self.clone();
        let runtime = Handle::current();
        let write = move || {
            let review = LiveReview::open(&scope)?;
            let git_dir = Path::new(&review.paths.git_dir);
            let _directories = lock_directories(git_dir)?;
            let _index_lock =
                lock_git_file(&git_dir.join("index.lock")).context("lock the worktree index")?;
            review.verify()?;
            let prepared = selection.prepare(&scope, &review, &path)?;
            ensure!(
                prepared.snapshot() == snapshot,
                "the reviewed Git changes no longer match; review them again"
            );
            runtime.block_on(store.execute(move |db| {
                let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let protected: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1 AND document = ?2 COLLATE NOCASE)",
                    params![scope.id, path], |row| row.get(0),
                )?;
                ensure!(!protected, "save or discard the protected editor draft before discarding changes from disk");
                prepared.apply(&scope.root, &path, &review)
            }))
        };
        match owner {
            Some(owner) => {
                with_managed_worktree_writer(self, project, checkout, owner, move |_| write()).await
            }
            None => tokio::task::spawn_blocking(write).await?,
        }
    }
}

enum DiscardSelection {
    Tracked(Option<String>),
    Untracked,
}

enum ReviewedDiscard {
    Tracked(PreparedDiscard),
    Untracked(ReviewedRecycle),
}

impl DiscardSelection {
    fn prepare(
        self,
        scope: &ReviewScope,
        review: &LiveReview,
        path: &str,
    ) -> Result<ReviewedDiscard> {
        match self {
            Self::Tracked(hunk_id) => {
                prepare(scope, review, path, hunk_id.as_deref()).map(ReviewedDiscard::Tracked)
            }
            Self::Untracked => {
                ReviewedRecycle::prepare(scope, review, path).map(ReviewedDiscard::Untracked)
            }
        }
    }
}

impl ReviewedDiscard {
    fn snapshot(&self) -> &str {
        match self {
            Self::Tracked(review) => &review.snapshot,
            Self::Untracked(review) => &review.snapshot,
        }
    }

    fn into_preview(self, path: String) -> GitDiscardPreview {
        match self {
            Self::Tracked(review) => GitDiscardPreview {
                path,
                snapshot: review.snapshot,
                hunk: review.hunk,
                before: PreviewFile::from_bytes(review.before.as_deref()),
                after: PreviewFile::from_bytes(Some(&review.after)),
            },
            Self::Untracked(review) => GitDiscardPreview {
                path,
                snapshot: review.snapshot,
                hunk: None,
                before: review.preview,
                after: PreviewFile::Missing,
            },
        }
    }

    fn apply(self, root: &Path, relative: &str, review: &LiveReview) -> Result<()> {
        match self {
            Self::Tracked(prepared) => prepared.apply(root, relative, review),
            Self::Untracked(prepared) => prepared.apply(relative, review),
        }
    }
}

struct PreparedDiscard {
    destination: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
    snapshot: String,
    hunk: Option<String>,
}

impl PreparedDiscard {
    fn apply(self, root: &Path, relative: &str, review: &LiveReview) -> Result<()> {
        let (folder, _) = relative.rsplit_once('/').unwrap_or(("", relative));
        let verify = || {
            checkout_directory(root, folder)?;
            review.verify()
        };
        if let Some(before) = self.before {
            return replace_reviewed_checkout_bytes(
                root,
                &self.destination,
                &digest(&before),
                &self.after,
                MAX_FILE_BYTES,
                verify,
            );
        }
        let parent = self
            .destination
            .parent()
            .context("the checkout file has no parent")?;
        let _parents = lock_directories(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(&self.after)?;
        staged.as_file().sync_all()?;
        verify()?;
        staged
            .persist_noclobber(&self.destination)
            .context("restore the deleted file without replacing an item created since review")?;
        Ok(())
    }
}

fn verify_discard_path(path: &str) -> Result<()> {
    validate_path(path)?;
    path.split('/').try_for_each(validate_name)
}

fn prepare(
    scope: &ReviewScope,
    review: &LiveReview,
    path: &str,
    hunk_id: Option<&str>,
) -> Result<PreparedDiscard> {
    let status = review
        .status()?
        .into_iter()
        .find(|entry| {
            entry.path == path && matches!(entry.working, Some('M' | 'D')) && !entry.unmerged()
        })
        .context("select an unstaged modification or deletion of a tracked file")?;
    let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
    let parent = checkout_directory(&scope.root, directory)?;
    let _parents = lock_directories(&parent)?;
    let destination = parent.join(name);
    let before = match open_locked_local_file(&destination) {
        Ok(file) => Some(read_bytes(file)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    ensure!(
        before.is_some() == (status.working == Some('M')),
        "the working file changed during discard review"
    );
    let indexed = read_checkout_bytes(review, path)?;
    let (after, hunk) = if let Some(id) = hunk_id {
        let before = before
            .as_deref()
            .context("hunk discard requires an existing file")?;
        let hunk = hunks::read(review, path, GitDiffSource::Working)?
            .into_iter()
            .find(|hunk| hunk.id == id)
            .context("the selected hunk changed; select it again in the current diff")?;
        (reverse_hunk(before, &indexed, &hunk)?, Some(hunk.header))
    } else {
        (indexed, None)
    };
    ensure!(
        before.as_deref() != Some(after.as_slice()),
        "the selected Git changes are already absent"
    );
    let snapshot = digest(&serde_json::to_vec(&(
        &scope.id,
        path,
        &review.revision,
        hunk_id,
        before.as_deref().map(digest),
        digest(&after),
    ))?);
    Ok(PreparedDiscard {
        destination,
        before,
        after,
        snapshot,
        hunk,
    })
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_bytes(file: File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "the file exceeds the 8 MiB discard review limit"
    );
    Ok(bytes)
}

fn read_checkout_bytes(review: &LiveReview, path: &str) -> Result<Vec<u8>> {
    let rows = String::from_utf8(review.view.read(&[
        "--literal-pathspecs",
        "ls-files",
        "--stage",
        "-z",
        "--",
        path,
    ])?)?;
    let entry = rows
        .split_terminator('\0')
        .filter_map(|row| row.split_once('\t'))
        .find(|(_, name)| *name == path)
        .context("the file is not in the Git index")?;
    let fields = entry.0.split_ascii_whitespace().collect::<Vec<_>>();
    let ["100644" | "100755", oid, "0"] = fields[..] else {
        anyhow::bail!("select a regular file in the Git index");
    };
    validate_oid(oid)?;
    let attribute = String::from_utf8(review.view.read(&[
        "--literal-pathspecs",
        "check-attr",
        "-z",
        "filter",
        "--",
        path,
    ])?)?;
    ensure!(
        matches!(
            attribute.split_terminator('\0').nth(2),
            Some("unspecified" | "unset")
        ),
        "this file uses a custom Git filter; restore it in the Git client configured for that filter"
    );
    let size = String::from_utf8(review.view.read(&["cat-file", "-s", oid])?)?
        .trim()
        .parse::<u64>()?;
    ensure!(
        size <= MAX_FILE_BYTES,
        "the index file exceeds the 8 MiB discard review limit"
    );
    let output = tempfile::tempfile()?;
    let converted = review
        .view
        .command()
        .args(["cat-file", "--filters", &format!("--path={path}"), oid])
        .stdout(Stdio::from(output.try_clone()?))
        .output()?;
    check(converted, "render the tracked file's checkout bytes")?;
    let mut output = output;
    io::Seek::rewind(&mut output)?;
    read_bytes(output)
}

fn reverse_hunk(working: &[u8], indexed: &[u8], hunk: &GitReviewHunk) -> Result<Vec<u8>> {
    let header_offset = hunk
        .patch
        .find("\n@@ ")
        .context("the Git hunk has no header")?;
    let (header, body) = hunk.patch[header_offset + 1..]
        .split_once('\n')
        .context("the Git hunk has no body")?;
    let mut ranges = header.split_ascii_whitespace().skip(1);
    let base = line_span(ranges.next(), '-')?;
    let edited = line_span(ranges.next(), '+')?;
    let working_lines = working
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let base_lines = indexed
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let mut changed = working_lines
        .get(edited.clone())
        .context("the hunk's working lines no longer exist")?
        .iter();
    let mut original = base_lines
        .get(base)
        .context("the hunk's index lines no longer exist")?
        .iter();
    let mut lines = body.split_terminator('\n').peekable();
    let mut reversed = Vec::new();
    while let Some(line) = lines.next() {
        let (&kind, text) = line
            .as_bytes()
            .split_first()
            .context("empty Git hunk line")?;
        let terminated = lines.peek() != Some(&"\\ No newline at end of file");
        if !terminated {
            lines.next();
        }
        let indexed = if matches!(kind, b' ' | b'-') {
            Some(matching_line(original.next().copied(), text, terminated)?)
        } else {
            None
        };
        let existing = if matches!(kind, b' ' | b'+') {
            Some(matching_line(changed.next().copied(), text, terminated)?)
        } else {
            None
        };
        match (kind, indexed, existing) {
            (b' ', _, Some(line)) | (b'-', Some(line), _) => reversed.extend_from_slice(line),
            (b'+', _, Some(_)) => (),
            _ => anyhow::bail!("unexpected Git hunk line"),
        }
    }
    ensure!(
        original.next().is_none() && changed.next().is_none(),
        "the Git hunk line counts do not match the reviewed files"
    );
    let mut output = working_lines[..edited.start].concat();
    output.extend(reversed);
    output.extend(
        working_lines[edited.end..]
            .iter()
            .flat_map(|line| line.iter().copied()),
    );
    ensure!(
        output.len() as u64 <= MAX_FILE_BYTES,
        "the restored hunk exceeds the discard review limit"
    );
    Ok(output)
}

fn matching_line<'a>(actual: Option<&'a [u8]>, text: &[u8], terminated: bool) -> Result<&'a [u8]> {
    let actual = actual.context("the Git hunk has too many lines")?;
    let without_lf = actual.strip_suffix(b"\n");
    let body = without_lf.unwrap_or(actual);
    ensure!(
        without_lf.is_some() == terminated
            && (body == text
                || terminated
                    && (body.strip_suffix(b"\r") == Some(text)
                        || text.strip_suffix(b"\r") == Some(body))),
        "the Git hunk cannot be mapped to the exact checkout bytes"
    );
    Ok(actual)
}

fn line_span(range: Option<&str>, sign: char) -> Result<std::ops::Range<usize>> {
    let range = range
        .and_then(|value| value.strip_prefix(sign))
        .context("invalid Git hunk range")?;
    let (first, count) = range.split_once(',').unwrap_or((range, "1"));
    let count = count.parse::<usize>()?;
    let first = first
        .parse::<usize>()?
        .checked_sub(usize::from(count > 0))
        .context("invalid Git hunk start")?;
    let end = first.checked_add(count).context("invalid Git hunk count")?;
    Ok(first..end)
}

#[cfg(test)]
mod tests;
