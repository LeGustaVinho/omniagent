use omniagent::{
    diagnostics,
    store::Store,
    tasks::{TaskDeliveryState, TaskWorkflowState},
};
use tauri::State;

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
