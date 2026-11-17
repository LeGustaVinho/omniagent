use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::windows::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    protection,
    providers::{
        Provider,
        configuration::{open_locked_local_file, plain_metadata},
    },
    runner::{
        AttachmentRunScope, AttachmentStaging, CheckoutAttachment, Runner, StagedAttachmentFile,
    },
    store::Store,
};

mod limits;
pub use limits::{
    AttachmentAssessment, AttachmentContextUsage, AttachmentDecision, AttachmentLimits,
    QualifiedAttachmentContext, QualifiedAttachmentFormat, QualifiedAttachmentLimits,
};
pub(crate) use limits::{AttachmentBudget, limits_for_checkout};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Pdf,
    Wav,
    Mp3,
    Text,
    Unknown,
}

impl AttachmentFormat {
    pub const ALL: [Self; 9] = [
        Self::Png,
        Self::Jpeg,
        Self::Gif,
        Self::Webp,
        Self::Pdf,
        Self::Wav,
        Self::Mp3,
        Self::Text,
        Self::Unknown,
    ];
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Pdf => "application/pdf",
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
            Self::Text => "text/plain",
            Self::Unknown => "application/octet-stream",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Pdf => "pdf",
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Text => "txt",
            Self::Unknown => "bin",
        }
    }

    pub fn is_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Gif | Self::Webp)
    }

    pub fn is_audio(self) -> bool {
        matches!(self, Self::Wav | Self::Mp3)
    }

    fn detect_binary(bytes: &[u8]) -> Option<Self> {
        match infer::get(bytes).map(|kind| kind.mime_type()) {
            Some("image/png") => Some(Self::Png),
            Some("image/jpeg") => Some(Self::Jpeg),
            Some("image/gif") => Some(Self::Gif),
            Some("image/webp") => Some(Self::Webp),
            Some("application/pdf") => Some(Self::Pdf),
            Some("audio/wav" | "audio/x-wav") => Some(Self::Wav),
            Some("audio/mpeg") => Some(Self::Mp3),
            Some(_) => Some(Self::Unknown),
            None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AttachmentSource {
    Checkout { relative_path: PathBuf },
    SelectedFile { path: PathBuf },
}

impl AttachmentSource {
    pub fn file_name(&self) -> Option<&str> {
        match self {
            Self::Checkout { relative_path } => relative_path,
            Self::SelectedFile { path } => path,
        }
        .file_name()?
        .to_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMetadata {
    pub source: AttachmentSource,
    pub format: AttachmentFormat,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentDelivery {
    Native,
    Excerpt { start: u64, end: u64 },
    Reference,
}

impl AttachmentDelivery {
    fn uses_file_budget(self, source: &AttachmentSource) -> bool {
        match self {
            Self::Native => true,
            Self::Excerpt { .. } => false,
            Self::Reference => matches!(source, AttachmentSource::SelectedFile { .. }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferencePreview<'a> {
    CheckoutPath(&'a Path),
    ReadOnlyCopyOf(&'a Path),
}

pub struct AttachmentPreview {
    metadata: AttachmentMetadata,
    canonical: PathBuf,
    checkout: PathBuf,
    delivery: AttachmentDelivery,
    excerpt: Option<TextExcerpt>,
}

impl AttachmentPreview {
    pub fn metadata(&self) -> &AttachmentMetadata {
        &self.metadata
    }
    /// An external path must come from the application's individual file selection.
    pub fn read(checkout: &Path, source: AttachmentSource) -> Result<Self> {
        Self::read_with_delivery(checkout, source, AttachmentDelivery::Native)
    }

    /// An external path must come from the application's individual file selection.
    pub fn read_with_delivery(
        checkout: &Path,
        source: AttachmentSource,
        delivery: AttachmentDelivery,
    ) -> Result<Self> {
        Self::read_with_limits(checkout, source, delivery, AttachmentLimits::default())
    }

    fn read_with_limits(
        checkout: &Path,
        source: AttachmentSource,
        delivery: AttachmentDelivery,
        limits: AttachmentLimits,
    ) -> Result<Self> {
        let result = (|| {
            let canonical = authorized_path(checkout, &source)?;
            let mut file = open_locked_local_file(&canonical)?;
            AttachmentBudget::new(limits).check(&source, delivery, file.metadata()?.len())?;
            let metadata = inspect(&mut file, &source)?;
            let excerpt = read_excerpt(&mut file, &metadata, delivery)?;
            Ok(Self {
                metadata,
                canonical,
                checkout: checkout.into(),
                delivery,
                excerpt,
            })
        })();
        with_file_reason(result, source.file_name().unwrap_or("attachment"))
    }

    pub fn delivery(&self) -> AttachmentDelivery {
        self.delivery
    }

    pub fn excerpt(&self) -> Option<&TextExcerpt> {
        self.excerpt.as_ref()
    }

    pub fn reference_target(&self) -> Option<ReferencePreview<'_>> {
        if self.delivery != AttachmentDelivery::Reference {
            return None;
        }
        Some(match self.metadata.source {
            AttachmentSource::Checkout { .. } => ReferencePreview::CheckoutPath(&self.canonical),
            AttachmentSource::SelectedFile { .. } => {
                ReferencePreview::ReadOnlyCopyOf(&self.canonical)
            }
        })
    }

    fn capture(self, checkout: &Path, limits: AttachmentLimits) -> Result<CapturedAttachment> {
        ensure!(
            self.checkout == checkout,
            "attachment preview belongs to another checkout"
        );
        let path = authorized_path(checkout, &self.metadata.source)?;
        ensure!(
            path == self.canonical,
            "attachment path changed after preview"
        );
        let mut file = open_locked_local_file(&path)?;
        limits.check_file(&self.metadata.source, self.delivery, file.metadata()?.len())?;
        let observed = inspect(&mut file, &self.metadata.source)?;
        ensure!(
            observed == self.metadata,
            "attachment changed after preview, preview it again"
        );
        let payload = match self.delivery {
            AttachmentDelivery::Native => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                CapturedPayload::Native {
                    content: STANDARD.encode(bytes),
                }
            }
            AttachmentDelivery::Excerpt { .. } => CapturedPayload::Excerpt {
                excerpt: self.excerpt.context("missing previewed excerpt")?,
            },
            AttachmentDelivery::Reference => CapturedPayload::Reference {
                reference_path: path,
            },
        };
        Ok(CapturedAttachment {
            metadata: observed,
            payload,
        })
    }
}

fn authorized_path(checkout: &Path, source: &AttachmentSource) -> Result<PathBuf> {
    ensure!(
        checkout.is_absolute() && dunce::canonicalize(checkout)? == checkout,
        "attachment checkout must be canonical"
    );
    let path = match source {
        AttachmentSource::Checkout { relative_path } => {
            ensure!(
                relative_path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
                "attachment must be inside the checkout"
            );
            checkout.join(relative_path)
        }
        AttachmentSource::SelectedFile { path } => path.clone(),
    };
    ensure!(
        plain_metadata(&path)?.is_some_and(|entry| entry.is_file()),
        "attachment must be a regular local file"
    );
    let canonical = dunce::canonicalize(&path)?;
    ensure!(
        dunce::simplified(&canonical)
            .as_os_str()
            .eq_ignore_ascii_case(dunce::simplified(&path).as_os_str()),
        "attachment path changed during resolution"
    );
    if matches!(source, AttachmentSource::Checkout { .. }) {
        ensure!(
            canonical.starts_with(checkout),
            "attachment resolved outside the checkout"
        );
    }
    Ok(canonical)
}

fn inspect(file: &mut File, source: &AttachmentSource) -> Result<AttachmentMetadata> {
    let size = file.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = [0; 8192];
    let mut carry = 0;
    let mut consumed = 0;
    let mut format = None;
    while consumed < size {
        let read = file.read(&mut buffer[carry..])?;
        ensure!(read > 0, "attachment size changed during inspection");
        if consumed == 0 {
            format = AttachmentFormat::detect_binary(&buffer[..read]);
        }
        consumed += read as u64;
        digest.update(&buffer[carry..carry + read]);
        if format.is_some() {
            continue;
        }
        let end = carry + read;
        let text = &buffer[..end];
        carry = match std::str::from_utf8(text) {
            Ok(_) if !text.contains(&0) => 0,
            Err(error) if error.error_len().is_none() && !text.contains(&0) => {
                let remaining = end - error.valid_up_to();
                buffer.copy_within(error.valid_up_to()..end, 0);
                remaining
            }
            _ => {
                format = Some(AttachmentFormat::Unknown);
                0
            }
        };
    }
    ensure!(
        consumed == size,
        "attachment size changed during inspection"
    );
    file.rewind()?;
    Ok(AttachmentMetadata {
        source: source.clone(),
        format: format.unwrap_or(if carry == 0 {
            AttachmentFormat::Text
        } else {
            AttachmentFormat::Unknown
        }),
        sha256: format!("{:x}", digest.finalize()),
        size,
    })
}

fn read_excerpt(
    file: &mut File,
    metadata: &AttachmentMetadata,
    delivery: AttachmentDelivery,
) -> Result<Option<TextExcerpt>> {
    let AttachmentDelivery::Excerpt { start, end } = delivery else {
        return Ok(None);
    };
    ensure!(
        metadata.format == AttachmentFormat::Text,
        "excerpt requires a UTF-8 text file"
    );
    file.seek(SeekFrom::Start(start))?;
    let mut selected = vec![0; (end - start) as usize];
    file.read_exact(&mut selected)?;
    Ok(Some(TextExcerpt {
        name: metadata
            .source
            .file_name()
            .unwrap_or("attachment")
            .to_owned(),
        start,
        end,
        text: String::from_utf8(selected).context("excerpt splits a UTF-8 character")?,
    }))
}

#[derive(Serialize, Deserialize)]
pub(crate) struct CapturedAttachment {
    pub metadata: AttachmentMetadata,
    #[serde(flatten)]
    pub payload: CapturedPayload,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum CapturedPayload {
    Native { content: String },
    Excerpt { excerpt: TextExcerpt },
    Reference { reference_path: PathBuf },
}

impl CapturedAttachment {
    pub(crate) fn delivery(&self) -> AttachmentDelivery {
        match &self.payload {
            CapturedPayload::Native { .. } => AttachmentDelivery::Native,
            CapturedPayload::Excerpt { excerpt } => AttachmentDelivery::Excerpt {
                start: excerpt.start,
                end: excerpt.end,
            },
            CapturedPayload::Reference { .. } => AttachmentDelivery::Reference,
        }
    }

    pub(crate) fn validate_reference(&self, checkout: &Path) -> Result<()> {
        if let CapturedPayload::Reference { reference_path } = &self.payload {
            open_reference(checkout, &self.metadata, reference_path)?;
        }
        Ok(())
    }
}

fn open_reference(
    checkout: &Path,
    metadata: &AttachmentMetadata,
    captured_path: &Path,
) -> Result<File> {
    let path = authorized_path(checkout, &metadata.source)?;
    ensure!(
        path == captured_path,
        "attachment reference target changed, preview it again"
    );
    let mut lock = open_locked_local_file(&path)?;
    ensure!(
        lock.metadata()?.len() == metadata.size
            && inspect(&mut lock, &metadata.source)? == *metadata,
        "attachment reference changed, preview it again"
    );
    Ok(lock)
}

pub(crate) fn capture(
    checkout: &Path,
    previews: Vec<AttachmentPreview>,
    limits: AttachmentLimits,
) -> Result<Vec<CapturedAttachment>> {
    check_limits(
        limits,
        previews.iter().map(|file| (&file.metadata, file.delivery)),
    )?;
    previews
        .into_iter()
        .map(|preview| {
            let label = preview
                .metadata
                .source
                .file_name()
                .unwrap_or("attachment")
                .to_owned();
            with_file_reason(preview.capture(checkout, limits), &label)
        })
        .collect()
}

pub(crate) fn captured_for_revision(
    db: &Connection,
    revision: &str,
) -> Result<Vec<CapturedAttachment>> {
    let payload: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM message_attachments WHERE message_revision_id = ?1",
            [revision],
            |row| row.get(0),
        )
        .optional()?;
    match payload {
        Some(payload) => Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?),
        None => Ok(Vec::new()),
    }
}

fn with_file_reason<T>(result: Result<T>, label: &str) -> Result<T> {
    result.map_err(|error| anyhow::anyhow!("{label}: {error:#}"))
}

pub(crate) fn check_limits<'a>(
    limits: AttachmentLimits,
    files: impl IntoIterator<Item = (&'a AttachmentMetadata, AttachmentDelivery)>,
) -> Result<()> {
    let mut budget = AttachmentBudget::new(limits);
    for (metadata, delivery) in files {
        with_file_reason(
            budget.check(&metadata.source, delivery, metadata.size),
            metadata.source.file_name().unwrap_or("attachment"),
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAttachmentEncoding {
    CodexLocalImage,
    ClaudeImage,
    ClaudeDocument,
    AcpImage,
    AcpAudio,
    AcpResource,
}

impl NativeAttachmentEncoding {
    pub fn provider(self) -> Provider {
        match self {
            Self::CodexLocalImage => Provider::Codex,
            Self::ClaudeImage | Self::ClaudeDocument => Provider::Claude,
            Self::AcpImage | Self::AcpAudio | Self::AcpResource => Provider::Antigravity,
        }
    }
    pub fn supports(self, format: AttachmentFormat) -> bool {
        match self {
            Self::CodexLocalImage | Self::ClaudeImage | Self::AcpImage => format.is_image(),
            Self::ClaudeDocument => format == AttachmentFormat::Pdf,
            Self::AcpResource => matches!(format, AttachmentFormat::Pdf | AttachmentFormat::Text),
            Self::AcpAudio => format.is_audio(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDispatch {
    pub message_revision_id: String,
    pub metadata: AttachmentMetadata,
    #[serde(default)]
    pub qualification_id: Option<String>,
    pub input: AttachmentInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentBlocker {
    pub message_revision_id: String,
    pub source: AttachmentSource,
    pub delivery: AttachmentDelivery,
    pub reason: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttachmentInput {
    Native(TurnAttachment),
    Excerpt(TextExcerpt),
    Reference(AttachmentReference),
}

impl AttachmentInput {
    pub fn native(&self) -> Option<&TurnAttachment> {
        match self {
            Self::Native(native) => Some(native),
            _ => None,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Native(native) => native.path.as_deref(),
            Self::Excerpt(_) => None,
            Self::Reference(reference) => Some(&reference.path),
        }
    }

    pub(crate) fn block(&self, provider: Provider) -> Result<Value> {
        match self {
            Self::Native(native) => native.block(provider),
            Self::Excerpt(excerpt) => Ok(json!({
                "type":"text",
                "text":format!(
                    "Excerpt from attachment {:?} (UTF-8 bytes {}..{}):\n{}",
                    excerpt.name, excerpt.start, excerpt.end, excerpt.text,
                ),
            })),
            Self::Reference(reference) => reference.block(provider),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextExcerpt {
    pub name: String,
    pub start: u64,
    pub end: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentReference {
    pub name: String,
    pub path: PathBuf,
    pub uri: Option<String>,
    pub format: AttachmentFormat,
    pub size: u64,
}

impl AttachmentReference {
    fn block(&self, provider: Provider) -> Result<Value> {
        ensure!(
            self.path.is_absolute(),
            "attachment reference requires an absolute runner path"
        );
        Ok(match provider {
            Provider::Antigravity => {
                let uri = self
                    .uri
                    .as_ref()
                    .context("ACP resource link requires a file URI")?;
                json!({
                    "type": "resource_link", "uri": uri, "name": self.name,
                    "mimeType": self.format.mime(), "size": self.size,
                })
            }
            Provider::Claude | Provider::Codex => {
                json!({
                    "type": "text",
                    "text": format!("Attachment reference {:?} (local file path): {}", self.name, self.path.display()),
                })
            }
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAttachment {
    pub encoding: NativeAttachmentEncoding,
    pub format: AttachmentFormat,
    /// Frozen bytes for the audit, including for transports using a staged path.
    pub content: String,
    pub path: Option<PathBuf>,
    pub resource_uri: Option<String>,
}

impl TurnAttachment {
    pub(crate) fn block(&self, provider: Provider) -> Result<Value> {
        ensure!(
            self.encoding.supports(self.format),
            "attachment format does not match its encoding"
        );
        let mime = self.format.mime();
        Ok(match (provider, self.encoding) {
            (Provider::Codex, NativeAttachmentEncoding::CodexLocalImage) => {
                let path = self
                    .path
                    .as_ref()
                    .filter(|path| path.is_absolute())
                    .context("Codex local image needs a staged runner path")?;
                json!({"type": "localImage", "path": path})
            }
            (
                Provider::Claude,
                NativeAttachmentEncoding::ClaudeImage | NativeAttachmentEncoding::ClaudeDocument,
            ) => {
                let kind = if self.format.is_image() {
                    "image"
                } else {
                    "document"
                };
                json!({"type":kind, "source": {"type":"base64", "media_type": mime, "data": self.content}})
            }
            (
                Provider::Antigravity,
                NativeAttachmentEncoding::AcpImage | NativeAttachmentEncoding::AcpAudio,
            ) => {
                let kind = if self.format.is_image() {
                    "image"
                } else {
                    "audio"
                };
                json!({"type":kind,"mimeType":mime,"data":self.content})
            }
            (Provider::Antigravity, NativeAttachmentEncoding::AcpResource) => {
                let uri = self
                    .resource_uri
                    .as_ref()
                    .context("ACP resource needs an attachment URI")?;
                let resource = if self.format == AttachmentFormat::Text {
                    json!({"uri":uri,"mimeType":mime,"text":String::from_utf8(STANDARD.decode(&self.content)?)?})
                } else {
                    json!({"uri":uri,"mimeType":mime,"blob":self.content})
                };
                json!({"type":"resource","resource":resource})
            }
            _ => anyhow::bail!("attachment delivery does not match the provider transport"),
        })
    }
}

#[derive(Default)]
pub(crate) struct StagedAttachments {
    locks: Vec<File>,
    directory: Option<Box<dyn AttachmentStaging>>,
    claude_read_directory: Option<PathBuf>,
}

impl StagedAttachments {
    pub(crate) fn active(&self) -> bool {
        self.directory.is_some() || !self.locks.is_empty()
    }

    pub(crate) fn retire(self) -> io::Result<()> {
        let Self {
            directory, locks, ..
        } = self;
        drop(locks);
        directory.map_or(Ok(()), |directory| directory.retire())
    }

    pub(crate) fn configure_claude_read_directory(
        &mut self,
        runner: &dyn Runner,
        scope: &AttachmentRunScope<'_>,
    ) -> Result<PathBuf> {
        let directory = self.directory(runner, scope)?.directory().to_owned();
        self.claude_read_directory = Some(directory.clone());
        Ok(directory)
    }

    pub(crate) fn native(
        &mut self,
        runner: &dyn Runner,
        scope: &AttachmentRunScope<'_>,
        metadata: &AttachmentMetadata,
        content: &str,
        encoding: NativeAttachmentEncoding,
    ) -> Result<TurnAttachment> {
        let path = if encoding == NativeAttachmentEncoding::CodexLocalImage {
            let bytes = STANDARD.decode(content)?;
            ensure!(
                bytes.len() as u64 == metadata.size
                    && format!("{:x}", Sha256::digest(&bytes)) == metadata.sha256,
                "frozen native attachment changed"
            );
            Some(self.stage(runner, scope, metadata, &mut bytes.as_slice())?)
        } else {
            None
        };
        Ok(TurnAttachment {
            encoding,
            format: metadata.format,
            content: content.into(),
            path,
            resource_uri: (encoding == NativeAttachmentEncoding::AcpResource)
                .then(|| format!("urn:omniagent:attachment:{}", metadata.sha256)),
        })
    }

    pub(crate) fn reference(
        &mut self,
        runner: &dyn Runner,
        scope: &AttachmentRunScope<'_>,
        provider: Provider,
        metadata: &AttachmentMetadata,
        captured_path: &Path,
    ) -> Result<AttachmentReference> {
        let mut lock = open_reference(scope.checkout, metadata, captured_path)?;
        let path = match &metadata.source {
            AttachmentSource::Checkout { relative_path } => {
                let path = runner.attachment_reference(
                    scope,
                    CheckoutAttachment {
                        relative_path,
                        size: metadata.size,
                        sha256: &metadata.sha256,
                    },
                )?;
                ensure!(path.is_absolute(), "runner reference must be absolute");
                self.locks.push(lock);
                path
            }
            AttachmentSource::SelectedFile { .. } => {
                if provider == Provider::Claude {
                    ensure!(
                        self.claude_read_directory.is_some(),
                        "Claude has no scoped staging directory in this runner"
                    );
                }
                let path = self.stage(runner, scope, metadata, &mut lock)?;
                if provider == Provider::Claude {
                    ensure!(
                        path.parent() == self.claude_read_directory.as_deref(),
                        "the attachment is outside Claude's staged read directory"
                    );
                }
                path
            }
        };
        let uri = if provider == Provider::Antigravity {
            Some(
                tauri::Url::from_file_path(&path)
                    .map_err(|_| anyhow::anyhow!("runner attachment path has no file URI"))?
                    .into(),
            )
        } else {
            None
        };
        Ok(AttachmentReference {
            name: metadata
                .source
                .file_name()
                .unwrap_or("attachment")
                .to_owned(),
            format: metadata.format,
            size: metadata.size,
            path,
            uri,
        })
    }

    fn stage(
        &mut self,
        runner: &dyn Runner,
        scope: &AttachmentRunScope<'_>,
        metadata: &AttachmentMetadata,
        bytes: &mut dyn Read,
    ) -> Result<PathBuf> {
        let name = format!("{}.{}", uuid::Uuid::new_v4(), metadata.format.extension());
        let directory = self.directory(runner, scope)?;
        directory.stage_read_only(
            StagedAttachmentFile {
                name: &name,
                size: metadata.size,
                sha256: &metadata.sha256,
            },
            &mut bytes.take(metadata.size),
        )?;
        Ok(directory.directory().join(name))
    }

    fn directory(
        &mut self,
        runner: &dyn Runner,
        scope: &AttachmentRunScope<'_>,
    ) -> Result<&mut dyn AttachmentStaging> {
        if self.directory.is_none() {
            let staging = runner.attachment_staging(scope)?;
            ensure!(
                staging.directory().is_absolute(),
                "runner has no absolute read-only attachment staging path"
            );
            self.directory = Some(staging);
        }
        Ok(self
            .directory
            .as_mut()
            .expect("attachment staging exists")
            .as_mut())
    }
}

struct LocalAttachmentStaging {
    locks: Vec<File>,
    canonical: PathBuf,
}

const LOCAL_STAGING_PREFIX: &str = "omniagent-attachments-";
const LOCAL_STAGING_SUFFIX_LEN: usize = 12;

pub(crate) fn local_attachment_staging(
    scope: &AttachmentRunScope<'_>,
) -> io::Result<Box<dyn AttachmentStaging>> {
    let run = uuid::Uuid::parse_str(scope.run_id).map_err(io::Error::other)?;
    let project = uuid::Uuid::parse_str(scope.project_id).map_err(io::Error::other)?;
    let directory = tempfile::Builder::new()
        .prefix(&format!(
            "{LOCAL_STAGING_PREFIX}{}-{}-",
            project.simple(),
            run.simple()
        ))
        .rand_bytes(LOCAL_STAGING_SUFFIX_LEN)
        .tempdir()?;
    LocalAttachmentStaging::keep(directory)
}

impl LocalAttachmentStaging {
    fn keep(directory: tempfile::TempDir) -> io::Result<Box<dyn AttachmentStaging>> {
        let canonical = dunce::canonicalize(directory.path())?;
        let _ = directory.keep();
        Ok(Box::new(LocalAttachmentStaging {
            locks: vec![],
            canonical,
        }))
    }
}

fn remove_local_staging(path: &Path) -> io::Result<()> {
    if plain_metadata(path)?.is_some_and(|metadata| metadata.is_dir()) {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

impl Store {
    /// Called after restart recovery by the single-instance desktop. The local
    /// runner's Windows job handles terminate their processes when the app exits.
    pub async fn retire_local_attachments_after_restart(&self) -> Result<()> {
        self.execute(|db| {
            let root = dunce::canonicalize(std::env::temp_dir())?;
            let mut terminal = db.prepare(
                "SELECT EXISTS(SELECT 1 FROM runs r
                 JOIN checkouts c ON c.id = r.checkout_id
                 JOIN agent_attempts a ON a.run_id = r.id
                 WHERE r.id = ?1 AND c.project_id = ?2
                 AND r.status IN ('succeeded', 'failed', 'cancelled', 'interrupted'))",
            )?;
            for entry in std::fs::read_dir(root)? {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name
                    .to_str()
                    .and_then(|name| name.strip_prefix(LOCAL_STAGING_PREFIX))
                else {
                    continue;
                };
                let Some((project, rest)) = name.split_once('-') else {
                    continue;
                };
                let Some((run, suffix)) = rest.split_once('-') else {
                    continue;
                };
                if project.len() != 32
                    || run.len() != 32
                    || suffix.len() != LOCAL_STAGING_SUFFIX_LEN
                    || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                {
                    continue;
                }
                let (Ok(project), Ok(run)) =
                    (uuid::Uuid::try_parse(project), uuid::Uuid::try_parse(run))
                else {
                    continue;
                };
                if terminal.query_row([run.to_string(), project.to_string()], |row| {
                    row.get::<_, bool>(0)
                })? && let Err(error) = remove_local_staging(&entry.path())
                {
                    eprintln!(
                        "Could not retire local attachments for run {run}: {}",
                        crate::diagnostics::preview(&error.to_string())
                    );
                }
            }
            Ok(())
        })
        .await
    }
}

impl AttachmentStaging for LocalAttachmentStaging {
    fn directory(&self) -> &Path {
        &self.canonical
    }

    fn stage_read_only(
        &mut self,
        file: StagedAttachmentFile<'_>,
        bytes: &mut dyn Read,
    ) -> io::Result<()> {
        let mut components = Path::new(file.name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid staged attachment name",
            ));
        }
        let path = self.canonical.join(file.name);
        {
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .share_mode(0)
                .open(&path)?;
            io::copy(bytes, &mut output)?;
        }
        let mut lock = open_locked_local_file(&path)?;
        let mut digest = Sha256::new();
        if io::copy(&mut lock, &mut digest)? != file.size
            || format!("{:x}", digest.finalize()) != file.sha256
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "staged attachment changed",
            ));
        }
        self.locks.push(lock);
        Ok(())
    }

    fn retire(self: Box<Self>) -> io::Result<()> {
        let Self { locks, canonical } = *self;
        drop(locks);
        remove_local_staging(&canonical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IsolatedAttachmentFixture {
        staging_root: PathBuf,
        checkout: PathBuf,
    }

    impl Runner for IsolatedAttachmentFixture {
        fn identity(&self) -> &str {
            "isolated-attachment-fixture"
        }

        fn launch(
            &self,
            _: crate::runner::LaunchRequest<'_>,
        ) -> io::Result<crate::process_io::CapturedProcess> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "attachment fixture has no CLI",
            ))
        }

        fn attachment_staging(
            &self,
            scope: &AttachmentRunScope<'_>,
        ) -> io::Result<Box<dyn AttachmentStaging>> {
            assert_eq!(
                (scope.project_id, scope.checkout_id),
                ("attachment-project", "attachment-checkout")
            );
            let staging = tempfile::Builder::new()
                .prefix(&format!("{}-", scope.run_id))
                .tempdir_in(&self.staging_root)?;
            LocalAttachmentStaging::keep(staging)
        }

        fn attachment_reference(
            &self,
            scope: &AttachmentRunScope<'_>,
            file: CheckoutAttachment<'_>,
        ) -> io::Result<PathBuf> {
            assert_ne!(scope.checkout, self.checkout);
            let destination = self.checkout.join(file.relative_path);
            let mut input = File::open(&destination)?;
            let mut hash = Sha256::new();
            if io::copy(&mut input, &mut hash)? != file.size
                || format!("{:x}", hash.finalize()) != file.sha256
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the runner checkout revision changed",
                ));
            }
            Ok(destination)
        }
    }

    #[test]
    fn isolated_staging_transfers_only_selected_bytes_and_maps_checkout_references() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        let checkout = root.join("host-checkout");
        let guest_checkout = root.join("runner-checkout");
        let staging_root = root.join("runner-files");
        for directory in [&checkout, &guest_checkout, &staging_root] {
            std::fs::create_dir(directory)?;
        }
        let external = root.join("selected.txt");
        std::fs::write(&external, b"selected attachment")?;
        std::fs::write(root.join("unselected.txt"), b"unselected file")?;
        for directory in [&checkout, &guest_checkout] {
            std::fs::write(directory.join("reference.txt"), b"same repository revision")?;
        }
        let selected = AttachmentPreview::read_with_delivery(
            &checkout,
            AttachmentSource::SelectedFile {
                path: external.clone(),
            },
            AttachmentDelivery::Reference,
        )?
        .capture(&checkout, AttachmentLimits::default())?;
        let reference = AttachmentPreview::read_with_delivery(
            &checkout,
            AttachmentSource::Checkout {
                relative_path: "reference.txt".into(),
            },
            AttachmentDelivery::Reference,
        )?
        .capture(&checkout, AttachmentLimits::default())?;
        let run = uuid::Uuid::new_v4().to_string();
        let scope = AttachmentRunScope {
            project_id: "attachment-project",
            run_id: &run,
            checkout_id: "attachment-checkout",
            checkout: &checkout,
        };
        let runner = IsolatedAttachmentFixture {
            staging_root: staging_root.clone(),
            checkout: guest_checkout.clone(),
        };
        let mut staging = StagedAttachments::default();
        let CapturedPayload::Reference { reference_path } = &reference.payload else {
            anyhow::bail!("expected a repository reference")
        };
        let mapped = staging.reference(
            &runner,
            &scope,
            Provider::Codex,
            &reference.metadata,
            reference_path,
        )?;
        assert_eq!(mapped.path, guest_checkout.join("reference.txt"));
        assert_eq!(std::fs::read_dir(&staging_root)?.count(), 0);
        std::fs::write(&mapped.path, b"different destination revision")?;
        let stale = staging
            .reference(
                &runner,
                &scope,
                Provider::Codex,
                &reference.metadata,
                reference_path,
            )
            .unwrap_err();
        assert!(stale.to_string().contains("checkout revision changed"));

        let CapturedPayload::Reference { reference_path } = &selected.payload else {
            anyhow::bail!("expected a selected file reference")
        };
        let staged = staging.reference(
            &runner,
            &scope,
            Provider::Codex,
            &selected.metadata,
            reference_path,
        )?;
        let directory = staged.path.parent().unwrap();
        assert!(directory.starts_with(&staging_root));
        assert!(
            directory
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&run)
        );
        assert_eq!(std::fs::read(&staged.path)?, b"selected attachment");
        assert_eq!(std::fs::read_dir(directory)?.count(), 1);
        assert!(OpenOptions::new().write(true).open(&staged.path).is_err());
        std::fs::write(external, b"changed after selection")?;
        assert!(
            staging
                .reference(
                    &runner,
                    &scope,
                    Provider::Codex,
                    &selected.metadata,
                    reference_path
                )
                .is_err()
        );
        assert_eq!(std::fs::read_dir(directory)?.count(), 1);
        staging.retire()?;
        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn checkout_reference_rejects_a_junction_added_after_preview() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        let checkout = root.join("checkout");
        let inside = checkout.join("docs");
        let outside = root.join("outside");
        for directory in [&checkout, &inside, &outside] {
            std::fs::create_dir(directory)?;
        }
        for directory in [&inside, &outside] {
            std::fs::write(directory.join("reference.txt"), b"same content")?;
        }
        let source = AttachmentSource::Checkout {
            relative_path: std::path::PathBuf::from("docs").join("reference.txt"),
        };
        let preview = AttachmentPreview::read_with_delivery(
            &checkout,
            source.clone(),
            AttachmentDelivery::Reference,
        )?;
        let captured = preview.capture(&checkout, AttachmentLimits::default())?;
        std::fs::remove_file(inside.join("reference.txt"))?;
        std::fs::remove_dir(&inside)?;
        let junction = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "New-Item -ItemType Junction -Path $env:OMNI_TEST_JUNCTION -Target $env:OMNI_TEST_TARGET -ErrorAction Stop | Out-Null",
            ])
            .env("OMNI_TEST_JUNCTION", &inside)
            .env("OMNI_TEST_TARGET", &outside)
            .output()?;
        ensure!(
            junction.status.success(),
            "could not create the Windows junction fixture: {}",
            String::from_utf8_lossy(&junction.stderr)
        );
        assert_eq!(
            std::fs::read(inside.join("reference.txt"))?,
            b"same content"
        );
        let new_preview =
            AttachmentPreview::read_with_delivery(&checkout, source, AttachmentDelivery::Reference);
        let revalidation = captured.validate_reference(&checkout);
        std::fs::remove_dir(&inside)?;
        assert!(new_preview.is_err());
        assert!(revalidation.is_err());
        assert!(outside.join("reference.txt").is_file());
        Ok(())
    }
}
