use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    attachments::{
        AttachmentAssessment, AttachmentBudget, AttachmentDecision, AttachmentDispatch,
        AttachmentFormat, AttachmentInput, AttachmentLimits, AttachmentMetadata, AttachmentPreview,
        AttachmentReference, NativeAttachmentEncoding, QualifiedAttachmentLimits, ReferencePreview,
        TextExcerpt, TurnAttachment, limits_for_checkout,
    },
    conversations::{SessionScope, load_conversation},
    dispatch::ContextSnapshot,
    jobs::{FrozenAttachments, freeze_attachments},
    providers::{
        capabilities::{
            CapabilityDescriptor, CapabilityScope, CapabilitySubject, ReportedCapabilities,
        },
        configuration,
    },
    runner::{DirectWindowsRunner, Runner},
};

use super::*;

const MEDIA_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
mod tests;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDraftReview {
    pub preview_id: Option<String>,
    pub files: Vec<AttachmentFileReview>,
    pub project_limits: AttachmentLimits,
    pub qualified_limits: Option<QualifiedAttachmentLimits>,
    pub minimum_encoded_bytes: u64,
    pub estimated_context_input_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentFileReview {
    pub id: String,
    pub metadata: Option<AttachmentMetadata>,
    pub delivery: Option<AttachmentDelivery>,
    pub alternatives: Vec<AttachmentDelivery>,
    pub native_unavailable: Option<String>,
    pub blocker: Option<String>,
    pub excerpt: Option<TextExcerpt>,
    pub reference: Option<AttachmentTarget>,
    pub media: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum AttachmentTarget {
    CheckoutPath(PathBuf),
    ReadOnlyCopyOf(PathBuf),
}

#[derive(Deserialize, Serialize)]
struct ReviewSeal {
    scope: SessionScope,
    #[serde(default)]
    target_checkout_id: Option<String>,
    context_id: String,
    prompt_digest: [u8; 32],
    project_limits: AttachmentLimits,
    qualified_limits: Option<QualifiedAttachmentLimits>,
    files: Vec<ReviewedFile>,
}

#[derive(Deserialize, Serialize)]
struct ReviewedFile {
    metadata: AttachmentMetadata,
    delivery: AttachmentDelivery,
    qualification: Option<String>,
}

pub(crate) struct PreparedAttachmentDraft {
    revision: String,
    context_id: String,
    pub(crate) preview_id: String,
    pub(crate) checkout_id: String,
    pub(crate) files: FrozenAttachments,
}

#[derive(Default)]
struct Qualification {
    native: Vec<(AttachmentFormat, NativeAttachmentEncoding, String)>,
    limits: Option<QualifiedAttachmentLimits>,
}

impl Store {
    pub async fn review_conversation_attachment_draft(
        &self,
        conversation_id: String,
        expected_revision: String,
        scope: SessionScope,
        text: String,
    ) -> Result<AttachmentDraftReview> {
        self.review_conversation_attachment_draft_at_checkout(
            conversation_id,
            expected_revision,
            scope,
            text,
            None,
        )
        .await
    }

    pub async fn review_conversation_attachment_draft_at_checkout(
        &self,
        conversation_id: String,
        expected_revision: String,
        scope: SessionScope,
        text: String,
        target_checkout_id: Option<String>,
    ) -> Result<AttachmentDraftReview> {
        let (conversation, checkout, draft, limits) = self
            .execute({
                let conversation_id = conversation_id.clone();
                move |db| {
                    let conversation = load_conversation(db, &conversation_id)?;
                    let checkout =
                        crate::conversations::fork_checkout::resolve_continuation_checkout(
                            db,
                            &conversation,
                            target_checkout_id.as_deref(),
                        )?;
                    let draft = load_draft(db, &conversation_id)?;
                    let limits = limits_for_checkout(db, &checkout.checkout_id)?;
                    Ok((conversation, checkout.checkout_id, draft, limits))
                }
            })
            .await?;
        ensure!(
            draft.revision.as_deref() == Some(&expected_revision),
            "The attachment draft changed. Review it again."
        );
        let qualification = attachment_qualification(self, &checkout, &scope).await?;
        let reviewed_scope = scope.clone();
        let context_id = conversation.context_id;
        let (mut reviewed, files) = tokio::task::spawn_blocking(move || {
            let qualified_limits = qualification.limits.clone();
            build_review(&reviewed_scope, draft.files, limits, qualification, &text).map(
                |(reviewed, files)| {
                    (
                        reviewed,
                        ReviewSeal {
                            scope: reviewed_scope,
                            target_checkout_id: Some(checkout),
                            context_id,
                            prompt_digest: Sha256::digest(text).into(),
                            project_limits: limits,
                            qualified_limits,
                            files,
                        },
                    )
                },
            )
        })
        .await??;
        let blocked = reviewed.files.iter().any(|entry| entry.blocker.is_some());
        let id = (!blocked).then(new_id);
        let payload = id
            .as_ref()
            .map(|_| protection::protect(&serde_json::to_vec(&files)?))
            .transpose()?;
        reviewed.preview_id = id.clone();
        self.execute(move |db| {
            ensure!(db.execute(
                "UPDATE conversation_attachment_drafts SET preview_id = ?3, preview_payload = ?4
                 WHERE conversation_id = ?1 AND revision = ?2 AND EXISTS (
                    SELECT 1 FROM conversations WHERE id = ?1 AND active_context_id = ?5)",
                params![conversation_id, expected_revision, id, payload, files.context_id],
            )? == 1, "The attachment draft or conversation changed. Review it again.");
            Ok(())
        })
        .await?;
        Ok(reviewed)
    }

    pub(crate) async fn prepare_attachment_draft_submission(
        &self,
        conversation: String,
        scope: &SessionScope,
        text: &str,
        preview_id: Option<String>,
    ) -> Result<Option<PreparedAttachmentDraft>> {
        let Some(preview_id) = preview_id else {
            return Ok(None);
        };
        let (owner, checkout, revision, seal, limits) = self.execute({
            let preview_id = preview_id.clone();
            move |db| {
                let owner = load_conversation(db, &conversation)?;
                let (revision, payload): (String, Vec<u8>) = db.query_row(
                    "SELECT revision, preview_payload FROM conversation_attachment_drafts WHERE conversation_id = ?1 AND preview_id = ?2",
                    params![conversation, preview_id], |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()?.context("The attachment preview expired. Review the files again.")?;
                let seal: ReviewSeal = serde_json::from_slice(&protection::unprotect(&payload)?)?;
                let checkout = crate::conversations::fork_checkout::resolve_continuation_checkout(db, &owner, seal.target_checkout_id.as_deref())?;
                let limits = limits_for_checkout(db, &checkout.checkout_id)?;
                Ok((owner, checkout.checkout_id, revision, seal, limits))
            }
        }).await?;
        let digest: [u8; 32] = Sha256::digest(text).into();
        ensure!(
            &seal.scope == scope
                && seal.prompt_digest == digest
                && seal.context_id == owner.context_id,
            "The message, selected profile, or conversation changed. Review the files again."
        );
        let qualification = attachment_qualification(self, &checkout, scope).await?;
        ensure!(
            limits == seal.project_limits && qualification.limits == seal.qualified_limits,
            "Attachment limits changed. Review the files again."
        );
        let root = scope.checkout.clone();
        let context_id = seal.context_id;
        let previews = tokio::task::spawn_blocking({
            let root = root.clone();
            move || {
                seal.files
                    .into_iter()
                    .map(|item| {
                        if let Some(approved) = &item.qualification {
                            ensure!(
                                qualification
                                    .native
                                    .iter()
                                    .any(|entry| entry.0 == item.metadata.format
                                        && &entry.2 == approved),
                                "Native qualification changed. Review the files again."
                            );
                        }
                        AttachmentPreview::prepare_draft_file(
                            &root,
                            &item.metadata,
                            item.delivery,
                            limits,
                        )
                    })
                    .collect::<Result<Vec<_>>>()
            }
        })
        .await??;
        let files = freeze_attachments(self, checkout.clone(), &root, previews)
            .await?
            .context("The attachment preview is empty")?;
        Ok(Some(PreparedAttachmentDraft {
            revision,
            context_id,
            preview_id,
            checkout_id: checkout,
            files,
        }))
    }
}

pub(crate) fn verify_attachment_draft(
    db: &Connection,
    conversation: &str,
    prepared: Option<&PreparedAttachmentDraft>,
) -> Result<()> {
    let current = db.query_row(
        "SELECT revision, preview_id FROM conversation_attachment_drafts WHERE conversation_id = ?1", [conversation],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    ).optional()?;
    ensure!(
        match (current, prepared) {
            (None, None) => true,
            (Some((revision, Some(id))), Some(prepared)) =>
                revision == prepared.revision
                    && id == prepared.preview_id
                    && load_conversation(db, conversation)?.context_id == prepared.context_id,
            _ => false,
        },
        "The attachment draft changed. Review the files again."
    );
    Ok(())
}

pub(crate) fn consume_attachment_draft(db: &Connection, conversation: &str) -> Result<()> {
    db.execute(
        "DELETE FROM conversation_attachment_drafts WHERE conversation_id = ?1",
        [conversation],
    )?;
    Ok(())
}

async fn attachment_qualification(
    store: &Store,
    checkout: &str,
    scope: &SessionScope,
) -> Result<Qualification> {
    let layout =
        repository::verify_checkout(store, checkout.to_owned(), scope.checkout.clone()).await?;
    if scope.runner != DirectWindowsRunner.identity() {
        return Ok(Qualification::default());
    }
    let runtime = scope.clone();
    let executable = tokio::task::spawn_blocking(move || {
        DirectWindowsRunner.executable_revision(&runtime.executable)
    })
    .await?;
    let Some(executable) = executable else {
        return Ok(Qualification::default());
    };
    let saved = store
        .execute({
            let checkout = checkout.to_owned();
            let scope = scope.clone();
            let executable = executable.clone();
            move |db| {
                let mut statement = db.prepare(
                    "SELECT s.payload FROM context_snapshots s JOIN runs r ON r.id = s.run_id
                 WHERE r.checkout_id = ?1 ORDER BY r.created_at DESC LIMIT 24",
                )?;
                for row in statement.query_map([checkout], |row| row.get::<_, Vec<u8>>(0))? {
                    let snapshot: ContextSnapshot =
                        serde_json::from_slice(&protection::unprotect(&row?)?)?;
                    if snapshot.scope == scope
                        && let Some(capabilities) = snapshot.capabilities
                        && capabilities.scope.executable_revision.as_deref() == Some(&executable)
                    {
                        return Ok(Some(capabilities.scope));
                    }
                }
                Ok(None)
            }
        })
        .await?;
    let effective = match saved {
        Some(scope) => scope,
        None => {
            let mut scope = scope.clone();
            let (configuration, _, _) =
                configuration::inspect(&scope, &layout, &ReportedCapabilities::default())?;
            scope.checkout = layout.cwd;
            CapabilityScope::from_dispatch(
                &scope,
                checkout.to_owned(),
                Some(executable),
                DirectWindowsRunner.execution_platform(),
                configuration,
                scope.model.clone(),
                scope.effort.clone(),
            )
        }
    };
    let encodings = [
        NativeAttachmentEncoding::CodexLocalImage,
        NativeAttachmentEncoding::ClaudeImage,
        NativeAttachmentEncoding::ClaudeDocument,
        NativeAttachmentEncoding::AcpImage,
        NativeAttachmentEncoding::AcpAudio,
        NativeAttachmentEncoding::AcpResource,
    ];
    let mut descriptors = vec![CapabilityDescriptor::attachment_limits()];
    descriptors.extend(
        encodings
            .into_iter()
            .filter(|encoding| encoding.provider() == scope.provider)
            .flat_map(|encoding| {
                AttachmentFormat::ALL
                    .into_iter()
                    .filter(move |format| encoding.supports(*format))
                    .map(move |format| CapabilityDescriptor::native_attachment(format, encoding))
            }),
    );
    let resolved = store
        .resolve_capability_assessment(effective, descriptors)
        .await?;
    let mut qualification = Qualification::default();
    for entry in resolved.entries {
        if let Some(evidence) = entry.evidence.filter(|evidence| evidence.verified()) {
            match entry.descriptor.subject {
                CapabilitySubject::NativeAttachment { format, encoding } => {
                    qualification.native.push((format, encoding, evidence.id))
                }
                CapabilitySubject::AttachmentLimits => {
                    qualification.limits = evidence.attachment_limits
                }
                _ => (),
            }
        }
    }
    Ok(qualification)
}

fn build_review(
    scope: &SessionScope,
    files: Vec<DraftAttachment>,
    limits: AttachmentLimits,
    qualification: Qualification,
    text: &str,
) -> Result<(AttachmentDraftReview, Vec<ReviewedFile>)> {
    let mut reviewed = Vec::new();
    let mut sealed = Vec::new();
    let mut inputs = Vec::new();
    let mut budget = AttachmentBudget::new(limits);
    let mut assessment = AttachmentAssessment {
        project: limits,
        qualified: qualification.limits.clone(),
        qualification_id: None,
        files: Vec::new(),
        blockers: Vec::new(),
        encoded_request_bytes: None,
        estimated_context_input_tokens: None,
        provider_context: None,
        provider_history_usage_unknown: true,
    };
    let mut wire_bytes = text.len() as u64;
    for (index, file) in files.into_iter().enumerate() {
        let mut row = AttachmentFileReview {
            id: file.id,
            metadata: None,
            delivery: file.delivery,
            alternatives: Vec::new(),
            native_unavailable: None,
            blocker: None,
            excerpt: None,
            reference: None,
            media: None,
        };
        let result = (|| -> Result<()> {
            let (metadata, suggested_end) = AttachmentPreview::describe_draft_file(
                &scope.checkout,
                &file.source,
                limits.utf8_bytes_per_excerpt,
            )?;
            row.metadata = Some(metadata.clone());
            let native = qualification
                .native
                .iter()
                .find(|entry| entry.0 == metadata.format);
            let native_route = limits
                .check_file(&file.source, AttachmentDelivery::Native, metadata.size)
                .and_then(|_| {
                    native.context(
                        "Native delivery is unverified for this format, profile, and checkout",
                    )
                })
                .and_then(|proof| {
                    if let Some(limits) = &qualification.limits {
                        limits.check_file(index, &metadata, Some(proof.1))?;
                    }
                    Ok(proof)
                });
            row.native_unavailable = native_route
                .as_ref()
                .err()
                .map(|error| format!("{error:#}"));
            if native_route.is_ok() {
                row.alternatives.push(AttachmentDelivery::Native);
            }
            if suggested_end > 0 {
                row.alternatives.push(AttachmentDelivery::Excerpt {
                    start: 0,
                    end: suggested_end,
                });
            }
            if limits
                .check_file(&file.source, AttachmentDelivery::Reference, metadata.size)
                .is_ok()
            {
                row.alternatives.push(AttachmentDelivery::Reference);
            }
            let delivery = file
                .delivery
                .or_else(|| row.alternatives.first().copied())
                .context(
                    "This file has no eligible delivery route. Remove it or use a smaller file.",
                )?;
            row.delivery = Some(delivery);
            let native = if delivery == AttachmentDelivery::Native {
                Some(native_route?)
            } else {
                None
            };
            budget.check(&file.source, delivery, metadata.size)?;
            if let Some(limits) = &qualification.limits {
                limits.check_file(index, &metadata, native.map(|entry| entry.1))?;
            }
            let preview = AttachmentPreview::prepare_draft_file(
                &scope.checkout,
                &metadata,
                delivery,
                limits,
            )?;
            row.excerpt = preview.excerpt().cloned();
            row.reference = preview.reference_target().map(|target| match target {
                ReferencePreview::CheckoutPath(path) => AttachmentTarget::CheckoutPath(path.into()),
                ReferencePreview::ReadOnlyCopyOf(path) => {
                    AttachmentTarget::ReadOnlyCopyOf(path.into())
                }
            });
            let input = if let Some((_, encoding, _)) = native {
                row.media = preview.draft_asset(MEDIA_PREVIEW_BYTES)?;
                if *encoding != NativeAttachmentEncoding::CodexLocalImage {
                    wire_bytes =
                        wire_bytes.saturating_add(metadata.size.div_ceil(3).saturating_mul(4));
                }
                AttachmentInput::Native(TurnAttachment {
                    encoding: *encoding,
                    format: metadata.format,
                    content: String::new(),
                    path: None,
                    resource_uri: None,
                })
            } else if let Some(excerpt) = &row.excerpt {
                wire_bytes = wire_bytes.saturating_add(excerpt.text.len() as u64);
                AttachmentInput::Excerpt(excerpt.clone())
            } else {
                let target = row.reference.as_ref().context("Missing reference target")?;
                let path = match target {
                    AttachmentTarget::CheckoutPath(path)
                    | AttachmentTarget::ReadOnlyCopyOf(path) => path,
                };
                let uri = tauri::Url::from_file_path(path).ok().map(Into::into);
                AttachmentInput::Reference(AttachmentReference {
                    name: file.source.file_name().unwrap_or("Attachment").into(),
                    path: path.clone(),
                    uri,
                    format: metadata.format,
                    size: metadata.size,
                })
            };
            let proof = native.map(|entry| entry.2.clone());
            inputs.push(AttachmentDispatch {
                message_revision_id: row.id.clone(),
                metadata: metadata.clone(),
                qualification_id: proof.clone(),
                input,
            });
            assessment.files.push(AttachmentDecision {
                message_revision_id: row.id.clone(),
                metadata: metadata.clone(),
                delivery,
                encoding: native.map(|entry| entry.1),
                qualification_id: proof.clone(),
            });
            sealed.push(ReviewedFile {
                metadata,
                delivery,
                qualification: proof,
            });
            Ok(())
        })();
        row.blocker = result.err().map(|error| format!("{error:#}"));
        reviewed.push(row);
    }
    assessment.check_context(scope.provider, text, &inputs)?;
    assessment.check_encoded_request(wire_bytes);
    for blocker in assessment.blockers {
        if let Some(file) = reviewed
            .iter_mut()
            .find(|file| file.id == blocker.message_revision_id)
        {
            file.blocker.get_or_insert(blocker.reason);
        }
    }
    Ok((
        AttachmentDraftReview {
            preview_id: None,
            files: reviewed,
            project_limits: limits,
            qualified_limits: qualification.limits,
            minimum_encoded_bytes: wire_bytes,
            estimated_context_input_tokens: assessment.estimated_context_input_tokens,
        },
        sealed,
    ))
}
