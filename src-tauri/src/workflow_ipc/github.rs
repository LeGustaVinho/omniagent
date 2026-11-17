use omniagent::{store::Store, workflows::github::GithubSettings};
use tauri::State;

#[tauri::command]
pub async fn workflow_github_settings(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Option<GithubSettings>, String> {
    store
        .workflow_github_settings(project_id)
        .await
        .map_err(super::report)
}

#[tauri::command]
pub async fn set_workflow_github_settings(
    store: State<'_, Store>,
    project_id: String,
    settings: GithubSettings,
) -> Result<String, String> {
    store
        .set_workflow_github_settings(project_id, settings)
        .await
        .map_err(super::report)
}
