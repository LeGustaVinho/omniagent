use std::{collections::HashMap, sync::Mutex};

use omniagent::{
    diagnostics,
    repository::{
        markdown_index::{
            IndexedNote, IndexedNoteDetails, IndexedNoteEdge, IndexedNoteRelationships,
            IndexedNoteSearchHit,
        },
        markdown_index_watch::{IndexInvalidation, RepositoryNoteIndexWatch},
        markdown_links::{MarkdownLinkQuery, MarkdownLinkResolution, MarkdownLinkSuggestion},
    },
    store::Store,
};
use tauri::{State, ipc::Channel};
use uuid::Uuid;

#[derive(Default)]
pub struct NoteIndexWatches(Mutex<HashMap<String, RepositoryNoteIndexWatch>>);

fn index_error(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn watch_repository_note_index(
    store: State<'_, Store>,
    watches: State<'_, NoteIndexWatches>,
    checkout_id: String,
    channel: Channel<IndexInvalidation>,
) -> Result<String, String> {
    let watch = store
        .watch_repository_note_index(checkout_id, move |notice| {
            let _ = channel.send(notice);
        })
        .await
        .map_err(index_error)?;
    let id = Uuid::new_v4().to_string();
    watches
        .0
        .lock()
        .map_err(index_error)?
        .insert(id.clone(), watch);
    Ok(id)
}

#[tauri::command]
pub fn unwatch_repository_note_index(
    watches: State<'_, NoteIndexWatches>,
    watch_id: String,
) -> Result<(), String> {
    watches.0.lock().map_err(index_error)?.remove(&watch_id);
    Ok(())
}

#[tauri::command]
pub async fn reconcile_repository_note_index(
    store: State<'_, Store>,
    checkout_id: String,
    paths: Option<Vec<String>>,
) -> Result<Vec<IndexedNote>, String> {
    store
        .reconcile_repository_note_index(checkout_id, paths)
        .await
        .map_err(index_error)
}

#[tauri::command]
pub async fn rebuild_repository_note_index(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<IndexedNote>, String> {
    store
        .rebuild_repository_note_index(checkout_id)
        .await
        .map_err(index_error)
}

#[tauri::command]
pub async fn repository_indexed_note(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
) -> Result<Option<IndexedNoteDetails>, String> {
    store
        .repository_indexed_note(checkout_id, path)
        .await
        .map_err(index_error)
}

#[tauri::command]
pub async fn search_repository_note_index(
    store: State<'_, Store>,
    checkout_id: String,
    query: String,
    limit: u32,
) -> Result<Vec<IndexedNoteSearchHit>, String> {
    store
        .search_repository_note_index(checkout_id, query, limit)
        .await
        .map_err(index_error)
}

#[tauri::command]
pub async fn repository_note_edges(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<IndexedNoteEdge>, String> {
    store
        .repository_note_edges(checkout_id)
        .await
        .map_err(index_error)
}

#[tauri::command]
pub async fn repository_note_relationships(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
) -> Result<IndexedNoteRelationships, String> {
    store
        .repository_note_relationships(checkout_id, path)
        .await
        .map_err(index_error)
}

#[tauri::command]
pub async fn resolve_repository_note_links(
    store: State<'_, Store>,
    checkout_id: String,
    queries: Vec<MarkdownLinkQuery>,
) -> Result<Vec<MarkdownLinkResolution>, String> {
    store
        .resolve_repository_note_links(checkout_id, queries)
        .await
        .map_err(index_error)
}

#[tauri::command]
pub async fn suggest_repository_note_links(
    store: State<'_, Store>,
    checkout_id: String,
    source_path: String,
    prefix: String,
    limit: usize,
) -> Result<Vec<MarkdownLinkSuggestion>, String> {
    store
        .suggest_repository_note_links(checkout_id, source_path, prefix, limit)
        .await
        .map_err(index_error)
}
