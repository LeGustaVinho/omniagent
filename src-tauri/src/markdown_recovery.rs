use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{protection, repository::RepositoryMarkdownRevision, store::Store};

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
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let base = stored_revision(&tx, &checkout_id, &base_revision)?;
            let current = load_payload(&tx, &checkout_id, &base.relative_path)?;
            ensure!(
                current.as_ref().map(|draft| draft.revision.as_str())
                    == expected_draft_revision.as_deref(),
                "The Markdown draft changed before the edit was saved"
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
        self.execute(move |db| {
            let draft = load_payload(db, &checkout_id, &canonical_relative)?;
            draft
                .map(|draft| {
                    let base = stored_revision(db, &checkout_id, &draft.base_revision)?;
                    ensure!(
                        base.relative_path == canonical_relative,
                        "The Markdown draft points to a different document"
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
        self.execute(move |db| {
            let mut rows = db.prepare(
                "SELECT d.document FROM drafts d JOIN checkouts c ON c.id = d.owner_id
                 WHERE d.kind = 'checkout_file' AND d.owner_id = ?1
                 ORDER BY d.updated_at DESC, d.document",
            )?;
            Ok(rows
                .query_map([checkout_id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn discard_repository_markdown_draft(
        &self,
        checkout_id: String,
        canonical_relative: String,
        expected_draft_revision: String,
    ) -> Result<bool> {
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
) -> Result<RepositoryMarkdownRevision> {
    db.query_row(
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
    .context("The Markdown base revision is unavailable for this checkout")
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
                .context("The Markdown draft could not be read")?;
            ensure!(
                draft.version == PAYLOAD_VERSION,
                "Unsupported Markdown draft version"
            );
            Ok(draft)
        })
        .transpose()
}
