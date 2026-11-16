use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{providers::configuration::open_locked_local_file, store::Store};

use super::{
    checkout_root, markdown_catalog::note_paths, markdown_links::MarkdownLinkNote, verify_checkout,
};
pub use links::{IndexedBacklink, IndexedNoteEdge, IndexedNoteRelationships, IndexedOutgoingLink};
pub use metadata::{
    IndexedBlock, IndexedHeading, IndexedLinkSyntax, IndexedNoteLink, IndexedProperty,
};
use metadata::{NoteMetadata, project};

mod links;
mod metadata;

#[cfg(test)]
mod tests;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedNote {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub frontmatter: Option<String>,
    pub tags: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedNoteDetails {
    #[serde(flatten)]
    pub note: IndexedNote,
    pub aliases: Vec<String>,
    pub properties: Vec<IndexedProperty>,
    pub headings: Vec<IndexedHeading>,
    pub blocks: Vec<IndexedBlock>,
    pub links: Vec<IndexedNoteLink>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedNoteSearchHit {
    pub path: String,
    pub title: String,
    pub snippet: String,
}

struct NoteSource {
    path: String,
    sha256: String,
    title: String,
    search_text: String,
    metadata: NoteMetadata,
}

struct NoteScan {
    present: BTreeSet<String>,
    updates: Vec<NoteSource>,
}

impl Store {
    pub async fn rebuild_repository_note_index(
        &self,
        checkout: String,
    ) -> Result<Vec<IndexedNote>> {
        self.reconcile_repository_note_index(checkout, None).await
    }

    pub async fn reconcile_repository_note_index(
        &self,
        checkout: String,
        paths: Option<Vec<String>>,
    ) -> Result<Vec<IndexedNote>> {
        let lock = {
            let mut locks = self
                .note_index_locks
                .lock()
                .expect("repository note index locks");
            locks.retain(|_, lock| lock.strong_count() > 0);
            let stored = locks.entry(checkout.clone()).or_default();
            stored.upgrade().unwrap_or_else(|| {
                let lock = Arc::default();
                *stored = Arc::downgrade(&lock);
                lock
            })
        };
        let _guard = lock.lock().await;
        let root = checkout_root(self, checkout.clone()).await?;
        let layout = verify_checkout(self, checkout.clone(), root).await?;
        let max_bytes = self
            .checkout_attachment_limits(checkout.clone())
            .await?
            .bytes_per_file;
        let known = self
            .execute({
                let checkout = checkout.clone();
                move |db| index_hashes(db, &checkout)
            })
            .await?;
        let scan = tokio::task::spawn_blocking(move || {
            let paths = paths.map(|paths| {
                paths
                    .into_iter()
                    .map(|path| path.replace('\\', "/").to_lowercase())
                    .collect::<BTreeSet<_>>()
            });
            scan_checkout(&layout.root, max_bytes, &known, paths.as_ref())
        })
        .await??;
        self.execute(move |db| {
            let tx = db.transaction()?;
            let changed = replace_index(&tx, &checkout, scan)?;
            links::rebuild_edges(&tx, &checkout, changed)?;
            let notes = list_indexed_notes(&tx, &checkout)?;
            tx.commit()?;
            Ok(notes)
        })
        .await
    }

    pub async fn repository_note_index(&self, checkout: String) -> Result<Vec<IndexedNote>> {
        self.execute(move |db| list_indexed_notes(db, &checkout))
            .await
    }

    pub async fn repository_indexed_note(
        &self,
        checkout: String,
        path: String,
    ) -> Result<Option<IndexedNoteDetails>> {
        self.execute(move |db| {
            db.query_row(
                "SELECT id, path, title, metadata FROM repository_note_index WHERE checkout_id = ?1 AND path = ?2",
                params![checkout, path],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
            ).optional()?.map(|(id, path, title, payload)| {
                let metadata: NoteMetadata = serde_json::from_str(&payload)?;
                Ok(IndexedNoteDetails {
                    note: IndexedNote { id, path, title, frontmatter: metadata.frontmatter, tags: metadata.tags },
                    aliases: metadata.aliases, properties: metadata.properties, headings: metadata.headings,
                    blocks: metadata.blocks, links: metadata.links,
                })
            }).transpose()
        }).await
    }

    pub async fn search_repository_note_index(
        &self,
        checkout: String,
        query: String,
        limit: u32,
    ) -> Result<Vec<IndexedNoteSearchHit>> {
        let query = search_query(&query);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT n.path, n.title, snippet(repository_note_fts, 1, '', '', ' … ', 24)
                 FROM repository_note_fts JOIN repository_note_index AS n ON n.id = repository_note_fts.rowid
                 WHERE repository_note_fts MATCH ?1 AND n.checkout_id = ?2
                 ORDER BY bm25(repository_note_fts, 5.0, 1.0, 2.0), n.path LIMIT ?3",
            )?;
            let hits = statement.query_map(params![query, checkout, limit.clamp(1, 100)], |row| {
                Ok(IndexedNoteSearchHit { path: row.get(0)?, title: row.get(1)?, snippet: row.get(2)? })
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(hits)
        }).await
    }
}

pub(super) fn source_link_note(path: &str, source: &str) -> MarkdownLinkNote {
    let (title, metadata) = project(path, source, "rename-preview");
    links::link_note(path.to_owned(), title, metadata)
}

fn index_hashes(db: &Connection, checkout: &str) -> Result<BTreeMap<String, String>> {
    let mut statement =
        db.prepare("SELECT path, sha256 FROM repository_note_index WHERE checkout_id = ?1")?;
    let entries = statement
        .query_map([checkout], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(entries)
}

fn scan_checkout(
    root: &Path,
    max_bytes: u64,
    known: &BTreeMap<String, String>,
    paths: Option<&BTreeSet<String>>,
) -> Result<NoteScan> {
    let mut scan = NoteScan {
        present: BTreeSet::new(),
        updates: Vec::new(),
    };
    for path in note_paths(root)? {
        if known.contains_key(&path) && paths.is_some_and(|hints| !is_affected(&path, hints)) {
            scan.present.insert(path);
            continue;
        }
        let Some(search_text) = read_source(&root.join(&path), max_bytes) else {
            continue;
        };
        let sha256 = format!("{:x}", Sha256::digest(search_text.as_bytes()));
        scan.present.insert(path.clone());
        if known.get(&path) == Some(&sha256) {
            continue;
        }
        let (title, metadata) = project(&path, &search_text, &sha256);
        scan.updates.push(NoteSource {
            path,
            sha256,
            title,
            search_text,
            metadata,
        });
    }
    Ok(scan)
}

fn is_affected(path: &str, hints: &BTreeSet<String>) -> bool {
    let path = path.to_lowercase();
    if hints.contains(&path) {
        return true;
    }
    path.match_indices('/')
        .any(|(index, _)| hints.contains(&path[..index]))
}

fn read_source(path: &Path, max_bytes: u64) -> Option<String> {
    let file = open_locked_local_file(path).ok()?;
    let mut bytes = Vec::new();
    if file
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()? as u64
        > max_bytes
    {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.contains('\0')).then_some(text)
}

fn replace_index(db: &Connection, checkout: &str, scan: NoteScan) -> Result<bool> {
    let known = index_hashes(db, checkout)?;
    let mut changed = !scan.updates.is_empty();
    let mut discard =
        db.prepare("DELETE FROM repository_note_index WHERE checkout_id = ?1 AND path = ?2")?;
    for path in known.keys().filter(|path| !scan.present.contains(*path)) {
        changed |= discard.execute(params![checkout, path])? != 0;
    }
    let mut replace = db.prepare(
        "INSERT INTO repository_note_index(checkout_id, path, sha256, title, search_text, metadata) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (checkout_id, path) DO UPDATE SET sha256 = excluded.sha256,
           title = excluded.title, search_text = excluded.search_text, metadata = excluded.metadata",
    )?;
    for note in scan.updates {
        replace.execute(params![
            checkout,
            note.path,
            note.sha256,
            note.title,
            note.search_text,
            serde_json::to_string(&note.metadata)?
        ])?;
    }
    Ok(changed)
}

fn list_indexed_notes(db: &Connection, checkout: &str) -> Result<Vec<IndexedNote>> {
    let mut statement = db.prepare(
        "SELECT id, path, title, json_extract(metadata, '$.frontmatter'), json_extract(metadata, '$.tags')
         FROM repository_note_index WHERE checkout_id = ?1 ORDER BY path",
    )?;
    let mut rows = statement.query([checkout])?;
    let mut notes = Vec::new();
    while let Some(row) = rows.next()? {
        let tags: String = row.get(4)?;
        notes.push(IndexedNote {
            id: row.get(0)?,
            path: row.get(1)?,
            title: row.get(2)?,
            frontmatter: row.get(3)?,
            tags: serde_json::from_str(&tags).context("read indexed note tags")?,
        });
    }
    Ok(notes)
}

fn search_query(input: &str) -> String {
    let trimmed = input.trim();
    let bounded = &trimmed[..trimmed
        .char_indices()
        .nth(256)
        .map_or(trimmed.len(), |(i, _)| i)];
    bounded
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .take(16)
        .map(|term| format!("\"{term}\"*"))
        .collect::<Vec<_>>()
        .join(" AND ")
}
