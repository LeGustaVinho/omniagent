use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde::Serialize;

use super::{
    DocumentKind, RepositoryDocument, RepositoryDocumentRecovery, Store, base_commit,
    editor_checkout, relative_name, remember_document,
    replacement::{FileMove, move_exact},
    with_managed_worktree_writer,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RepositoryDocumentMove {
    Moved {
        document: RepositoryDocument,
    },
    Conflict {
        message: String,
    },
    RecoveryRequired {
        recovery: RepositoryDocumentRecovery,
    },
}

pub(in crate::repository) fn proposed_move_paths(
    root: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(String, String)> {
    let source = move_path(root, source)?;
    let destination = move_path(root, destination)?;
    ensure!(
        source != destination,
        "the rename destination must differ from the source path"
    );
    Ok((source, destination))
}

fn move_path(root: &Path, relative: &Path) -> Result<String> {
    ensure!(!relative.has_root(), "rename by checkout-relative paths");
    let path = root.join(relative);
    relative_name(root, &path)?;
    DocumentKind::Markdown.require_path(&path)?;
    let parent = path
        .parent()
        .context("the Markdown move path has no parent directory")?;
    let parent: PathBuf = parent.components().collect();
    let physical = dunce::canonicalize(&parent)
        .context("the Markdown move parent directory is unavailable")?;
    ensure!(
        physical.starts_with(root),
        "the Markdown move path resolves outside its checkout"
    );
    ensure!(
        physical.to_string_lossy().to_lowercase() == parent.to_string_lossy().to_lowercase(),
        "the Markdown move parent must be a physical checkout directory"
    );
    let name = path
        .file_name()
        .context("the Markdown move path has no file name")?;
    relative_name(root, &physical.join(name))
}

impl Store {
    pub async fn move_repository_document(
        &self,
        checkout: String,
        source: PathBuf,
        destination: PathBuf,
        expected_revision: String,
        expected_file_identity: String,
    ) -> Result<RepositoryDocumentMove> {
        let (root, max) = editor_checkout(self, &checkout).await?;
        let (source, destination) = proposed_move_paths(&root, &source, &destination)?;
        let expected = self
            .repository_markdown(checkout.clone(), expected_revision)
            .await?
            .context("the expected Markdown revision is not in this checkout")?;
        ensure!(
            expected.revision.relative_path == source,
            "the expected revision belongs to another Markdown document"
        );
        let managed = self
            .execute({
                let checkout = checkout.clone();
                move |db| {
                    Ok(db.query_row(
                "SELECT project_id, task_id FROM managed_task_worktrees WHERE checkout_id = ?1",
                [checkout], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ).optional()?)
                }
            })
            .await?;
        let write = move || {
            let head = base_commit(&root)?;
            if head != expected.revision.base_commit {
                return Ok::<_, anyhow::Error>((
                    head,
                    root.join(&destination),
                    destination,
                    FileMove::Conflict("the checkout base changed after the rename preview".into()),
                ));
            }
            let destination_path = root.join(&destination);
            let result = move_exact(
                &root,
                &root.join(source),
                &destination_path,
                &expected.revision.sha256,
                &expected_file_identity,
                max,
            )?;
            Ok((head, destination_path, destination, result))
        };
        let store = self.clone();
        tokio::spawn(async move {
            let (head, destination_path, destination, result) = if let Some((project, task)) =
                managed
            {
                with_managed_worktree_writer(&store, project, checkout.clone(), task, move |_| {
                    write()
                })
                .await?
            } else {
                tokio::task::spawn_blocking(write).await??
            };
            let file = match result {
                FileMove::Moved(file) => file,
                FileMove::Conflict(message) => {
                    return Ok(RepositoryDocumentMove::Conflict { message });
                }
            };
            let format =
                super::RawDocument::from_bytes(DocumentKind::Markdown, &file.bytes)?.format();
            match remember_document(
                &store,
                DocumentKind::Markdown,
                checkout,
                destination_path.clone(),
                destination,
                head,
                file,
                format,
                None,
            )
            .await
            {
                Ok(document) => Ok(RepositoryDocumentMove::Moved { document }),
                Err(error) => Ok(RepositoryDocumentMove::RecoveryRequired {
                    recovery: RepositoryDocumentRecovery {
                        destination_path,
                        message: format!(
                            "the file moved, but its new revision could not be recorded: {error:#}"
                        ),
                        edited_path: None,
                        displaced_path: None,
                    },
                }),
            }
        })
        .await?
    }
}
