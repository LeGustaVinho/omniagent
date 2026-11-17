use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    markdown_recovery::{load_payload, stored_revision, write_payload},
    providers::configuration::open_locked_local_file,
    repository::{
        checkout_items::{
            checkout_directory, checkout_regular_file, validate_name, write_in_checkout,
        },
        markdown_renames::markdown_link,
    },
    store::Store,
};

use super::{
    DocumentKind, EditorFormat, encode, hash, lock_directories, open_for_rename, read_open_file,
    relative_name, rename,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRepositoryNote {
    path: String,
    base_revision: String,
    file_identity: String,
    expected_draft_revision: Option<String>,
    from: usize,
    to: usize,
    selected_text: String,
    relative_directory: String,
    name: String,
    replace_with_link: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedRepositoryNote {
    pub path: String,
    pub replacement: Option<ExtractedSourceDraft>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedSourceDraft {
    pub markdown: String,
    pub draft_revision: Option<String>,
}

impl Store {
    pub async fn extract_repository_note_selection(
        &self,
        checkout_id: String,
        request: ExtractRepositoryNote,
    ) -> Result<ExtractedRepositoryNote> {
        let max = self
            .checkout_attachment_limits(checkout_id.clone())
            .await?
            .bytes_per_file;
        let store = self.clone();
        let runtime = tokio::runtime::Handle::current();
        write_in_checkout(self, checkout_id.clone(), move |root| {
            let root = root.to_owned();
            runtime
                .block_on(store.execute(move |db| extract(db, &root, &checkout_id, request, max)))
        })
        .await
    }
}

fn extract(
    db: &mut Connection,
    root: &Path,
    checkout: &str,
    request: ExtractRepositoryNote,
    max: u64,
) -> Result<ExtractedRepositoryNote> {
    let kind = DocumentKind::Markdown;
    let source = checkout_regular_file(root, &request.path)?;
    let _source_dirs = lock_directories(
        source
            .parent()
            .context("The source has no parent folder.")?,
    )?;
    let source_file = open_locked_local_file(&source)?;
    let observed = read_open_file(source_file.try_clone()?, max)?;
    let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let base = stored_revision(&transaction, checkout, &request.base_revision, kind)?;
    ensure!(
        base.relative_path == request.path
            && observed.identity == request.file_identity
            && hash(&observed.bytes) == base.sha256,
        "The source note changed. Review it and select the passage again."
    );

    let original = String::from_utf8(observed.bytes).context("The source note must be UTF-8.")?;
    let saved = EditorFormat::from_raw(&original);
    let current = load_payload(&transaction, checkout, &request.path)?;
    ensure!(
        current.as_ref().map(|draft| draft.revision.as_str())
            == request.expected_draft_revision.as_deref()
            && current
                .as_ref()
                .is_none_or(|draft| draft.base_revision == base.id),
        "The source draft changed. Reopen it and select the passage again."
    );
    let markdown = current
        .as_ref()
        .map_or(saved.markdown.as_str(), |draft| draft.markdown.as_str());
    ensure!(
        !markdown.contains(['\r', '\0']),
        "The source draft must use LF line endings."
    );
    let (from, to) = selection_range(markdown, &request)?;
    let selected = &markdown[from..to];

    validate_name(&request.name)?;
    let destination_folder = checkout_directory(root, &request.relative_directory)?;
    let _destination_dirs = lock_directories(&destination_folder)?;
    let destination = destination_folder.join(&request.name);
    kind.require_path(&destination)?;
    let path = relative_name(root, &destination)?;
    validate_destination_draft(&transaction, checkout, &path)?;

    let encoded = if current.is_some() {
        String::from_utf8(encode(&original, markdown))?
    } else {
        original
    };
    let raw = if saved.bom {
        encoded.strip_prefix('\u{feff}').unwrap_or(&encoded)
    } else {
        &encoded
    };
    let raw_start = utf16_offset(raw, request.from).context("Invalid selection start.")?;
    let raw_end = utf16_offset(raw, request.to).context("Invalid selection end.")?;
    let extracted = &raw.as_bytes()[raw_start..raw_end];
    ensure!(
        extracted.len() as u64 <= max,
        "The extracted note exceeds the project file limit of {max} bytes."
    );

    let staged = tempfile::Builder::new()
        .prefix(".omniagent-extract-")
        .tempdir_in(&destination_folder)?;
    let staged_path = staged.path().join("note");
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_path)?;
        file.write_all(extracted)?;
        file.sync_all()?;
    }
    let staged_file = open_for_rename(&staged_path)?;

    let mut replacement = None;
    if request.replace_with_link {
        let link = markdown_link(&request.path, &path);
        let leading_end = selected.len() - selected.trim_start_matches('\n').len();
        let trailing_start = selected.trim_end_matches('\n').len();
        let edited = format!(
            "{}{}{link}{}{}",
            &markdown[..from],
            &selected[..leading_end],
            &selected[trailing_start..],
            &markdown[to..]
        );
        let draft_revision = if edited == saved.markdown {
            transaction.execute(
                "DELETE FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1 AND document = ?2",
                params![checkout, request.path],
            )?;
            None
        } else {
            Some(write_payload(&transaction, &base, edited.clone())?.revision)
        };
        replacement = Some(ExtractedSourceDraft {
            markdown: edited,
            draft_revision,
        });
    }

    rename(&staged_file, &destination)
        .context("Create the extracted note without replacing an existing item")?;
    if let Err(error) = transaction.commit() {
        // The open handle prevents another writer from replacing or editing this exact file.
        if let Err(rollback) = rename(&staged_file, &staged_path) {
            return Err(error).with_context(|| format!(
                "The source draft was preserved but {path} could not be rolled back ({rollback})."
            ));
        }
        return Err(error)
            .context("The extraction failed. Your original selection and draft were preserved.");
    }
    // Keep the source bytes, identity, and directory names pinned until the operation commits.
    drop(source_file);
    Ok(ExtractedRepositoryNote { path, replacement })
}

fn validate_destination_draft(db: &Connection, checkout: &str, destination: &str) -> Result<()> {
    let destination = destination.to_lowercase();
    let mut query =
        db.prepare("SELECT document FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1")?;
    for path in query.query_map([checkout], |row| row.get::<_, String>(0))? {
        ensure!(
            path?.to_lowercase() != destination,
            "The destination already has a recovered draft. Choose another note name."
        );
    }
    Ok(())
}

fn selection_range(markdown: &str, request: &ExtractRepositoryNote) -> Result<(usize, usize)> {
    let from = utf16_offset(markdown, request.from).context("Invalid selection start.")?;
    let to = utf16_offset(markdown, request.to).context("Invalid selection end.")?;
    ensure!(
        from < to
            && !request.selected_text.trim().is_empty()
            && markdown[from..to] == request.selected_text,
        "The Markdown selection changed. Select the passage again."
    );
    Ok((from, to))
}

// The editor counts UTF-16 code units and treats each line separator as one LF.
fn utf16_offset(text: &str, offset: usize) -> Option<usize> {
    let mut position = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((byte, value)) = chars.next() {
        if position == offset {
            return Some(byte);
        }
        if value == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            chars.next();
        }
        position += value.len_utf16();
        if position > offset {
            return None;
        }
    }
    (position == offset).then_some(text.len())
}

#[cfg(test)]
mod tests;
