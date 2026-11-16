use std::{io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::{repository, store::Store};

use super::{
    AttachmentBlocker, AttachmentDelivery, AttachmentDispatch, AttachmentFormat, AttachmentInput,
    AttachmentMetadata, AttachmentPreview, AttachmentSource, NativeAttachmentEncoding,
    authorized_path, inspect,
};
use crate::providers::Provider;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualifiedAttachmentLimits {
    pub files_per_turn: Option<u32>,
    pub encoded_request_bytes: Option<u64>,
    #[serde(default)]
    pub formats: Vec<QualifiedAttachmentFormat>,
    pub context: Option<QualifiedAttachmentContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualifiedAttachmentFormat {
    pub format: AttachmentFormat,
    pub encoding: NativeAttachmentEncoding,
    pub file_bytes: Option<u64>,
    /// Qualified upper token cost per file within the verified byte limit.
    pub input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualifiedAttachmentContext {
    pub window_tokens: u64,
    pub reserved_output_tokens: u64,
    /// Upper bound for the provider-added prompt, tools, and framing on a fresh session.
    pub fixed_input_tokens: u64,
}

impl QualifiedAttachmentLimits {
    pub(crate) fn validate(&self, provider: Provider) -> Result<()> {
        if let Some(context) = &self.context {
            ensure!(
                context.window_tokens > context.reserved_output_tokens
                    && context.window_tokens - context.reserved_output_tokens
                        >= context.fixed_input_tokens,
                "qualified context must accommodate the output reserve and fixed input"
            );
        }
        for (index, format) in self.formats.iter().enumerate() {
            ensure!(
                format.encoding.provider() == provider && format.encoding.supports(format.format),
                "qualified file limit does not match the transport and format"
            );
            ensure!(
                !self.formats[..index].iter().any(|prior| prior.format == format.format && prior.encoding == format.encoding),
                "duplicate qualified format limit"
            );
            ensure!(
                format.input_tokens.is_none() || format.file_bytes.is_some(),
                "qualified native token cost requires a bounded file size"
            );
        }
        ensure!(
            *self != Self::default(),
            "qualification has no attachment limits"
        );
        Ok(())
    }

    pub(crate) fn check_file(
        &self,
        index: usize,
        metadata: &AttachmentMetadata,
        encoding: Option<NativeAttachmentEncoding>,
    ) -> Result<()> {
        if let Some(count) = self.files_per_turn {
            ensure!(
                index < count as usize,
                "file {} exceeds the qualified limit of {count} attachments per turn",
                index + 1
            );
        }
        if let Some(limit) = encoding
            .and_then(|encoding| self.file_limit(metadata.format, encoding))
            .and_then(|format| format.file_bytes)
        {
            ensure!(
                metadata.size <= limit,
                "file has {} bytes and the qualified {} limit is {limit} bytes",
                metadata.size,
                metadata.format.mime()
            );
        }
        Ok(())
    }

    fn file_limit(
        &self,
        format: AttachmentFormat,
        encoding: NativeAttachmentEncoding,
    ) -> Option<&QualifiedAttachmentFormat> {
        self.formats
            .iter()
            .find(|entry| entry.format == format && entry.encoding == encoding)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDecision {
    pub message_revision_id: String,
    pub metadata: AttachmentMetadata,
    pub delivery: AttachmentDelivery,
    pub encoding: Option<NativeAttachmentEncoding>,
    pub qualification_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentContextUsage {
    pub used: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentAssessment {
    pub project: AttachmentLimits,
    pub qualified: Option<QualifiedAttachmentLimits>,
    pub qualification_id: Option<String>,
    pub files: Vec<AttachmentDecision>,
    pub blockers: Vec<AttachmentBlocker>,
    pub encoded_request_bytes: Option<u64>,
    pub estimated_context_input_tokens: Option<u64>,
    pub provider_context: Option<AttachmentContextUsage>,
    pub provider_history_usage_unknown: bool,
}

impl AttachmentDecision {
    pub(crate) fn blocker(&self, reason: String) -> AttachmentBlocker {
        AttachmentBlocker {
            message_revision_id: self.message_revision_id.clone(),
            source: self.metadata.source.clone(),
            delivery: self.delivery,
            reason,
        }
    }
}

impl AttachmentAssessment {
    pub(crate) fn check_context(
        &mut self,
        provider: Provider,
        prompt: &str,
        attachments: &[AttachmentDispatch],
    ) -> Result<()> {
        let Some(limits) = &self.qualified else {
            return Ok(());
        };
        let Some(context) = &limits.context else {
            return Ok(());
        };
        let (retained, window) = self
            .provider_context
            .map_or((0, context.window_tokens), |report| {
                (report.used, context.window_tokens.min(report.size))
            });
        let available = window.saturating_sub(context.reserved_output_tokens);
        let mut total = retained
            .max(context.fixed_input_tokens)
            .saturating_add(prompt.len() as u64);
        let mut complete = true;
        for (file, attachment) in self.files.iter().zip(attachments) {
            let added = match &attachment.input {
                AttachmentInput::Native(native) => limits
                    .file_limit(native.format, native.encoding)
                    .and_then(|entry| entry.input_tokens),
                fallback => {
                    // Count UTF-8 bytes for client-visible text, including attachment labels.
                    let block = fallback.block(provider)?;
                    Some(
                        match block.get("text").and_then(serde_json::Value::as_str) {
                            Some(text) => text.len() as u64,
                            None => serde_json::to_vec(&block)?.len() as u64,
                        },
                    )
                }
            };
            if let Some(added) = added {
                total = total.saturating_add(added);
                if total > available {
                    self.blockers.push(file.blocker(format!(
                        "estimated context input through this file is {total} tokens with {available} available after reserving {} for output",
                        context.reserved_output_tokens
                    )));
                }
            } else {
                complete = false;
                self.blockers.push(file.blocker("this native format has no qualified token cost within the active context limit".into()));
            }
        }
        self.estimated_context_input_tokens = complete.then_some(total);
        Ok(())
    }

    pub(crate) fn check_encoded_request(&mut self, bytes: u64) {
        self.encoded_request_bytes = Some(bytes);
        if let Some(limit) = self
            .qualified
            .as_ref()
            .and_then(|limits| limits.encoded_request_bytes)
            && bytes > limit
        {
            let reason = format!(
                "the combined encoded request has {bytes} bytes and the qualified limit is {limit} bytes"
            );
            self.blockers
                .extend(self.files.iter().map(|file| file.blocker(reason.clone())));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct AttachmentLimits {
    pub files_per_turn: u32,
    #[ts(type = "number")]
    pub bytes_per_file: u64,
    #[ts(type = "number")]
    pub bytes_per_turn: u64,
    #[ts(type = "number")]
    pub utf8_bytes_per_excerpt: u64,
}

impl Default for AttachmentLimits {
    fn default() -> Self {
        Self {
            files_per_turn: 10,
            bytes_per_file: 20 * 1024 * 1024,
            bytes_per_turn: 50 * 1024 * 1024,
            utf8_bytes_per_excerpt: 64 * 1024,
        }
    }
}

impl AttachmentLimits {
    pub(crate) fn check_file(
        self,
        source: &AttachmentSource,
        delivery: AttachmentDelivery,
        size: u64,
    ) -> Result<()> {
        if delivery.uses_file_budget(source) {
            ensure!(
                size <= self.bytes_per_file,
                "file has {size} bytes and the project file limit is {} bytes",
                self.bytes_per_file
            );
        }
        if let AttachmentDelivery::Excerpt { start, end } = delivery {
            ensure!(
                start < end && end <= size,
                "excerpt range is outside the file"
            );
            ensure!(
                end - start <= self.utf8_bytes_per_excerpt,
                "excerpt has {} UTF-8 bytes and the project limit is {} bytes",
                end - start,
                self.utf8_bytes_per_excerpt
            );
        }
        Ok(())
    }
}

impl AttachmentPreview {
    pub(crate) fn describe_draft_file(
        checkout: &Path,
        source: &AttachmentSource,
        excerpt_limit: u64,
    ) -> Result<(AttachmentMetadata, u64)> {
        let path = authorized_path(checkout, source)?;
        let mut file = crate::providers::configuration::open_locked_local_file(&path)?;
        let metadata = inspect(&mut file, source)?;
        let mut first = Vec::new();
        if metadata.format == AttachmentFormat::Text {
            file.take(excerpt_limit.min(64 * 1024))
                .read_to_end(&mut first)?;
        }
        let end = match std::str::from_utf8(&first) {
            Ok(text) => text.len(),
            Err(error) => error.valid_up_to(),
        };
        Ok((metadata, end as u64))
    }

    pub(crate) fn prepare_draft_file(
        checkout: &Path,
        metadata: &AttachmentMetadata,
        delivery: AttachmentDelivery,
        limits: AttachmentLimits,
    ) -> Result<Self> {
        let preview = Self::read_with_limits(checkout, metadata.source.clone(), delivery, limits)?;
        ensure!(
            &preview.metadata == metadata,
            "The file changed. Review it again."
        );
        Ok(preview)
    }

    pub(crate) fn draft_asset(&self, max_bytes: u64) -> Result<Option<String>> {
        if self.metadata.size > max_bytes || self.metadata.format == AttachmentFormat::Unknown {
            return Ok(None);
        }
        let path = authorized_path(&self.checkout, &self.metadata.source)?;
        ensure!(
            path == self.canonical,
            "The file path changed. Review it again."
        );
        let file = crate::providers::configuration::open_locked_local_file(&path)?;
        let mut bytes = Vec::new();
        file.take(max_bytes + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == self.metadata.size
                && format!("{:x}", Sha256::digest(&bytes)) == self.metadata.sha256,
            "The file changed. Review it again."
        );
        Ok(Some(STANDARD.encode(bytes)))
    }
}

pub(crate) struct AttachmentBudget {
    limits: AttachmentLimits,
    count: u64,
    charged_bytes: u64,
}

impl AttachmentBudget {
    pub(crate) fn new(limits: AttachmentLimits) -> Self {
        Self {
            limits,
            count: 0,
            charged_bytes: 0,
        }
    }

    pub(crate) fn check(
        &mut self,
        source: &AttachmentSource,
        delivery: AttachmentDelivery,
        size: u64,
    ) -> Result<()> {
        self.count += 1;
        if delivery.uses_file_budget(source) {
            self.charged_bytes = self
                .charged_bytes
                .checked_add(size)
                .context("combined attachment size exceeds the application range")?;
        }
        self.limits.check_file(source, delivery, size)?;
        ensure!(
            self.count <= u64::from(self.limits.files_per_turn),
            "file {} exceeds the project limit of {} attachments per turn",
            self.count,
            self.limits.files_per_turn
        );
        if delivery.uses_file_budget(source) {
            ensure!(
                self.charged_bytes <= self.limits.bytes_per_turn,
                "native inputs and staged copies total {} bytes through this file and the project limit is {} bytes per turn",
                self.charged_bytes,
                self.limits.bytes_per_turn
            );
        }
        Ok(())
    }
}

pub(crate) fn limits_for_checkout(db: &Connection, checkout: &str) -> Result<AttachmentLimits> {
    let settings: Option<String> = db.query_row(
        "SELECT p.attachment_limits FROM projects p JOIN checkouts c ON c.project_id = p.id WHERE c.id = ?1",
        [checkout], |row| row.get(0),
    )?;
    decode(settings)
}

fn decode(settings: Option<String>) -> Result<AttachmentLimits> {
    settings
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("invalid project attachment limits")
        .map(Option::unwrap_or_default)
}

impl Store {
    pub async fn project_attachment_limits(&self, project_id: String) -> Result<AttachmentLimits> {
        self.execute(move |db| {
            let settings = db.query_row(
                "SELECT attachment_limits FROM projects WHERE id = ?1",
                [project_id],
                |row| row.get(0),
            )?;
            decode(settings)
        })
        .await
    }

    pub async fn set_project_attachment_limits(
        &self,
        project_id: String,
        limits: AttachmentLimits,
    ) -> Result<()> {
        let settings = serde_json::to_string(&limits)?;
        self.execute(move |db| {
            let changed = db.execute(
                "UPDATE projects SET attachment_limits = ?2 WHERE id = ?1",
                params![project_id, settings],
            )?;
            ensure!(changed == 1, "project does not exist");
            Ok(())
        })
        .await
    }

    pub(crate) async fn checkout_attachment_limits(
        &self,
        checkout_id: String,
    ) -> Result<AttachmentLimits> {
        self.execute(move |db| limits_for_checkout(db, &checkout_id))
            .await
    }

    /// External files must come from the application's individual file selection.
    pub async fn preview_attachment(
        &self,
        checkout_id: String,
        source: AttachmentSource,
        delivery: AttachmentDelivery,
    ) -> Result<AttachmentPreview> {
        let checkout = repository::checkout_root(self, checkout_id.clone()).await?;
        let limits = self.checkout_attachment_limits(checkout_id).await?;
        tokio::task::spawn_blocking(move || {
            AttachmentPreview::read_with_limits(&checkout, source, delivery, limits)
        })
        .await?
    }
}
