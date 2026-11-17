use std::collections::BTreeMap;

use omniagent::{
    diagnostics,
    markdown_analysis::{MarkdownAnalysis, analyze_markdown},
    markdown_properties::{
        MarkdownProperty, comparable_markdown_property, describe_markdown_properties,
        matching_markdown_property, replace_markdown_property,
    },
    repository::markdown_renames::{
        replace_markdown_note_reference, replace_wiki_note_reference_at,
    },
    store::Store,
};
use serde::Serialize;
use tauri::State;

#[tauri::command]
pub async fn analyze_note_markdown(
    markdown: String,
    buffer_revision: String,
) -> Result<MarkdownAnalysis, String> {
    tokio::task::spawn_blocking(move || analyze_markdown(&markdown, buffer_revision))
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn repair_note_wikilink(
    markdown: String,
    reference: String,
    source_line: Option<u32>,
    destination: String,
    anchor: Option<String>,
    markdown_source_path: Option<String>,
    source_offset: Option<usize>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || match markdown_source_path.as_deref() {
        Some(source) => replace_markdown_note_reference(
            &markdown,
            &reference,
            source,
            source_line,
            source_offset,
            &destination,
            anchor.as_deref(),
        ),
        None => replace_wiki_note_reference_at(
            &markdown,
            &reference,
            source_line,
            source_offset,
            &destination,
            anchor.as_deref(),
        ),
    })
    .await
    .map_err(|error| diagnostics::preview(&error.to_string()))?
    .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn replace_note_property(
    markdown: String,
    key: String,
    value: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || replace_markdown_property(&markdown, &key, &value))
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))?
        .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn describe_note_properties(markdown: String) -> Result<Vec<MarkdownProperty>, String> {
    tokio::task::spawn_blocking(move || describe_markdown_properties(&markdown))
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))?
        .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[derive(Serialize)]
pub struct NotePropertyHit {
    path: String,
    title: String,
    line: usize,
    value: String,
}

#[tauri::command]
pub async fn search_repository_note_properties(
    store: State<'_, Store>,
    checkout_id: String,
    key: String,
    value: String,
    drafts: BTreeMap<String, Option<String>>,
) -> Result<Vec<NotePropertyHit>, String> {
    let notes = store
        .repository_note_index(checkout_id)
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let expected = comparable_markdown_property(&value)?;
        Ok(notes
            .into_iter()
            .filter_map(|note| {
                let source = drafts
                    .get(&note.path)
                    .map(Option::as_deref)
                    .unwrap_or(note.frontmatter.as_deref())?;
                let (line, value) = matching_markdown_property(source, &key, &expected)?;
                Some(NotePropertyHit {
                    path: note.path,
                    title: note.title,
                    line,
                    value,
                })
            })
            .collect())
    })
    .await
    .map_err(|error| diagnostics::preview(&error.to_string()))?
    .map_err(|error| diagnostics::preview(&error.to_string()))
}
