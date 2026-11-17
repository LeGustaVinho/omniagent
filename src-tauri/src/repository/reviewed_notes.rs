use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::TransactionBehavior;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    markdown_recovery::{RepositoryMarkdownDraft, load_payload, stored_revision, write_payload},
    providers::configuration::open_locked_local_file,
    store::Store,
};

use super::{
    RepositoryMarkdownRevision,
    checkout_items::checkout_regular_file,
    checkout_root,
    documents::{DocumentKind, RepositoryDocument, file_identity, lock_directories},
    verify_checkout,
};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReviewedNoteRevision {
    pub(super) path: String,
    pub(super) revision_id: String,
}

pub(super) struct ReviewedNoteEdit {
    pub(super) revision: RepositoryMarkdownRevision,
    pub(super) file_identity: String,
    pub(super) markdown: String,
}

impl ReviewedNoteEdit {
    pub(super) fn new(document: &RepositoryDocument, markdown: String) -> Self {
        Self {
            revision: document.revision.clone(),
            file_identity: document.file_identity.clone(),
            markdown,
        }
    }

    pub(super) fn reviewed_revision(&self) -> ReviewedNoteRevision {
        ReviewedNoteRevision {
            path: self.revision.relative_path.clone(),
            revision_id: self.revision.id.clone(),
        }
    }
}

impl Store {
    pub(super) async fn stage_reviewed_note_edits(
        &self,
        checkout: String,
        expected_root: PathBuf,
        edits: Vec<ReviewedNoteEdit>,
    ) -> Result<Vec<RepositoryMarkdownDraft>> {
        let root = checkout_root(self, checkout.clone()).await?;
        let root = verify_checkout(self, checkout.clone(), root).await?.root;
        ensure!(
            root == expected_root,
            "The checkout moved after the preview."
        );
        let max = self
            .checkout_attachment_limits(checkout.clone())
            .await?
            .bytes_per_file;
        let (edits, pinned) = tokio::task::spawn_blocking(move || {
            let pinned = pin_revisions(&root, &edits, max)?;
            Ok::<_, anyhow::Error>((edits, pinned))
        })
        .await??;
        self.execute(move |db| {
            let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for edit in &edits {
                let path = &edit.revision.relative_path;
                ensure!(
                    stored_revision(
                        &transaction,
                        &checkout,
                        &edit.revision.id,
                        DocumentKind::Markdown
                    )? == edit.revision,
                    "{path} changed after the preview. Review the changes again."
                );
                ensure!(
                    load_payload(&transaction, &checkout, path)?.is_none(),
                    "{path} has a draft. Save or discard it and review the changes again."
                );
            }
            let mut drafts = Vec::with_capacity(edits.len());
            for edit in edits {
                let payload = write_payload(&transaction, &edit.revision, edit.markdown)?;
                drafts.push(RepositoryMarkdownDraft {
                    base: edit.revision,
                    revision: payload.revision,
                    markdown: payload.markdown,
                });
            }
            transaction.commit()?;
            drop(pinned);
            Ok(drafts)
        })
        .await
    }
}

fn pin_revisions(root: &Path, edits: &[ReviewedNoteEdit], max: u64) -> Result<Vec<File>> {
    let mut pins = Vec::new();
    for edit in edits {
        let path = &edit.revision.relative_path;
        let file = checkout_regular_file(root, path)?;
        pins.extend(lock_directories(
            file.parent().context("The note has no parent folder.")?,
        )?);
        let handle = open_locked_local_file(&file)?;
        let mut bytes = Vec::new();
        (&handle)
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= max
                && file_identity(&handle)? == edit.file_identity
                && format!("{:x}", Sha256::digest(&bytes)) == edit.revision.sha256,
            "{path} changed after the preview. Review the changes again."
        );
        let line_breaks = edit.markdown.bytes().filter(|byte| *byte == b'\n').count();
        let extra = if bytes.contains(&b'\r') {
            line_breaks
        } else {
            0
        };
        let bom = if bytes.starts_with("\u{feff}".as_bytes()) {
            3
        } else {
            0
        };
        ensure!(
            (edit.markdown.len() + extra + bom) as u64 <= max,
            "The draft for {path} exceeds the project file limit."
        );
        pins.push(handle);
    }
    Ok(pins)
}
