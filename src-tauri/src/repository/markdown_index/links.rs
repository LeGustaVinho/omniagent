use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::store::Store;

use super::super::markdown_links::{
    MarkdownLinkAnchor, MarkdownLinkAnchorKind, MarkdownLinkNote, MarkdownLinkQuery,
    MarkdownLinkResolution, MarkdownLinkResolver, MarkdownLinkSuggestion, MarkdownLinkSyntax,
};
use super::metadata::{IndexedLinkSyntax, IndexedNoteLink, NoteMetadata};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedNoteEdge {
    pub source_path: String,
    pub target_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedNoteRelationships {
    pub outgoing: Vec<IndexedOutgoingLink>,
    pub backlinks: Vec<IndexedBacklink>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedOutgoingLink {
    pub link: IndexedNoteLink,
    pub resolution: MarkdownLinkResolution,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedBacklink {
    pub path: String,
    pub title: String,
    pub line: usize,
    pub label: Option<String>,
}

struct LinkSource {
    id: i64,
    note: MarkdownLinkNote,
    links: Vec<IndexedNoteLink>,
}

impl Store {
    pub async fn resolve_repository_note_links(
        &self,
        checkout: String,
        queries: Vec<MarkdownLinkQuery>,
    ) -> Result<Vec<MarkdownLinkResolution>> {
        self.execute(move |db| {
            let catalog = load_resolver_catalog(db, &checkout)?;
            Ok(MarkdownLinkResolver::new(&catalog).resolve_batch(&queries))
        })
        .await
    }

    pub async fn suggest_repository_note_links(
        &self,
        checkout: String,
        source: String,
        prefix: String,
        limit: usize,
    ) -> Result<Vec<MarkdownLinkSuggestion>> {
        self.execute(move |db| {
            let catalog = load_resolver_catalog(db, &checkout)?;
            Ok(MarkdownLinkResolver::new(&catalog).suggest(&source, &prefix, limit.clamp(1, 60)))
        })
        .await
    }

    pub async fn repository_note_edges(&self, checkout: String) -> Result<Vec<IndexedNoteEdge>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT DISTINCT s.path, t.path FROM repository_note_link_resolutions l
                 JOIN repository_note_index s ON s.id = l.source_id
                 JOIN repository_note_index t ON t.id = l.target_id
                 WHERE s.checkout_id = ?1 AND t.checkout_id = ?1 ORDER BY s.path, t.path",
            )?;
            let result = query
                .query_map([checkout], |row| {
                    Ok(IndexedNoteEdge {
                        source_path: row.get(0)?,
                        target_path: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(result)
        })
        .await
    }

    pub async fn repository_note_relationships(
        &self,
        checkout: String,
        path: String,
    ) -> Result<IndexedNoteRelationships> {
        self.execute(move |db| {
            Ok(IndexedNoteRelationships {
                outgoing: indexed_outgoing(db, &checkout, &path)?,
                backlinks: indexed_backlinks(db, &checkout, &path)?,
            })
        })
        .await
    }
}

pub(super) fn rebuild_edges(db: &Connection, checkout: &str, notes_changed: bool) -> Result<()> {
    let previous = db
        .query_row(
            "SELECT 1 FROM checkout_note_links_indexed WHERE checkout_id = ?1",
            [checkout],
            |_| Ok(()),
        )
        .optional()?;
    if !notes_changed && previous.is_some() {
        return Ok(());
    }
    let sources = load_sources(db, checkout)?;
    let catalog = sources
        .iter()
        .map(|source| source.note.clone())
        .collect::<Vec<_>>();
    let resolver = MarkdownLinkResolver::new(&catalog);
    let ids = sources
        .iter()
        .map(|source| (source.note.path.as_str(), source.id))
        .collect::<BTreeMap<_, _>>();
    db.execute(
        "DELETE FROM repository_note_link_resolutions WHERE source_id IN
        (SELECT id FROM repository_note_index WHERE checkout_id = ?1)",
        [checkout],
    )?;
    let mut insert = db.prepare(
        "INSERT INTO repository_note_link_resolutions(source_id, ordinal, target_id, resolution) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for source in &sources {
        for (ordinal, link) in source.links.iter().enumerate() {
            let Some(query) = link_query(&source.note.path, link) else {
                continue;
            };
            let resolution = resolver.resolve(&query);
            let target = match &resolution {
                MarkdownLinkResolution::Resolved { location } => {
                    ids.get(location.path.as_str()).copied()
                }
                _ => None,
            };
            insert.execute(params![
                source.id,
                ordinal as i64,
                target,
                serde_json::to_string(&resolution)?
            ])?;
        }
    }
    db.execute(
        "INSERT OR IGNORE INTO checkout_note_links_indexed(checkout_id) VALUES (?1)",
        [checkout],
    )?;
    Ok(())
}

fn load_sources(db: &Connection, checkout: &str) -> Result<Vec<LinkSource>> {
    let mut query = db.prepare("SELECT id, path, title, metadata FROM repository_note_index WHERE checkout_id = ?1 ORDER BY path")?;
    let mut rows = query.query([checkout])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let payload: String = row.get(3)?;
        let mut metadata: NoteMetadata = serde_json::from_str(&payload)?;
        let links = std::mem::take(&mut metadata.links);
        result.push(LinkSource {
            id: row.get(0)?,
            note: link_note(row.get(1)?, row.get(2)?, metadata),
            links,
        });
    }
    Ok(result)
}

fn load_resolver_catalog(db: &Connection, checkout: &str) -> Result<Vec<MarkdownLinkNote>> {
    let mut query = db.prepare(
        "SELECT path, title, json_extract(metadata, '$.headings'), json_extract(metadata, '$.blocks'), json_extract(metadata, '$.aliases')
         FROM repository_note_index WHERE checkout_id = ?1 ORDER BY path",
    )?;
    let mut rows = query.query([checkout])?;
    let mut notes = Vec::new();
    while let Some(row) = rows.next()? {
        let headings: String = row.get(2)?;
        let blocks: String = row.get(3)?;
        let aliases: String = row.get(4)?;
        notes.push(link_note(
            row.get(0)?,
            row.get(1)?,
            NoteMetadata {
                headings: serde_json::from_str(&headings)?,
                blocks: serde_json::from_str(&blocks)?,
                aliases: serde_json::from_str(&aliases)?,
                ..NoteMetadata::default()
            },
        ));
    }
    Ok(notes)
}

pub(super) fn link_note(path: String, title: String, metadata: NoteMetadata) -> MarkdownLinkNote {
    let anchors = metadata
        .headings
        .into_iter()
        .map(|heading| MarkdownLinkAnchor {
            kind: MarkdownLinkAnchorKind::Heading,
            label: heading.label,
            line: heading.line as u32,
        })
        .chain(metadata.blocks.into_iter().map(|block| MarkdownLinkAnchor {
            kind: MarkdownLinkAnchorKind::Block,
            label: block.id,
            line: block.line as u32,
        }))
        .collect();
    MarkdownLinkNote {
        path,
        title: Some(title),
        aliases: metadata.aliases,
        anchors,
    }
}

fn link_query(source_path: &str, link: &IndexedNoteLink) -> Option<MarkdownLinkQuery> {
    match link.syntax {
        IndexedLinkSyntax::Markdown => MarkdownLinkQuery::from_markdown(source_path, &link.target),
        IndexedLinkSyntax::Wiki => Some(MarkdownLinkQuery {
            source_path: source_path.to_owned(),
            target: link.target.clone(),
            anchor: link.anchor.clone(),
            syntax: MarkdownLinkSyntax::Wiki,
        }),
    }
}

fn indexed_outgoing(
    db: &Connection,
    checkout: &str,
    path: &str,
) -> Result<Vec<IndexedOutgoingLink>> {
    let mut query = db.prepare(
        "SELECT ref.value, l.resolution FROM repository_note_index n, json_each(n.metadata, '$.links') ref
         JOIN repository_note_link_resolutions l ON l.source_id = n.id AND l.ordinal = ref.key
         WHERE n.checkout_id = ?1 AND n.path = ?2 ORDER BY l.ordinal",
    )?;
    let mut rows = query.query(params![checkout, path])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let link: String = row.get(0)?;
        let resolution: String = row.get(1)?;
        result.push(IndexedOutgoingLink {
            link: serde_json::from_str(&link)?,
            resolution: serde_json::from_str(&resolution)?,
        });
    }
    Ok(result)
}

fn indexed_backlinks(db: &Connection, checkout: &str, path: &str) -> Result<Vec<IndexedBacklink>> {
    let mut query = db.prepare(
        "SELECT s.path, s.title, json_extract(ref.value, '$.line'), json_extract(ref.value, '$.alias')
         FROM repository_note_index t
         JOIN repository_note_link_resolutions l ON l.target_id = t.id
         JOIN repository_note_index s ON s.id = l.source_id AND t.checkout_id = s.checkout_id
         JOIN json_each(s.metadata, '$.links') ref ON ref.key = l.ordinal
         WHERE t.checkout_id = ?1 AND t.path = ?2 ORDER BY s.path, l.ordinal",
    )?;
    let result = query
        .query_map(params![checkout, path], |row| {
            Ok(IndexedBacklink {
                path: row.get(0)?,
                title: row.get(1)?,
                line: row.get::<_, u32>(2)? as usize,
                label: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(result)
}
