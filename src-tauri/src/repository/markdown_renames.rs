use std::{fs, io, path::PathBuf};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{protection, store::Store};

use super::{
    checkout_root,
    documents::{
        RepositoryDocument, RepositoryDocumentMove, RepositoryDocumentOpen,
        RepositoryDocumentRecovery, RepositoryDocumentSave, proposed_move_paths,
    },
    markdown_catalog::note_paths,
    verify_checkout,
};

mod links;
pub use links::{
    replace_markdown_note_reference, replace_wiki_note_reference, replace_wiki_note_reference_at,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownRenamePreview {
    pub id: String,
    pub checkout_id: String,
    pub source: String,
    pub destination: String,
    pub source_revision_id: String,
    pub files: Vec<MarkdownRenameFile>,
    pub warnings: Vec<MarkdownRenameWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownRenameFile {
    pub path: String,
    pub revision_id: String,
    pub edits: Vec<MarkdownRenameEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownRenameEdit {
    pub byte_start: usize,
    pub byte_end: usize,
    pub line: usize,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownRenameWarning {
    pub path: String,
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownRenameReport {
    pub preview_id: String,
    pub source: String,
    pub destination: String,
    pub outcome: MarkdownRenameOutcome,
    pub partial: bool,
    pub files: Vec<MarkdownRenameFileResult>,
    pub warnings: Vec<MarkdownRenameWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MarkdownRenameOutcome {
    Moved {
        revision_id: String,
    },
    Conflict {
        message: String,
    },
    RecoveryRequired {
        recovery: RepositoryDocumentRecovery,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MarkdownRenameFileResult {
    Rewritten {
        path: String,
        revision_id: String,
        count: usize,
    },
    Changed {
        path: String,
        message: String,
        recovery: Option<RepositoryDocumentRecovery>,
    },
    RecoveryRequired {
        path: String,
        recovery: RepositoryDocumentRecovery,
    },
    Failed {
        path: String,
        message: String,
    },
}

#[derive(Serialize, Deserialize)]
struct RenamePlan {
    preview: MarkdownRenamePreview,
    source_identity: String,
    files: Vec<FileIdentity>,
}

#[derive(Serialize, Deserialize)]
struct FileIdentity {
    path: String,
    identity: String,
}

enum Claim {
    Pending(RenamePlan),
    Settled(MarkdownRenameReport),
}

impl Store {
    pub async fn preview_markdown_rename(
        &self,
        checkout: String,
        source: PathBuf,
        destination: PathBuf,
        expected_source_revision: String,
    ) -> Result<MarkdownRenamePreview> {
        let root = checkout_root(self, checkout.clone()).await?;
        let root = verify_checkout(self, checkout.clone(), root).await?.root;
        let (source, destination, paths) = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || {
                let (source, destination) = proposed_move_paths(&root, &source, &destination)?;
                match fs::symlink_metadata(root.join(&destination)) {
                    Ok(_) => bail!("the Markdown rename destination already exists"),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                Ok::<_, anyhow::Error>((source, destination, note_paths(&root)?))
            })
            .await??
        };
        let RepositoryDocumentOpen::Document { document } = self
            .open_repository_document(checkout.clone(), source.clone().into())
            .await?
        else {
            bail!("the selected Markdown document cannot be opened");
        };
        ensure!(
            document.revision.relative_path.to_lowercase() == source.to_lowercase(),
            "the source is not the physical checkout path"
        );
        ensure!(
            document.revision.id == expected_source_revision,
            "the Markdown rename source changed before preview"
        );
        let source = document.revision.relative_path.clone();
        let source_identity = document.file_identity.clone();
        let mut documents = vec![document];
        let mut warnings = Vec::new();
        for path in paths.into_iter().filter(|path| path != &source) {
            match self
                .open_repository_document(checkout.clone(), path.clone().into())
                .await
            {
                Ok(RepositoryDocumentOpen::Document { document })
                    if document.revision.relative_path == path =>
                {
                    documents.push(document)
                }
                Ok(_) => warnings.push(MarkdownRenameWarning {
                    path,
                    line: None,
                    message: "the note could not be safely read for this preview".into(),
                }),
                Err(error) => warnings.push(MarkdownRenameWarning {
                    path,
                    line: None,
                    message: format!("could not read the note: {error:#}"),
                }),
            }
        }
        documents.sort_unstable_by(|left, right| {
            left.revision
                .relative_path
                .cmp(&right.revision.relative_path)
        });
        let (files, identities, warnings) = {
            let source = source.clone();
            let destination = destination.clone();
            tokio::task::spawn_blocking(move || {
                let (files, identities) = links::planned_references(
                    &root,
                    &source,
                    &destination,
                    &documents,
                    &mut warnings,
                );
                (files, identities, warnings)
            })
            .await?
        };
        let preview = MarkdownRenamePreview {
            id: Uuid::new_v4().to_string(),
            checkout_id: checkout.clone(),
            source,
            destination,
            source_revision_id: expected_source_revision,
            files,
            warnings,
        };
        let plan = RenamePlan {
            preview: preview.clone(),
            source_identity,
            files: identities,
        };
        let payload = seal(&plan)?;
        self.execute(move |db| {
            db.execute("INSERT INTO markdown_rename_previews(id, checkout_id, payload, state) VALUES (?1, ?2, ?3, 'pending')",
                params![plan.preview.id, checkout, payload])?;
            Ok(())
        }).await?;
        Ok(preview)
    }

    pub async fn apply_markdown_rename(
        &self,
        checkout: String,
        preview_id: String,
    ) -> Result<MarkdownRenameReport> {
        let store = self.clone();
        tokio::spawn(async move {
            let plan = match store.claim_markdown_rename(&checkout, &preview_id).await? {
                Claim::Pending(plan) => plan,
                Claim::Settled(report) => return Ok(report),
            };
            let report = apply_plan(&store, checkout.clone(), plan).await;
            let cipher = seal(&report)?;
            store.execute(move |db| {
                let changed = db.execute(
                    "UPDATE markdown_rename_previews SET state = 'settled', report = ?3 WHERE id = ?1 AND checkout_id = ?2 AND state = 'applying'",
                    params![preview_id, checkout, cipher],
                )?;
                ensure!(changed == 1, "the Markdown rename receipt could not be recorded");
                Ok(())
            }).await?;
            Ok(report)
        }).await?
    }

    async fn claim_markdown_rename(&self, checkout: &str, preview_id: &str) -> Result<Claim> {
        let (checkout, preview_id) = (checkout.to_owned(), preview_id.to_owned());
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let state = tx.query_row(
                "SELECT state, payload, report FROM markdown_rename_previews WHERE id = ?1 AND checkout_id = ?2",
                params![preview_id, checkout],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, Option<Vec<u8>>>(2)?)),
            ).optional()?.context("the rename preview is unavailable in this checkout")?;
            match state.0.as_str() {
                "settled" => Ok(Claim::Settled(unseal(&state.2.context("the rename receipt is missing")?)?)),
                "applying" => bail!("the rename preview was claimed already; check the checkout before making a new preview"),
                "pending" => {
                    let plan: RenamePlan = unseal(&state.1)?;
                    ensure!(plan.preview.id == preview_id && plan.preview.checkout_id == checkout, "the rename preview identity does not match");
                    tx.execute("UPDATE markdown_rename_previews SET state = 'applying' WHERE id = ?1 AND checkout_id = ?2", params![preview_id, checkout])?;
                    tx.commit()?;
                    Ok(Claim::Pending(plan))
                }
                _ => bail!("the rename preview has an unknown status"),
            }
        }).await
    }
}

async fn apply_plan(store: &Store, checkout: String, plan: RenamePlan) -> MarkdownRenameReport {
    let RenamePlan {
        preview,
        source_identity,
        files,
    } = plan;
    let mut report = MarkdownRenameReport {
        preview_id: preview.id,
        source: preview.source,
        destination: preview.destination,
        outcome: MarkdownRenameOutcome::Failed {
            message: "the document was not moved".into(),
        },
        partial: false,
        files: Vec::new(),
        warnings: preview.warnings,
    };
    let moved = store
        .move_repository_document(
            checkout.clone(),
            report.source.clone().into(),
            report.destination.clone().into(),
            preview.source_revision_id,
            source_identity,
        )
        .await;
    let document = match moved {
        Ok(RepositoryDocumentMove::Moved { document }) => document,
        other => {
            report.outcome = match other {
                Ok(RepositoryDocumentMove::Conflict { message }) => {
                    MarkdownRenameOutcome::Conflict { message }
                }
                Ok(RepositoryDocumentMove::RecoveryRequired { recovery }) => {
                    MarkdownRenameOutcome::RecoveryRequired { recovery }
                }
                Err(error) => MarkdownRenameOutcome::Failed {
                    message: format!("{error:#}"),
                },
                Ok(RepositoryDocumentMove::Moved { .. }) => unreachable!(),
            };
            report.partial = matches!(
                report.outcome,
                MarkdownRenameOutcome::RecoveryRequired { .. }
            );
            return report;
        }
    };
    let mut final_revision = document.revision.id.clone();
    for file in preview.files {
        let Some(identity) = files.iter().find(|identity| identity.path == file.path) else {
            report.files.push(MarkdownRenameFileResult::Failed {
                path: file.path,
                message: "the reviewed file identity is unavailable".into(),
            });
            continue;
        };
        let moved_file = file.path == report.source;
        let (path, revision) = if moved_file {
            (report.destination.clone(), document.revision.id.clone())
        } else {
            (file.path, file.revision_id)
        };
        let outcome = rewrite_checked(
            store,
            &checkout,
            path,
            revision,
            &identity.identity,
            &file.edits,
        )
        .await;
        if moved_file && let MarkdownRenameFileResult::Rewritten { revision_id, .. } = &outcome {
            final_revision.clone_from(revision_id);
        }
        report.files.push(outcome);
    }
    report.partial = !report.warnings.is_empty()
        || report
            .files
            .iter()
            .any(|result| !matches!(result, MarkdownRenameFileResult::Rewritten { .. }));
    report.outcome = MarkdownRenameOutcome::Moved {
        revision_id: final_revision,
    };
    report
}

async fn rewrite_checked(
    store: &Store,
    checkout: &str,
    path: String,
    revision: String,
    identity: &str,
    edits: &[MarkdownRenameEdit],
) -> MarkdownRenameFileResult {
    let changed = |message: &str| MarkdownRenameFileResult::Changed {
        path: path.clone(),
        message: message.into(),
        recovery: None,
    };
    let failed = |error: anyhow::Error| MarkdownRenameFileResult::Failed {
        path: path.clone(),
        message: format!("{error:#}"),
    };
    let current = match store
        .open_repository_document(checkout.into(), path.clone().into())
        .await
    {
        Ok(RepositoryDocumentOpen::Document { document }) => document,
        Ok(_) => return changed("the reviewed note was removed or is no longer readable"),
        Err(error) => return failed(error),
    };
    if current.revision.id != revision
        || current.revision.relative_path != path
        || current.file_identity != identity
    {
        return changed("the note changed after the rename preview");
    }
    let Some(markdown) = replace_ranges(current.markdown, edits) else {
        return changed("the reviewed ranges do not match the note");
    };
    match store
        .save_repository_document(checkout.into(), path.clone().into(), revision, markdown)
        .await
    {
        Ok(RepositoryDocumentSave::Saved { document }) => MarkdownRenameFileResult::Rewritten {
            path,
            revision_id: document.revision.id,
            count: edits.len(),
        },
        Ok(RepositoryDocumentSave::Conflict { recovery }) => MarkdownRenameFileResult::Changed {
            path,
            message: recovery.message.clone(),
            recovery: Some(recovery),
        },
        Ok(RepositoryDocumentSave::RecoveryRequired { recovery }) => {
            MarkdownRenameFileResult::RecoveryRequired { path, recovery }
        }
        Err(error) => failed(error),
    }
}

fn replace_ranges(mut text: String, edits: &[MarkdownRenameEdit]) -> Option<String> {
    let mut right = text.len();
    for edit in edits.iter().rev() {
        let range = edit.byte_start..edit.byte_end;
        if range.is_empty() || range.end > right || text.get(range.clone())? != edit.before {
            return None;
        }
        text.replace_range(range, &edit.after);
        right = edit.byte_start;
    }
    Some(text)
}

fn seal<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn unseal<T: DeserializeOwned>(cipher: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(cipher)?)?)
}

#[cfg(test)]
mod tests;
