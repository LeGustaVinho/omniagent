use std::path::PathBuf;

use omniagent::{
    diagnostics,
    repository::project_search::{ProjectSearch, RipgrepStatus},
    store::Store,
};
use tauri::State;

fn report(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn ripgrep_status(store: State<'_, Store>) -> Result<RipgrepStatus, String> {
    store.ripgrep_status().await.map_err(report)
}

#[tauri::command]
pub async fn set_ripgrep_executable(
    store: State<'_, Store>,
    executable: Option<PathBuf>,
) -> Result<RipgrepStatus, String> {
    store
        .set_ripgrep_executable(executable)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn search_repository_files(
    store: State<'_, Store>,
    checkout_id: String,
    query: String,
    relative_directory: Option<String>,
    relative_file: Option<String>,
) -> Result<ProjectSearch, String> {
    match (relative_directory, relative_file) {
        (Some(_), Some(_)) => return Err("Choose either a search folder or a search file.".into()),
        (None, Some(file)) => store.search_repository_file(checkout_id, query, file).await,
        (Some(directory), None) => {
            store
                .search_repository_files_in(checkout_id, query, directory)
                .await
        }
        (None, None) => store.search_repository_files(checkout_id, query).await,
    }
    .map_err(report)
}
