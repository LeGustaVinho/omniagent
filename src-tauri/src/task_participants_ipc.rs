use omniagent::{diagnostics, store::Store, tasks::TaskRun};
use tauri::State;

#[tauri::command]
pub async fn project_task_runs(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<Vec<TaskRun>, String> {
    store
        .project_task_runs(project_id, task_id)
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}
