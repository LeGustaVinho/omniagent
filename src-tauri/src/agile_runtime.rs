use omniagent::{
    diagnostics,
    store::Store,
    tasks::{ProjectWorkEvent, TaskDeliveryState, TaskWorkflowState},
};
use tauri::State;

#[tauri::command]
pub async fn project_work_events(
    store: State<'_, Store>,
    project_id: String,
    after_sequence: Option<String>,
    limit: Option<u16>,
) -> Result<Vec<ProjectWorkEvent>, String> {
    let after = after_sequence
        .map(|value| value.parse::<i64>())
        .transpose()
        .map_err(|_| "Invalid Agile event sequence".to_owned())?;
    store
        .project_work_events(project_id, after, limit.unwrap_or(100))
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn transition_project_work(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    expected_state: TaskWorkflowState,
    next_state: TaskWorkflowState,
) -> Result<TaskDeliveryState, String> {
    store
        .transition_project_work(project_id, task_id, expected_state, next_state)
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}
