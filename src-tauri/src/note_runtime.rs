use std::path::PathBuf;

use omniagent::{
    diagnostics,
    repository::documents::{RepositoryAttachment, RepositoryDocumentOpen},
    store::Store,
};
use tauri::State;

fn report(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn open_repository_document(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
) -> Result<RepositoryDocumentOpen, String> {
    store
        .open_repository_document(checkout_id, path)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn read_repository_attachment(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
) -> Result<Option<RepositoryAttachment>, String> {
    store
        .read_repository_attachment(checkout_id, path)
        .await
        .map_err(report)
}
