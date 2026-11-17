use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{providers::configuration::open_locked_local_file, store::Store};

use super::{
    RepositoryMarkdownRevision, base_commit, checkout_root, verify_checkout,
    with_managed_worktree_writer,
};

mod extraction;
mod format;
pub(super) mod history;
mod movement;
mod replacement;

use super::markdown::load_document_payload;
pub use extraction::{ExtractRepositoryNote, ExtractedRepositoryNote};
pub(super) use format::RawDocument;
use format::{EditorFormat, encode};
pub use history::{CheckoutDocumentHistoryPage, CheckoutDocumentHistoryText};
pub use movement::RepositoryDocumentMove;
pub(super) use movement::proposed_move_paths;
pub(super) use replacement::lock_directories;
use replacement::{FileSave, replace};
pub(super) use replacement::{file_identity, open_directory_for_rename, open_for_rename, rename};

pub(super) fn replace_reviewed_checkout_bytes(
    root: &Path,
    destination: &Path,
    expected_sha256: &str,
    edited: &[u8],
    max: u64,
    verify: impl Fn() -> Result<()>,
) -> Result<()> {
    use replacement::ReplacementStep;
    verify()?;
    match replace(root, destination, expected_sha256, edited, max, |step| {
        if step == ReplacementStep::BeforeDisplacement {
            verify()?;
        }
        Ok(())
    })? {
        FileSave::Verified { recovery, .. } => {
            if let Some(recovery) = recovery {
                recovery.clean_verified();
            }
            Ok(())
        }
        FileSave::Conflict(recovery) | FileSave::RecoveryRequired(recovery) => {
            let copies = [recovery.displaced_path, recovery.edited_path]
                .into_iter()
                .flatten()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            anyhow::bail!("{}. Recovery copies: {}", recovery.message, copies.join(", "));
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DocumentKind {
    Markdown,
    Source,
}

impl DocumentKind {
    pub(crate) fn contains_path(self, path: &Path) -> bool {
        let markdown = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
            });
        markdown == matches!(self, Self::Markdown)
    }

    pub(crate) fn contains_revision(self, id: &str) -> bool {
        id.starts_with("source-") == matches!(self, Self::Source)
    }

    pub(super) fn editor_prefix(self) -> &'static str {
        match self {
            Self::Markdown => "editor",
            Self::Source => "source",
        }
    }

    fn require_path(self, path: &Path) -> Result<()> {
        ensure!(
            self.contains_path(path),
            match self {
                Self::Markdown => "select a Markdown document",
                Self::Source => "select a source file in the checkout",
            }
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEndings {
    None,
    Lf,
    Crlf,
    Cr,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceFormatChange {
    pub encoding: Option<TextEncoding>,
    pub line_endings: Option<LineEndings>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFormatPreview {
    pub original_encoding: TextEncoding,
    pub original_line_endings: LineEndings,
    pub encoding: TextEncoding,
    pub line_endings: LineEndings,
    pub original_bytes: usize,
    pub converted_bytes: usize,
    pub original_prefix: String,
    pub converted_prefix: String,
    pub sha256: String,
}

enum DocumentEdit {
    Text(String),
    Format {
        change: SourceFormatChange,
        preview_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryDocument {
    pub revision: RepositoryMarkdownRevision,
    pub canonical_path: PathBuf,
    pub markdown: String,
    pub utf8_bom: bool,
    pub encoding: TextEncoding,
    pub line_endings: LineEndings,
    pub file_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RepositoryDocumentOpen {
    Document { document: RepositoryDocument },
    Missing,
    UnsupportedEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RepositoryDocumentSave {
    Saved {
        document: RepositoryDocument,
    },
    Conflict {
        recovery: RepositoryDocumentRecovery,
    },
    RecoveryRequired {
        recovery: RepositoryDocumentRecovery,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryDocumentRecovery {
    pub destination_path: PathBuf,
    pub message: String,
    pub edited_path: Option<PathBuf>,
    pub displaced_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckoutFileText {
    Text { text: String },
    UnsupportedEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryAttachment {
    pub canonical_path: PathBuf,
    pub relative_path: String,
    pub sha256: String,
    pub mime_type: String,
    pub content_base64: String,
}

impl Store {
    pub async fn open_repository_document(
        &self,
        checkout: String,
        selected: PathBuf,
    ) -> Result<RepositoryDocumentOpen> {
        self.open_checkout_document(checkout, selected, DocumentKind::Markdown)
            .await
    }

    pub(super) async fn open_checkout_document(
        &self,
        checkout: String,
        selected: PathBuf,
        kind: DocumentKind,
    ) -> Result<RepositoryDocumentOpen> {
        let (root, max) = editor_checkout(self, &checkout).await?;
        let opened = tokio::task::spawn_blocking(move || {
            let Some(path) = canonical_file(&root, &selected)? else {
                return Ok::<_, anyhow::Error>(None);
            };
            kind.require_path(&path)?;
            let Some(file) = read_file(&path, max)? else {
                return Ok(None);
            };
            let relative = relative_name(&root, &path)?;
            Ok(Some((path, relative, file, base_commit(&root)?)))
        })
        .await??;
        let Some((path, relative, file, head)) = opened else {
            return Ok(RepositoryDocumentOpen::Missing);
        };
        let Ok(raw) = RawDocument::from_bytes(kind, &file.bytes) else {
            return Ok(RepositoryDocumentOpen::UnsupportedEncoding);
        };
        let document = remember_document(
            self,
            kind,
            checkout,
            path,
            relative,
            head,
            file,
            raw.format(),
            None,
        )
        .await?;
        Ok(RepositoryDocumentOpen::Document { document })
    }

    pub async fn read_checkout_file_text(
        &self,
        checkout: String,
        path: String,
    ) -> Result<CheckoutFileText> {
        let (root, max) = editor_checkout(self, &checkout).await?;
        tokio::task::spawn_blocking(move || {
            let path = super::checkout_items::checkout_regular_file(&root, &path)?;
            let parent = path
                .parent()
                .context("The checkout file has no parent folder.")?;
            let _directories = lock_directories(parent)?;
            let file = read_file(&path, max)?.context("The checkout file no longer exists.")?;
            if infer::get(&file.bytes).is_some_and(|kind| {
                let mime = kind.mime_type();
                !mime.starts_with("text/") && !matches!(mime, "application/rtf" | "application/xml" | "image/svg+xml")
            }) {
                return Ok(CheckoutFileText::UnsupportedEncoding);
            }
            let Ok(text) = RawDocument::from_bytes(DocumentKind::Source, &file.bytes) else {
                return Ok(CheckoutFileText::UnsupportedEncoding);
            };
            Ok(CheckoutFileText::Text {
                text: text.into_text(),
            })
        })
        .await?
    }

    pub async fn read_repository_attachment(
        &self,
        checkout: String,
        selected: PathBuf,
    ) -> Result<Option<RepositoryAttachment>> {
        let (root, max) = editor_checkout(self, &checkout).await?;
        tokio::task::spawn_blocking(move || {
            let Some(path) = canonical_file(&root, &selected)? else {
                return Ok(None);
            };
            let Some(file) = read_file(&path, max)? else {
                return Ok(None);
            };
            Ok(Some(RepositoryAttachment {
                relative_path: relative_name(&root, &path)?,
                canonical_path: path,
                sha256: hash(&file.bytes),
                mime_type: infer::get(&file.bytes)
                    .map_or("application/octet-stream", |kind| kind.mime_type())
                    .to_owned(),
                content_base64: STANDARD.encode(&file.bytes),
            }))
        })
        .await?
    }

    pub async fn save_repository_document(
        &self,
        checkout: String,
        canonical_relative: PathBuf,
        expected_revision: String,
        markdown: String,
    ) -> Result<RepositoryDocumentSave> {
        self.save_checkout_document(
            checkout,
            canonical_relative,
            expected_revision,
            markdown,
            DocumentKind::Markdown,
        )
        .await
    }

    pub(super) async fn save_checkout_document(
        &self,
        checkout: String,
        canonical_relative: PathBuf,
        expected_revision: String,
        markdown: String,
        kind: DocumentKind,
    ) -> Result<RepositoryDocumentSave> {
        self.save_checkout_document_edit(
            checkout,
            canonical_relative,
            expected_revision,
            DocumentEdit::Text(markdown),
            kind,
        )
        .await
    }

    pub async fn preview_repository_source_format(
        &self,
        checkout: String,
        path: PathBuf,
        expected_revision: String,
        change: SourceFormatChange,
    ) -> Result<SourceFormatPreview> {
        let (root, max) = editor_checkout(self, &checkout).await?;
        let kind = DocumentKind::Source;
        let relative = document_save_path(&root, &path, kind)?;
        let (expected, original) = self
            .editor_document_payload(&checkout, &relative, expected_revision, kind)
            .await?;
        self.require_no_source_draft(&checkout, &relative).await?;
        tokio::task::spawn_blocking(move || {
            let current =
                read_file(&root.join(path), max)?.context("the source file is missing")?;
            ensure!(
                hash(&current.bytes) == expected.sha256,
                "the file changed; reopen it and review the new bytes"
            );
            let converted = convert_source_format(&original, change, max)?;
            let original_format = RawDocument::from_bytes(kind, &original)?.format();
            let format = RawDocument::from_bytes(kind, &converted)?.format();
            let prefix = |bytes: &[u8]| {
                bytes
                    .iter()
                    .take(24)
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            Ok(SourceFormatPreview {
                original_encoding: original_format.encoding,
                original_line_endings: original_format.endings,
                encoding: format.encoding,
                line_endings: format.endings,
                original_bytes: original.len(),
                converted_bytes: converted.len(),
                original_prefix: prefix(&original),
                converted_prefix: prefix(&converted),
                sha256: hash(&converted),
            })
        })
        .await?
    }

    pub async fn save_repository_source_format(
        &self,
        checkout: String,
        path: PathBuf,
        expected_revision: String,
        change: SourceFormatChange,
        preview_sha256: String,
    ) -> Result<RepositoryDocumentSave> {
        self.save_checkout_document_edit(
            checkout,
            path,
            expected_revision,
            DocumentEdit::Format {
                change,
                preview_sha256,
            },
            DocumentKind::Source,
        )
        .await
    }

    async fn editor_document_payload(
        &self,
        checkout: &str,
        relative: &str,
        expected_revision: String,
        kind: DocumentKind,
    ) -> Result<(RepositoryMarkdownRevision, Vec<u8>)> {
        ensure!(
            kind.contains_revision(&expected_revision),
            "the expected file revision is not in this checkout"
        );
        let checkout = checkout.to_owned();
        let (expected, original) = self
            .execute(move |db| load_document_payload(db, &checkout, &expected_revision))
            .await?
            .context("the expected file revision is not in this checkout")?;
        ensure!(
            expected.relative_path == relative,
            "the expected revision belongs to another document"
        );
        ensure!(
            hash(&original) == expected.sha256,
            "the stored file revision does not match its checksum"
        );
        Ok((expected, original))
    }

    async fn require_no_source_draft(&self, checkout: &str, relative: &str) -> Result<()> {
        ensure!(
            self.repository_source_draft(checkout.to_owned(), relative.to_owned())
                .await?
                .is_none(),
            "save or discard the source draft before converting its file format"
        );
        Ok(())
    }

    async fn save_checkout_document_edit(
        &self,
        checkout: String,
        canonical_relative: PathBuf,
        expected_revision: String,
        edit: DocumentEdit,
        kind: DocumentKind,
    ) -> Result<RepositoryDocumentSave> {
        let (root, max) = editor_checkout(self, &checkout).await?;
        let relative = document_save_path(&root, &canonical_relative, kind)?;
        let (expected, original) = self
            .editor_document_payload(&checkout, &relative, expected_revision, kind)
            .await?;
        let destination = root.join(canonical_relative);
        let converting = matches!(edit, DocumentEdit::Format { .. });
        let managed = self
            .execute({
                let checkout = checkout.clone();
                move |db| {
                    Ok(db.query_row(
                    "SELECT project_id, task_id FROM managed_task_worktrees WHERE checkout_id = ?1",
                    [checkout],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ).optional()?)
                }
            })
            .await?;
        let before_save = expected.clone();
        let write = move || {
            let head = base_commit(&root)?;
            let edited = match edit {
                DocumentEdit::Text(markdown) => {
                    ensure!(
                        !markdown.contains(['\r', '\0']),
                        "the editor buffer must be text with LF line endings"
                    );
                    ensure!(
                        markdown.len() as u64 <= max,
                        "the editor buffer exceeds the project file limit of {max} bytes"
                    );
                    RawDocument::from_bytes(kind, &original)?.encode_edit(&markdown)
                }
                DocumentEdit::Format {
                    change,
                    preview_sha256,
                } => {
                    let converted = convert_source_format(&original, change, max)?;
                    ensure!(
                        hash(&converted) == preview_sha256,
                        "the conversion no longer matches the reviewed bytes"
                    );
                    converted
                }
            };
            ensure!(
                edited.len() as u64 <= max,
                "the encoded editor buffer exceeds the project file limit of {max} bytes"
            );
            let result = replace(&root, &destination, &expected.sha256, &edited, max, |_| {
                Ok(())
            })?;
            Ok::<_, anyhow::Error>((head, destination, result))
        };
        // Settle and record the revision even if the caller closes the editor.
        let store = self.clone();
        tokio::spawn(async move {
            let lock = {
                let mut locks = store
                    .document_write_locks
                    .lock()
                    .expect("editor write locks");
                locks.retain(|_, lock| lock.strong_count() > 0);
                let existing = locks
                    .entry((checkout.clone(), relative.clone()))
                    .or_default();
                existing.upgrade().unwrap_or_else(|| {
                    let lock = Arc::default();
                    *existing = Arc::downgrade(&lock);
                    lock
                })
            };
            let _guard = lock.lock().await;
            if converting {
                store.require_no_source_draft(&checkout, &relative).await?;
            }
            let (head, destination, result) = if let Some((project, owner)) = managed {
                with_managed_worktree_writer(&store, project, checkout.clone(), owner, move |_| {
                    write()
                })
                .await?
            } else {
                tokio::task::spawn_blocking(write).await??
            };
            let (file, recovery) = match result {
                FileSave::Conflict(recovery) => {
                    return Ok(RepositoryDocumentSave::Conflict { recovery });
                }
                FileSave::RecoveryRequired(recovery) => {
                    return Ok(RepositoryDocumentSave::RecoveryRequired { recovery });
                }
                FileSave::Verified { file, recovery } => (file, recovery),
            };
            let result = async {
                let format = RawDocument::from_bytes(kind, &file.bytes)?.format();
                remember_document(
                    &store,
                    kind,
                    checkout,
                    destination,
                    relative,
                    head,
                    file,
                    format,
                    Some(before_save),
                )
                .await
            }
            .await;
            match (result, recovery) {
                (Ok(document), recovery) => {
                    if let Some(recovery) = recovery {
                        recovery.clean_verified();
                    }
                    Ok(RepositoryDocumentSave::Saved { document })
                }
                (Err(error), Some(recovery)) => Ok(RepositoryDocumentSave::RecoveryRequired {
                    recovery: recovery
                        .describe(format!("could not record the saved revision: {error}")),
                }),
                (Err(error), None) => Err(error),
            }
        })
        .await?
    }
}

fn document_save_path(root: &Path, path: &Path, kind: DocumentKind) -> Result<String> {
    ensure!(
        !path.has_root(),
        "save by the checkout-relative document path"
    );
    kind.require_path(path)?;
    relative_name(root, &root.join(path))
}

fn convert_source_format(original: &[u8], change: SourceFormatChange, max: u64) -> Result<Vec<u8>> {
    let converted = RawDocument::from_bytes(DocumentKind::Source, original)?.convert(change)?;
    ensure!(
        converted != original,
        "the source file already uses this format"
    );
    ensure!(
        converted.len() as u64 <= max,
        "the encoded editor buffer exceeds the project file limit of {max} bytes"
    );
    Ok(converted)
}

async fn editor_checkout(store: &Store, checkout: &str) -> Result<(PathBuf, u64)> {
    let root = checkout_root(store, checkout.to_owned()).await?;
    let layout = verify_checkout(store, checkout.to_owned(), root).await?;
    let max = store
        .checkout_attachment_limits(checkout.to_owned())
        .await?
        .bytes_per_file;
    Ok((layout.root, max))
}

async fn remember_document(
    store: &Store,
    kind: DocumentKind,
    checkout: String,
    canonical_path: PathBuf,
    relative: String,
    head: Option<String>,
    file: OpenedFile,
    format: EditorFormat,
    before_save: Option<RepositoryMarkdownRevision>,
) -> Result<RepositoryDocument> {
    let revision = store
        .remember_editor_document(
            kind,
            checkout,
            relative,
            hash(&file.bytes),
            head,
            file.bytes,
            before_save,
        )
        .await?;
    Ok(RepositoryDocument {
        revision,
        canonical_path,
        markdown: format.markdown,
        utf8_bom: format.encoding == TextEncoding::Utf8Bom,
        encoding: format.encoding,
        line_endings: format.endings,
        file_identity: file.identity,
    })
}

fn canonical_file(root: &Path, selected: &Path) -> Result<Option<PathBuf>> {
    let selected = if selected.is_absolute() {
        dunce::simplified(selected).to_owned()
    } else {
        relative_name(root, &root.join(selected))?;
        root.join(selected)
    };
    relative_name(root, &selected)?;
    match dunce::canonicalize(selected) {
        Ok(canonical) => {
            relative_name(root, &canonical)
                .context("the resolved file is outside the selected repository checkout")?;
            Ok(Some(canonical))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn relative_name(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .context("select a file inside the current checkout")?;
    let name = relative
        .to_str()
        .context("the repository file path is not UTF-8")?;
    ensure!(
        !name.is_empty()
            && !name.contains(':')
            && relative.components().all(|component| {
                matches!(component, Component::Normal(part) if !part.eq_ignore_ascii_case(".git"))
            }),
        "select a file inside the current checkout"
    );
    Ok(name.replace('\\', "/"))
}

struct OpenedFile {
    bytes: Vec<u8>,
    identity: String,
}

fn read_file(path: &Path, max: u64) -> Result<Option<OpenedFile>> {
    match open_locked_local_file(path) {
        Ok(file) => read_open_file(file, max).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_open_file(file: fs::File, max: u64) -> Result<OpenedFile> {
    let identity = file_identity(&file)?;
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max,
        "the repository file exceeds the project file limit of {max} bytes"
    );
    Ok(OpenedFile { bytes, identity })
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
