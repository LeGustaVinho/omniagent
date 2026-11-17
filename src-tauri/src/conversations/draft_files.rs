use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    attachments::{AttachmentDelivery, AttachmentSource},
    dispatch::load_saved_native_attachment,
    protection,
    providers::configuration::plain_metadata,
    repository,
    store::Store,
};

use super::{load_conversation, new_id};

mod review;
pub use review::{AttachmentDraftReview, AttachmentFileReview};
pub(crate) use review::{consume_attachment_draft, verify_attachment_draft};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAttachment {
    pub id: String,
    pub source: AttachmentSource,
    pub delivery: Option<AttachmentDelivery>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDraft {
    pub revision: Option<String>,
    pub files: Vec<DraftAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReuseSavedConversationAttachment {
    pub source_conversation_id: String,
    pub run_id: String,
    pub ordinal: usize,
    pub message_revision_id: String,
    pub source_sha256: String,
    pub destination_conversation_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftAttachmentEdit {
    Remove,
    SetDelivery {
        delivery: Option<AttachmentDelivery>,
    },
}

impl Store {
    pub async fn reuse_saved_conversation_attachment(
        &self,
        request: ReuseSavedConversationAttachment,
    ) -> Result<AttachmentDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let destination = load_conversation(&tx, &request.destination_conversation_id)?;
            let project = checkout_project(&tx, &destination.checkout_id)?;
            let saved = load_saved_native_attachment(
                &tx,
                &project,
                &request.source_conversation_id,
                &request.run_id,
                request.ordinal,
                &request.message_revision_id,
            )?;
            ensure!(
                saved.metadata.sha256 == request.source_sha256,
                "The saved attachment differs from the selected evidence."
            );
            let draft = append_sources(&tx, &destination.id, [saved.metadata.source])?;
            tx.commit()?;
            Ok(draft)
        })
        .await
    }

    pub async fn conversation_attachment_draft(
        &self,
        conversation_id: String,
    ) -> Result<AttachmentDraft> {
        self.execute(move |db| {
            load_conversation(db, &conversation_id)?;
            load_draft(db, &conversation_id)
        })
        .await
    }

    /// `paths` come from the native file dialog or a validated native drop receipt.
    pub async fn add_conversation_attachment_paths(
        &self,
        conversation_id: String,
        paths: Vec<PathBuf>,
    ) -> Result<AttachmentDraft> {
        if paths.is_empty() {
            return self.conversation_attachment_draft(conversation_id).await;
        }
        let conversation = self.conversation(conversation_id.clone()).await?;
        let checkout = repository::checkout_root(self, conversation.checkout_id.clone()).await?;
        let sources = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .map(|path| selected_source(&checkout, path))
                .collect::<Result<Vec<_>>>()
        })
        .await??;
        self.add_conversation_attachment_sources(conversation_id, conversation.checkout_id, sources)
            .await
    }

    pub async fn add_conversation_checkout_paths(
        &self,
        conversation_id: String,
        checkout_id: String,
        paths: Vec<String>,
    ) -> Result<AttachmentDraft> {
        ensure!(!paths.is_empty(), "Select at least one checkout file.");
        let conversation = self.conversation(conversation_id.clone()).await?;
        ensure!(
            conversation.checkout_id == checkout_id,
            "The conversation belongs to another checkout."
        );
        let checkout = repository::checkout_root(self, checkout_id.clone()).await?;
        let layout = repository::verify_checkout(self, checkout_id.clone(), checkout).await?;
        let sources = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .map(|relative| {
                    let path =
                        repository::checkout_items::checkout_regular_file(&layout.root, relative)?;
                    let source = selected_source(&layout.root, &path)?;
                    ensure!(
                        matches!(source, AttachmentSource::Checkout { .. }),
                        "The selected file left the checkout."
                    );
                    Ok(source)
                })
                .collect::<Result<Vec<_>>>()
        })
        .await??;
        self.add_conversation_attachment_sources(conversation_id, checkout_id, sources)
            .await
    }

    async fn add_conversation_attachment_sources(
        &self,
        conversation_id: String,
        checkout_id: String,
        sources: Vec<AttachmentSource>,
    ) -> Result<AttachmentDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                load_conversation(&tx, &conversation_id)?.checkout_id == checkout_id,
                "The conversation checkout changed. Select the files again."
            );
            let draft = append_sources(&tx, &conversation_id, sources)?;
            tx.commit()?;
            Ok(draft)
        })
        .await
    }

    pub async fn edit_conversation_attachment_draft(
        &self,
        conversation_id: String,
        expected_revision: String,
        file_id: String,
        edit: DraftAttachmentEdit,
    ) -> Result<AttachmentDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut draft = load_draft(&tx, &conversation_id)?;
            ensure!(
                draft.revision.as_deref() == Some(&expected_revision),
                "The attachment draft changed. Review it again."
            );
            let index = draft
                .files
                .iter()
                .position(|item| item.id == file_id)
                .context("The attachment is unavailable")?;
            match edit {
                DraftAttachmentEdit::Remove => {
                    draft.files.remove(index);
                }
                DraftAttachmentEdit::SetDelivery { delivery } => {
                    draft.files[index].delivery = delivery
                }
            }
            let updated = save_draft(&tx, &conversation_id, draft.files)?;
            tx.commit()?;
            Ok(updated)
        })
        .await
    }
}

