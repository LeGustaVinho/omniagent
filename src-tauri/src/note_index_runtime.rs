use omniagent::{
    diagnostics,
    repository::markdown_index::{IndexedNote, IndexedNoteDetails, IndexedNoteSearchHit},
    store::Store,
};
use tauri::State;

fn index_error(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
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
