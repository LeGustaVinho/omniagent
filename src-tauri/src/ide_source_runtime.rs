use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use omniagent::{
    diagnostics,
    markdown_recovery::{
        RepositoryMarkdownDraft, RepositoryMarkdownInvalidation, RepositoryMarkdownWatch,
    },
    repository::{
        RepositoryMarkdown,
        documents::{
            CheckoutDocumentHistoryPage, CheckoutDocumentHistoryText, RepositoryDocumentOpen,
            RepositoryDocumentSave,
        },
    },
    store::Store,
};
use tauri::{State, ipc::Channel};

#[derive(Default)]
pub struct SourceWatches(Mutex<HashMap<String, RepositoryMarkdownWatch>>);

fn report(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn list_repository_source_files(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<String>, String> {
    store
        .list_repository_source_files(checkout_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn open_repository_source_file(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
) -> Result<RepositoryDocumentOpen, String> {
    store
        .open_repository_source_file(checkout_id, path)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_repository_source_file(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
    expected_revision: String,
    markdown: String,
) -> Result<RepositoryDocumentSave, String> {
    store
        .save_repository_source_file(checkout_id, path, expected_revision, markdown)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn read_repository_source_revision(
    store: State<'_, Store>,
    checkout_id: String,
    revision_id: String,
) -> Result<Option<RepositoryMarkdown>, String> {
    store
        .repository_source_revision(checkout_id, revision_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn checkout_document_history(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
    before: Option<i64>,
) -> Result<CheckoutDocumentHistoryPage, String> {
    store
        .checkout_document_history(checkout_id, path, before)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn checkout_document_history_text(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
    position: i64,
) -> Result<CheckoutDocumentHistoryText, String> {
    store
        .checkout_document_history_text(checkout_id, path, position)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn list_repository_source_drafts(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<String>, String> {
    store
        .repository_source_draft_paths(checkout_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn read_repository_source_draft(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
) -> Result<Option<RepositoryMarkdownDraft>, String> {
    store
        .repository_source_draft(checkout_id, path)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_repository_source_draft(
    store: State<'_, Store>,
    checkout_id: String,
    base_revision: String,
    expected_draft_revision: Option<String>,
    markdown: String,
) -> Result<RepositoryMarkdownDraft, String> {
    store
        .save_repository_source_draft(
            checkout_id,
            base_revision,
            expected_draft_revision,
            markdown,
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn discard_repository_source_draft(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    expected_draft_revision: String,
) -> Result<bool, String> {
    store
        .discard_repository_source_draft(checkout_id, path, expected_draft_revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn watch_repository_source(
    store: State<'_, Store>,
    watches: State<'_, SourceWatches>,
    checkout_id: String,
    base_revision: String,
    channel: Channel<RepositoryMarkdownInvalidation>,
) -> Result<String, String> {
    let watch = store
        .watch_repository_source(checkout_id, base_revision, move |event| {
            let _ = channel.send(event);
        })
        .await
        .map_err(report)?;
    let id = uuid::Uuid::new_v4().to_string();
    watches.0.lock().map_err(report)?.insert(id.clone(), watch);
    Ok(id)
}

#[tauri::command]
pub fn unwatch_repository_source(
    watches: State<'_, SourceWatches>,
    watch_id: String,
) -> Result<(), String> {
    drop(watches.0.lock().map_err(report)?.remove(&watch_id));
    Ok(())
}
