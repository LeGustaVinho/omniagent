use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::{
    providers::{
        Provider,
        configuration::{open_verified_local_file, plain_metadata},
    },
    runner::Runner,
};

const FILE_LIMIT: u64 = 20 * 1024 * 1024;
const TURN_LIMIT: u64 = 50 * 1024 * 1024;
const COUNT_LIMIT: usize = 10;

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

    fn detect(bytes: &[u8]) -> Self {
        match infer::get(bytes).map(|kind| kind.mime_type()) {
            Some("image/png") => Self::Png,
            Some("image/jpeg") => Self::Jpeg,
            Some("image/gif") => Self::Gif,
            Some("image/webp") => Self::Webp,
            Some("application/pdf") => Self::Pdf,
            Some("audio/wav" | "audio/x-wav") => Self::Wav,
            Some("audio/mpeg") => Self::Mp3,
            None if !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() => Self::Text,
            _ => Self::Unknown,
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

pub struct AttachmentPreview {
    metadata: AttachmentMetadata,
    canonical: PathBuf,
    checkout: PathBuf,
}

impl AttachmentPreview {
    pub fn metadata(&self) -> &AttachmentMetadata {
        &self.metadata
    }
    /// An external path must come from the application's individual file selection.
    pub fn read(checkout: &Path, source: AttachmentSource) -> Result<Self> {
        let canonical = authorized_path(checkout, &source)?;
        let bytes = read_file(&canonical)?;
        Ok(Self {
            metadata: metadata(source, &bytes),
            canonical,
            checkout: checkout.into(),
        })
    }

    fn capture(self, checkout: &Path) -> Result<CapturedAttachment> {
        ensure!(
            self.checkout == checkout,
            "attachment preview belongs to another checkout"
        );
        let path = authorized_path(checkout, &self.metadata.source)?;
        ensure!(
            path == self.canonical,
            "attachment path changed after preview"
        );
        let bytes = read_file(&path)?;
        let observed = metadata(self.metadata.source.clone(), &bytes);
        ensure!(
            observed == self.metadata,
            "attachment changed after preview"
        );
        Ok(CapturedAttachment {
            metadata: observed,
            content: STANDARD.encode(bytes),
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

fn read_file(path: &Path) -> Result<Vec<u8>> {
    let file = open_verified_local_file(path)?;
    ensure!(
        file.metadata()?.len() <= FILE_LIMIT,
        "attachment exceeds the 20 MiB native file limit"
    );
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= FILE_LIMIT,
        "attachment exceeds the 20 MiB native file limit"
    );
    Ok(bytes)
}

fn metadata(source: AttachmentSource, bytes: &[u8]) -> AttachmentMetadata {
    AttachmentMetadata {
        source,
        format: AttachmentFormat::detect(bytes),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size: bytes.len() as u64,
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct CapturedAttachment {
    pub metadata: AttachmentMetadata,
    content: String,
}

pub(crate) fn capture(
    checkout: &Path,
    previews: Vec<AttachmentPreview>,
) -> Result<Vec<CapturedAttachment>> {
    check_limits(previews.iter().map(|file| file.metadata.size))?;
    previews
        .into_iter()
        .map(|preview| preview.capture(checkout))
        .collect()
}

pub(crate) fn check_limits(sizes: impl IntoIterator<Item = u64>) -> Result<()> {
    let mut total = 0;
    for (index, size) in sizes.into_iter().enumerate() {
        ensure!(index < COUNT_LIMIT, "turn exceeds the 10-attachment limit");
        ensure!(
            size <= FILE_LIMIT,
            "attachment exceeds the 20 MiB native file limit"
        );
        total += size;
        ensure!(
            total <= TURN_LIMIT,
            "turn exceeds the 50 MiB attachment limit"
        );
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
            Self::ClaudeDocument | Self::AcpResource => format == AttachmentFormat::Pdf,
            Self::AcpAudio => format.is_audio(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDispatch {
    pub message_revision_id: String,
    pub metadata: AttachmentMetadata,
    pub qualification_id: String,
    pub input: TurnAttachment,
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
                json!({"type":"resource","resource":{"uri":uri,"mimeType":mime,"blob":self.content}})
            }
            _ => anyhow::bail!("attachment delivery does not match the provider transport"),
        })
    }
}

#[derive(Default)]
pub(crate) struct StagedAttachments {
    locks: Vec<File>,
    directory: Option<tempfile::TempDir>,
}

impl StagedAttachments {
    pub(crate) fn materialize(
        &mut self,
        runner: &dyn Runner,
        captured: &CapturedAttachment,
        encoding: NativeAttachmentEncoding,
    ) -> Result<TurnAttachment> {
        let path = if encoding == NativeAttachmentEncoding::CodexLocalImage {
            if self.directory.is_none() {
                self.directory = Some(
                    tempfile::Builder::new()
                        .prefix("omniagent-attachments-")
                        .tempdir()?,
                );
            }
            let directory = self.directory.as_ref().expect("attachment staging exists");
            let path = dunce::canonicalize(directory.path())?.join(format!(
                "{}.{}",
                uuid::Uuid::new_v4(),
                captured.metadata.format.extension()
            ));
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?
                .write_all(&STANDARD.decode(&captured.content)?)?;
            let lock = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&path)?;
            let mut digest = Sha256::new();
            std::io::copy(&mut &lock, &mut digest)?;
            ensure!(
                format!("{:x}", digest.finalize()) == captured.metadata.sha256,
                "staged attachment changed"
            );
            self.locks.push(lock);
            Some(
                runner
                    .attachment_path(&path)
                    .context("runner has no read-only attachment path mapping")?,
            )
        } else {
            None
        };
        Ok(TurnAttachment {
            encoding,
            format: captured.metadata.format,
            content: captured.content.clone(),
            path,
            resource_uri: (encoding == NativeAttachmentEncoding::AcpResource)
                .then(|| format!("urn:omniagent:attachment:{}", captured.metadata.sha256)),
        })
    }
}
