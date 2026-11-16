use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
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

mod format;
mod movement;
mod replacement;

use format::{EditorFormat, encode};
pub use movement::RepositoryDocumentMove;
pub(super) use movement::proposed_move_paths;
pub(super) use replacement::lock_directories;
use replacement::{FileSave, file_identity, replace};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryDocument {
    pub revision: RepositoryMarkdownRevision,
    pub canonical_path: PathBuf,
    pub markdown: String,
    pub utf8_bom: bool,
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
        let Ok(raw_markdown) = String::from_utf8(file.bytes) else {
            return Ok(RepositoryDocumentOpen::UnsupportedEncoding);
        };
        if raw_markdown.contains('\0') {
            return Ok(RepositoryDocumentOpen::UnsupportedEncoding);
        }
        let document = remember_document(
            self,
            kind,
            checkout,
            path,
            relative,
            head,
            raw_markdown,
            file.identity,
        )
        .await?;
        Ok(RepositoryDocumentOpen::Document { document })
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
        let (root, max) = editor_checkout(self, &checkout).await?;
        ensure!(
            !canonical_relative.has_root(),
            "save by the checkout-relative document path"
        );
        let relative = relative_name(&root, &root.join(&canonical_relative))?;
        let expected = self
            .checkout_document_revision(checkout.clone(), expected_revision, kind)
            .await?
            .context("the expected file revision is not in this checkout")?;
        ensure!(
            expected.revision.relative_path == relative,
            "the expected revision belongs to another document"
        );
        ensure!(
            !markdown.contains(['\r', '\0']),
            "the editor buffer must be text with LF line endings"
        );
        ensure!(
            markdown.len() as u64 <= max,
            "the editor buffer exceeds the project file limit of {max} bytes"
        );
        let destination = root.join(canonical_relative);
        kind.require_path(&destination)?;
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
        let write = move || {
            let head = base_commit(&root)?;
            let edited = encode(&expected.markdown, &markdown);
            ensure!(
                edited.len() as u64 <= max,
                "the encoded editor buffer exceeds the project file limit of {max} bytes"
            );
            let result = replace(
                &root,
                &destination,
                &expected.revision.sha256,
                &edited,
                max,
                |_| Ok(()),
            )?;
            Ok::<_, anyhow::Error>((head, destination, result))
        };
        // Settle and record the revision even if the caller closes the editor.
        let store = self.clone();
        tokio::spawn(async move {
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
                remember_document(
                    &store,
                    kind,
                    checkout,
                    destination,
                    relative,
                    head,
                    String::from_utf8(file.bytes)?,
                    file.identity,
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
    raw_markdown: String,
    file_identity: String,
) -> Result<RepositoryDocument> {
    let format = EditorFormat::from_raw(&raw_markdown);
    let revision = store
        .remember_editor_document(
            kind,
            checkout,
            relative,
            hash(raw_markdown.as_bytes()),
            head,
            raw_markdown,
        )
        .await?;
    Ok(RepositoryDocument {
        revision,
        canonical_path,
        markdown: format.markdown,
        utf8_bom: format.bom,
        line_endings: format.endings,
        file_identity,
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
