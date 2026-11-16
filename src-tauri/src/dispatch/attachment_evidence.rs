use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{
    attachments::{
        AttachmentDispatch, AttachmentInput, AttachmentSource, NativeAttachmentEncoding,
    },
    protection,
    store::Store,
};

use super::ContextSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "method",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AttachmentTransport {
    Native {
        encoding: NativeAttachmentEncoding,
        runner_path: Option<PathBuf>,
        resource_uri: Option<String>,
    },
    Excerpt {
        start: u64,
        end: u64,
        text: String,
    },
    Reference {
        runner_path: PathBuf,
        uri: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentEvidence {
    pub name: String,
    pub mime_type: &'static str,
    pub source: AttachmentSource,
    pub source_sha256: String,
    pub source_size: u64,
    pub message_revision_id: String,
    pub transport: AttachmentTransport,
}

impl From<&AttachmentDispatch> for AttachmentEvidence {
    fn from(attachment: &AttachmentDispatch) -> Self {
        let transport = match &attachment.input {
            AttachmentInput::Native(native) => AttachmentTransport::Native {
                encoding: native.encoding,
                runner_path: native.path.clone(),
                resource_uri: native.resource_uri.clone(),
            },
            AttachmentInput::Excerpt(excerpt) => AttachmentTransport::Excerpt {
                start: excerpt.start,
                end: excerpt.end,
                text: excerpt.text.clone(),
            },
            AttachmentInput::Reference(reference) => AttachmentTransport::Reference {
                runner_path: reference.path.clone(),
                uri: reference.uri.clone(),
            },
        };
        Self {
            name: attachment
                .metadata
                .source
                .file_name()
                .unwrap_or("Attachment")
                .into(),
            mime_type: attachment.metadata.format.mime(),
            source: attachment.metadata.source.clone(),
            source_sha256: attachment.metadata.sha256.clone(),
            source_size: attachment.metadata.size,
            message_revision_id: attachment.message_revision_id.clone(),
            transport,
        }
    }
}

impl Store {
    pub async fn attachment_dispatch_content(
        &self,
        conversation_id: String,
        run_id: String,
        index: usize,
    ) -> Result<Option<String>> {
        self.execute(move |db| {
            let protected: Option<Vec<u8>> = db
                .query_row(
                    "SELECT s.payload FROM context_snapshots s
                 JOIN agent_attempts a ON a.run_id = s.run_id
                 JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
                 WHERE s.run_id = ?1 AND a.conversation_id = ?2",
                    params![run_id, conversation_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(protected) = protected else {
                return Ok(None);
            };
            let snapshot: ContextSnapshot =
                serde_json::from_slice(&protection::unprotect(&protected)?)?;
            Ok(snapshot
                .attachments
                .into_iter()
                .nth(index)
                .and_then(|attachment| match attachment.input {
                    AttachmentInput::Native(native) => Some(native.content),
                    _ => None,
                }))
        })
        .await
    }
}