fn checkout_project(db: &Connection, checkout: &str) -> Result<String> {
    Ok(db.query_row(
        "SELECT project_id FROM checkouts WHERE id = ?1",
        [checkout],
        |row| row.get(0),
    )?)
}

fn append_sources(
    db: &Connection,
    conversation: &str,
    sources: impl IntoIterator<Item = AttachmentSource>,
) -> Result<AttachmentDraft> {
    let mut draft = load_draft(db, conversation)?;
    let prior_len = draft.files.len();
    for source in sources {
        if draft.files.iter().all(|item| item.source != source) {
            draft.files.push(DraftAttachment {
                id: new_id(),
                source,
                delivery: None,
            });
        }
    }
    if draft.files.len() == prior_len {
        return Ok(draft);
    }
    save_draft(db, conversation, draft.files)
}

fn selected_source(checkout: &Path, selected: &Path) -> Result<AttachmentSource> {
    let label = selected.file_name().unwrap_or_default().to_string_lossy();
    let checked = || -> Result<AttachmentSource> {
        ensure!(
            plain_metadata(selected)?.is_some_and(|metadata| metadata.is_file()),
            "Select a regular local file"
        );
        let canonical = dunce::canonicalize(selected)?;
        ensure!(
            dunce::simplified(&canonical)
                .as_os_str()
                .eq_ignore_ascii_case(dunce::simplified(selected).as_os_str()),
            "The selected file path changed"
        );
        Ok(match canonical.strip_prefix(checkout) {
            Ok(relative_path) => AttachmentSource::Checkout {
                relative_path: relative_path.into(),
            },
            Err(_) => AttachmentSource::SelectedFile { path: canonical },
        })
    };
    checked().with_context(|| format!("Could not attach {label}"))
}

pub(super) fn load_draft(db: &Connection, conversation: &str) -> Result<AttachmentDraft> {
    let saved = db.query_row(
        "SELECT revision, payload FROM conversation_attachment_drafts WHERE conversation_id = ?1",
        [conversation], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
    ).optional()?;
    let Some((revision, payload)) = saved else {
        return Ok(AttachmentDraft::default());
    };
    Ok(AttachmentDraft {
        revision: Some(revision),
        files: serde_json::from_slice(&protection::unprotect(&payload)?)?,
    })
}

fn save_draft(
    db: &Connection,
    conversation: &str,
    files: Vec<DraftAttachment>,
) -> Result<AttachmentDraft> {
    if files.is_empty() {
        db.execute(
            "DELETE FROM conversation_attachment_drafts WHERE conversation_id = ?1",
            [conversation],
        )?;
        return Ok(AttachmentDraft::default());
    }
    let revision = new_id();
    let payload = protection::protect(&serde_json::to_vec(&files)?)?;
    db.execute(
        "INSERT INTO conversation_attachment_drafts(conversation_id, revision, payload) VALUES (?1, ?2, ?3)
         ON CONFLICT(conversation_id) DO UPDATE SET revision = excluded.revision, payload = excluded.payload, preview_id = NULL, preview_payload = NULL",
        params![conversation, revision, payload],
    )?;
    Ok(AttachmentDraft {
        revision: Some(revision),
        files,
    })
}

#[cfg(test)]
mod tests;
