use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    protection,
    repository::{RepositoryMarkdownRevision, documents::DocumentKind},
    store::Store,
};

mod watcher;
pub use watcher::{RepositoryMarkdownInvalidation, RepositoryMarkdownWatch};

#[cfg(test)]
mod tests;

const PAYLOAD_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryMarkdownDraft {
    pub base: RepositoryMarkdownRevision,
    pub revision: String,
    pub markdown: String,
}

#[derive(Serialize, Deserialize)]
struct DraftPayload {
    version: u8,
    base_revision: String,
    revision: String,
    markdown: String,
}

impl Store {
    pub async fn save_repository_markdown_draft(
        &self,
        checkout_id: String,
        base_revision: String,
        expected_draft_revision: Option<String>,
        markdown: String,
    ) -> Result<RepositoryMarkdownDraft> {
        self.save_checkout_file_draft(
            checkout_id,
            base_revision,
            expected_draft_revision,
            markdown,
            DocumentKind::Markdown,
        )
        .await
    }

    pub(crate) async fn save_checkout_file_draft(
        &self,
        checkout_id: String,
        base_revision: String,
        expected_draft_revision: Option<String>,
        markdown: String,
        kind: DocumentKind,
    ) -> Result<RepositoryMarkdownDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let base = stored_revision(&tx, &checkout_id, &base_revision, kind)?;
            let current = load_payload(&tx, &checkout_id, &base.relative_path)?;
            ensure!(
                current.as_ref().map(|draft| draft.revision.as_str())
                    == expected_draft_revision.as_deref(),
                "The file draft changed before the edit was saved"
            );
            if let Some(current) = current
                && current.base_revision == base_revision
                && current.markdown == markdown
            {
                return Ok(RepositoryMarkdownDraft {
                    base,
                    revision: current.revision,
                    markdown,
                });
            }
            let draft = DraftPayload {
                version: PAYLOAD_VERSION,
                base_revision,
                revision: Uuid::new_v4().to_string(),
                markdown,
            };
            let protected = protection::protect(&serde_json::to_vec(&draft)?)?;
            tx.execute(
                "INSERT INTO drafts(kind, owner_id, document, payload)
                 VALUES ('checkout_file', ?1, ?2, ?3)
                 ON CONFLICT(kind, owner_id, document) DO UPDATE
                 SET payload = excluded.payload, updated_at = unixepoch()",
                params![checkout_id, base.relative_path, protected],
            )?;
            tx.commit()?;
            Ok(RepositoryMarkdownDraft {
                base,
                revision: draft.revision,
                markdown: draft.markdown,
            })
        })
        .await
    }

    pub async fn repository_markdown_draft(
        &self,
        checkout_id: String,
        canonical_relative: String,
    ) -> Result<Option<RepositoryMarkdownDraft>> {
        self.checkout_file_draft(checkout_id, canonical_relative, DocumentKind::Markdown)
            .await
    }

    pub(crate) async fn checkout_file_draft(
        &self,
        checkout_id: String,
        canonical_relative: String,
        kind: DocumentKind,
    ) -> Result<Option<RepositoryMarkdownDraft>> {
        if !kind.contains_path(Path::new(&canonical_relative)) {
            return Ok(None);
        }
        self.execute(move |db| {
            let draft = load_payload(db, &checkout_id, &canonical_relative)?;
            draft
                .map(|draft| {
                    let base = stored_revision(db, &checkout_id, &draft.base_revision, kind)?;
                    ensure!(
                        base.relative_path == canonical_relative,
                        "The draft points to a different document"
                    );
                    Ok(RepositoryMarkdownDraft {
                        base,
                        revision: draft.revision,
                        markdown: draft.markdown,
                    })
                })
                .transpose()
        })
        .await
    }

    pub async fn repository_markdown_draft_paths(
        &self,
        checkout_id: String,
    ) -> Result<Vec<String>> {
        self.checkout_file_draft_paths(checkout_id, DocumentKind::Markdown)
            .await
    }

    pub(crate) async fn checkout_file_draft_paths(
        &self,
        checkout_id: String,
        kind: DocumentKind,
    ) -> Result<Vec<String>> {
        self.execute(move |db| {
            let mut rows = db.prepare(
                "SELECT d.document FROM drafts d JOIN checkouts c ON c.id = d.owner_id
                 WHERE d.kind = 'checkout_file' AND d.owner_id = ?1
                 ORDER BY d.updated_at DESC, d.document",
            )?;
            let paths = rows
                .query_map([checkout_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(paths
                .into_iter()
                .filter(|path| kind.contains_path(Path::new(path)))
                .collect())
        })
        .await
    }

    pub async fn discard_repository_markdown_draft(
        &self,
        checkout_id: String,
        canonical_relative: String,
        expected_draft_revision: String,
    ) -> Result<bool> {
        self.discard_checkout_file_draft(
            checkout_id,
            canonical_relative,
            expected_draft_revision,
            DocumentKind::Markdown,
        )
        .await
    }

    pub(crate) async fn discard_checkout_file_draft(
        &self,
        checkout_id: String,
        canonical_relative: String,
        expected_draft_revision: String,
        kind: DocumentKind,
    ) -> Result<bool> {
        if !kind.contains_path(Path::new(&canonical_relative)) {
            return Ok(false);
        }
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !load_payload(&tx, &checkout_id, &canonical_relative)?
                .is_some_and(|draft| draft.revision == expected_draft_revision)
            {
                return Ok(false);
            }
            let deleted = tx.execute(
                "DELETE FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1 AND document = ?2",
                params![checkout_id, canonical_relative],
            )?;
            tx.commit()?;
            Ok(deleted == 1)
        })
        .await
    }
}

fn stored_revision(
    db: &Connection,
    checkout_id: &str,
    revision: &str,
    kind: DocumentKind,
) -> Result<RepositoryMarkdownRevision> {
    ensure!(
        kind.contains_revision(revision),
        "The base revision belongs to a different editor"
    );
    let base = db
        .query_row(
            "SELECT id, checkout_id, path, sha256, base_commit FROM repository_markdown_revisions
         WHERE checkout_id = ?1 AND id = ?2",
            params![checkout_id, revision],
            |row| {
                Ok(RepositoryMarkdownRevision {
                    id: row.get(0)?,
                    checkout_id: row.get(1)?,
                    relative_path: row.get(2)?,
                    sha256: row.get(3)?,
                    base_commit: row.get(4)?,
                })
            },
        )
        .optional()?
        .context("The file base revision is unavailable for this checkout")?;
    ensure!(
        kind.contains_path(Path::new(&base.relative_path)),
        "The base file belongs to a different editor"
    );
    Ok(base)
}

fn load_payload(
    db: &Connection,
    checkout_id: &str,
    relative_path: &str,
) -> Result<Option<DraftPayload>> {
    let protected: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1 AND document = ?2",
            params![checkout_id, relative_path],
            |row| row.get(0),
        )
        .optional()?;
    protected
        .map(|protected| {
            let draft: DraftPayload = serde_json::from_slice(&protection::unprotect(&protected)?)
                .context("The file draft could not be read")?;
            ensure!(
                draft.version == PAYLOAD_VERSION,
                "Unsupported file draft version"
            );
            Ok(draft)
        })
        .transpose()
}
