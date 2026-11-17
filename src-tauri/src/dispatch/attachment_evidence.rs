use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    attachments::{
        AttachmentDispatch, AttachmentInput, AttachmentMetadata, AttachmentSource,
        NativeAttachmentEncoding, SavedNativeAttachment,
    },
    protection,
    providers::{ProviderEvent, ProviderEventData},
    store::Store,
};

use super::{
    ContextSnapshot, Delivery,
    attachment_retention::{
        archived_attachment_was_removed, dispatch_removed_ordinals, present_ordinals,
    },
    inventory::{belongs_to_dispatch, resolve_reported_path, same_path},
};

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
    pub ordinal: usize,
    pub name: String,
    pub mime_type: &'static str,
    pub source: AttachmentSource,
    pub source_sha256: String,
    pub source_size: u64,
    pub message_revision_id: String,
    pub transport: AttachmentTransport,
    pub reported_reads: Vec<AttachmentRead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRead {
    pub event_sequence: String,
    pub path: String,
    pub tool: String,
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
            ordinal: 0,
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
            reported_reads: Vec::new(),
        }
    }
}

pub(super) fn load(
    db: &Connection,
    run_id: &str,
    dispatch_id: &str,
    snapshot: &ContextSnapshot,
    delivery: Delivery,
    removed_ordinals: &BTreeSet<usize>,
) -> Result<Vec<AttachmentEvidence>> {
    let mut evidence: Vec<_> = present_ordinals(removed_ordinals)
        .zip(&snapshot.attachments)
        .map(|(ordinal, attachment)| AttachmentEvidence {
            ordinal,
            ..AttachmentEvidence::from(attachment)
        })
        .collect();
    if delivery != Delivery::Submitted
        || !snapshot
            .attachments
            .iter()
            .any(|attachment| attachment.input.path().is_some())
    {
        return Ok(evidence);
    }
    let (session, turn): (Option<String>, Option<String>) = db.query_row(
        "SELECT s.provider_session_id, a.provider_turn_id
         FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE a.run_id = ?1",
        [run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut query = db.prepare(
        "SELECT seq, source, payload FROM agent_events
         WHERE run_id = ?1 AND source IN ('application', 'provider') ORDER BY seq",
    )?;
    let mut rows = query.query([run_id])?;
    let mut submitted = false;
    while let Some(row) = rows.next()? {
        let application_event = row.get::<_, String>(1)? == "application";
        if application_event == submitted {
            continue;
        }
        let payload: Vec<u8> = row.get(2)?;
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        if application_event {
            submitted = matches!(event.data, ProviderEventData::DispatchSubmitted { dispatch_id: id } if id == dispatch_id);
            continue;
        }
        if !belongs_to_dispatch(&event.location, session.as_deref(), turn.as_deref()) {
            continue;
        }
        let ProviderEventData::ContextRead { path, tool } = event.data else {
            continue;
        };
        let Some(resolved) =
            resolve_reported_path(Path::new(&path), Some(snapshot.working_directory()))
        else {
            continue;
        };
        let sequence: i64 = row.get(0)?;
        for (attachment, item) in snapshot.attachments.iter().zip(&mut evidence) {
            if attachment
                .input
                .path()
                .is_some_and(|target| same_path(target, &resolved))
            {
                item.reported_reads.push(AttachmentRead {
                    event_sequence: sequence.to_string(),
                    path: path.clone(),
                    tool: tool.clone(),
                });
            }
        }
    }
    Ok(evidence)
}

fn archived_attachment(
    db: &Connection,
    conversation_id: &str,
    run_id: &str,
    index: usize,
    project_id: Option<&str>,
) -> Result<Option<AttachmentDispatch>> {
    let record: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT d.id, s.payload FROM context_snapshots s
             JOIN runs r ON r.id = s.run_id
             JOIN checkouts c ON c.id = r.checkout_id
             JOIN agent_attempts a ON a.run_id = s.run_id
             JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
             WHERE s.run_id = ?1 AND a.conversation_id = ?2
               AND (?3 IS NULL OR c.project_id = ?3)",
            params![run_id, conversation_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((dispatch_id, protected)) = record else {
        return Ok(None);
    };
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&protected)?)?;
    let removed = dispatch_removed_ordinals(db, &dispatch_id)?;
    Ok(present_ordinals(&removed)
        .zip(snapshot.attachments)
        .find_map(|(ordinal, attachment)| (ordinal == index).then_some(attachment)))
}

pub(super) fn require_archived_native(
    db: &Connection,
    project_id: &str,
    conversation_id: &str,
    run_id: &str,
    ordinal: usize,
    message_revision_id: &str,
) -> Result<AttachmentDispatch> {
    let attachment = archived_attachment(db, conversation_id, run_id, ordinal, Some(project_id))?
        .context("The saved attachment is unavailable in this project.")?;
    ensure!(
        attachment.message_revision_id == message_revision_id
            && attachment.input.native().is_some_and(|native| {
                native.format == attachment.metadata.format
                    && native.encoding.supports(native.format)
            })
            && !archived_attachment_was_removed(db, &attachment)?,
        "The saved native attachment changed or was removed."
    );
    Ok(attachment)
}

pub(crate) fn load_saved_native_attachment(
    db: &Connection,
    project_id: &str,
    conversation_id: &str,
    run_id: &str,
    ordinal: usize,
    message_revision_id: &str,
) -> Result<SavedNativeAttachment> {
    let attachment = require_archived_native(
        db,
        project_id,
        conversation_id,
        run_id,
        ordinal,
        message_revision_id,
    )?;
    let AttachmentDispatch {
        mut metadata,
        input,
        ..
    } = attachment;
    let bytes = STANDARD.decode(
        &input
            .native()
            .context("Missing saved native bytes.")?
            .content,
    )?;
    ensure!(
        bytes.len() as u64 == metadata.size
            && format!("{:x}", Sha256::digest(&bytes)) == metadata.sha256,
        "The saved native attachment bytes do not match its recorded identity."
    );
    let name = metadata
        .source
        .file_name()
        .unwrap_or("Attachment")
        .to_owned();
    let mut source = std::mem::replace(
        &mut metadata.source,
        AttachmentSource::SavedNative {
            conversation_id: conversation_id.into(),
            run_id: run_id.into(),
            ordinal,
            message_revision_id: message_revision_id.into(),
            name,
        },
    );
    let mut seen = BTreeSet::from([(run_id.to_owned(), ordinal)]);
    while let AttachmentSource::SavedNative {
        conversation_id,
        run_id,
        ordinal,
        message_revision_id,
        name,
    } = source
    {
        ensure!(
            seen.insert((run_id.clone(), ordinal)),
            "The saved attachment has a circular origin."
        );
        let parent = require_archived_native(
            db,
            project_id,
            &conversation_id,
            &run_id,
            ordinal,
            &message_revision_id,
        )?;
        ensure!(
            parent.metadata.size == metadata.size
                && parent.metadata.sha256 == metadata.sha256
                && parent.metadata.format == metadata.format
                && parent.metadata.source.file_name().unwrap_or("Attachment") == name,
            "The saved attachment no longer matches its original recorded bytes."
        );
        source = parent.metadata.source;
    }
    Ok(SavedNativeAttachment { metadata, bytes })
}

pub(crate) fn saved_native_from_source(
    db: &Connection,
    project_id: &str,
    source: &AttachmentSource,
) -> Result<Option<SavedNativeAttachment>> {
    let AttachmentSource::SavedNative {
        conversation_id,
        run_id,
        ordinal,
        message_revision_id,
        ..
    } = source
    else {
        return Ok(None);
    };
    let saved = load_saved_native_attachment(
        db,
        project_id,
        conversation_id,
        run_id,
        *ordinal,
        message_revision_id,
    )?;
    ensure!(
        &saved.metadata.source == source,
        "The saved attachment is different from the reviewed record."
    );
    Ok(Some(saved))
}

pub(crate) fn verify_saved_native_metadata(
    db: &Connection,
    project_id: &str,
    metadata: &AttachmentMetadata,
) -> Result<()> {
    if let Some(saved) = saved_native_from_source(db, project_id, &metadata.source)? {
        ensure!(
            &saved.metadata == metadata,
            "The saved attachment changed. Review it again."
        );
    }
    Ok(())
}

impl Store {
    pub async fn attachment_dispatch_content(
        &self,
        conversation_id: String,
        run_id: String,
        index: usize,
    ) -> Result<Option<String>> {
        self.execute(move |db| {
            Ok(
                archived_attachment(db, &conversation_id, &run_id, index, None)?.and_then(
                    |attachment| match attachment.input {
                        AttachmentInput::Native(native) => Some(native.content),
                        _ => None,
                    },
                ),
            )
        })
        .await
    }
}
