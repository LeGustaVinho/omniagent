use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    attachments::{AttachmentDelivery, AttachmentSource},
    protection,
    providers::configuration::plain_metadata,
    repository,
    store::Store,
};

use super::{load_conversation, new_id};

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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftAttachmentEdit {
    Remove,
    SetDelivery {
        delivery: Option<AttachmentDelivery>,
    },
}

impl Store {
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
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                load_conversation(&tx, &conversation_id)?.checkout_id == conversation.checkout_id,
                "The conversation checkout changed. Select the files again."
            );
            let draft = load_draft(&tx, &conversation_id)?;
            let mut files = draft.files;
            let prior_len = files.len();
            for source in sources {
                if files.iter().all(|item| item.source != source) {
                    files.push(DraftAttachment {
                        id: new_id(),
                        source,
                        delivery: None,
                    });
                }
            }
            let draft = if files.len() == prior_len {
                AttachmentDraft {
                    revision: draft.revision,
                    files,
                }
            } else {
                save_draft(&tx, &conversation_id, files)?
            };
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
