use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::store::Store;

use super::{
    DocumentKind, LineEndings, RawDocument, RepositoryMarkdownRevision, TextEncoding,
    checkout_root, relative_name, verify_checkout,
};
use crate::repository::markdown::load_document_payload;

const PAGE_SIZE: usize = 30;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutDocumentHistoryEntry {
    pub position: i64,
    pub revision_id: String,
    pub recorded_at: i64,
    pub origin: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutDocumentHistoryPage {
    pub entries: Vec<CheckoutDocumentHistoryEntry>,
    pub next_before: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutDocumentHistoryText {
    pub text: String,
    pub utf8_bom: bool,
    pub encoding: TextEncoding,
    pub line_endings: LineEndings,
}

impl Store {
    pub async fn checkout_document_history(
        &self,
        checkout: String,
        path: PathBuf,
        before: Option<i64>,
    ) -> Result<CheckoutDocumentHistoryPage> {
        ensure!(before.is_none_or(|id| id > 0), "invalid history cursor");
        let path = history_path(self, &checkout, &path).await?;
        self.execute(move |db| {
            let mut entries = db.prepare(
                "SELECT position, revision_id, recorded_at, origin FROM checkout_document_history
                 WHERE checkout_id = ?1 AND path = ?2 AND position < ?3
                 ORDER BY position DESC LIMIT ?4",
            )?.query_map(params![checkout, path, before.unwrap_or(i64::MAX), (PAGE_SIZE + 1) as i64], |row| {
                Ok(CheckoutDocumentHistoryEntry { position: row.get(0)?, revision_id: row.get(1)?, recorded_at: row.get(2)?, origin: row.get(3)? })
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            let next_before = if entries.len() > PAGE_SIZE {
                entries.pop();
                entries.last().map(|entry| entry.position)
            } else { None };
            Ok(CheckoutDocumentHistoryPage { entries, next_before })
        }).await
    }

    pub async fn checkout_document_history_text(
        &self,
        checkout: String,
        path: PathBuf,
        position: i64,
    ) -> Result<CheckoutDocumentHistoryText> {
        let path = history_path(self, &checkout, &path).await?;
        self.execute(move |db| {
            let revision: String = db.query_row(
                "SELECT revision_id FROM checkout_document_history WHERE checkout_id = ?1 AND path = ?2 AND position = ?3",
                params![checkout, path, position], |row| row.get(0),
            ).optional()?.context("the local revision does not belong to this file and checkout")?;
            let kind = if DocumentKind::Source.contains_revision(&revision) { DocumentKind::Source } else { DocumentKind::Markdown };
            let (_, body) = load_document_payload(db, &checkout, &revision)?.context("the local revision is unavailable")?;
            let format = RawDocument::from_bytes(kind, &body)?.format();
            Ok(CheckoutDocumentHistoryText { text: format.markdown, utf8_bom: format.encoding == TextEncoding::Utf8Bom, encoding: format.encoding, line_endings: format.endings })
        }).await
    }
}

async fn history_path(store: &Store, checkout: &str, path: &Path) -> Result<String> {
    ensure!(!path.has_root(), "use the checkout-relative history path");
    let root = checkout_root(store, checkout.to_owned()).await?;
    let layout = verify_checkout(store, checkout.to_owned(), root).await?;
    relative_name(&layout.root, &layout.root.join(path))
}

pub(in crate::repository) fn append_save(
    db: &Connection,
    before: &RepositoryMarkdownRevision,
    saved: &RepositoryMarkdownRevision,
) -> Result<()> {
    let last_sha: Option<String> = db.query_row(
        "SELECT revision.sha256 FROM checkout_document_history history
         JOIN repository_markdown_revisions revision ON revision.checkout_id = history.checkout_id AND revision.id = history.revision_id
         WHERE history.checkout_id = ?1 AND history.path = ?2 ORDER BY history.position DESC LIMIT 1",
        params![saved.checkout_id, saved.relative_path], |row| row.get(0),
    ).optional()?;
    let mut insert = db.prepare(
        "INSERT INTO checkout_document_history (checkout_id, path, revision_id, origin) VALUES (?1, ?2, ?3, ?4)",
    )?;
    if last_sha.as_deref() != Some(&before.sha256) {
        insert.execute(params![
            before.checkout_id,
            before.relative_path,
            before.id,
            "before_save"
        ])?;
    }
    insert.execute(params![
        saved.checkout_id,
        saved.relative_path,
        saved.id,
        "saved"
    ])?;
    Ok(())
}
