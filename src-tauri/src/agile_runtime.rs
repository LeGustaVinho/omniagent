use omniagent::{
    diagnostics,
    jobs::Job,
    model_catalog::ModelSelection,
    providers::SessionMode,
    store::Store,
    tasks::{
        ChecklistDispatchTarget, ChecklistExecutionMode, ProjectWorkEvent,
        TaskChecklistModeSettings, TaskDeliveryState, TaskWorkFields, TaskWorkflowState,
    },
};
use tauri::State;

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn project_work_fields(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<TaskWorkFields, String> {
    store
        .project_work_fields(project_id, task_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn update_project_work_fields(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    expected: TaskWorkFields,
    next: TaskWorkFields,
) -> Result<TaskWorkFields, String> {
    store
        .update_project_work_fields(project_id, task_id, expected, next)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn project_checklist_mode(
    store: State<'_, Store>,
    project_id: String,
) -> Result<ChecklistExecutionMode, String> {
    store
        .project_checklist_mode(project_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn set_project_checklist_mode(
    store: State<'_, Store>,
    project_id: String,
    mode: ChecklistExecutionMode,
) -> Result<(), String> {
    store
        .set_project_checklist_mode(project_id, mode)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn task_checklist_mode_settings(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<TaskChecklistModeSettings, String> {
    store
        .task_checklist_mode_settings(project_id, task_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn set_task_checklist_mode(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    mode: Option<ChecklistExecutionMode>,
) -> Result<(), String> {
    store
        .set_task_checklist_mode(project_id, task_id, mode)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn project_checklist_dispatch(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<Option<ChecklistDispatchTarget>, String> {
    store
        .project_checklist_dispatch(project_id, task_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn queue_project_checklist_work(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    expected_release_id: String,
    expected_unit_id: String,
    item_id: Option<String>,
    selection: ModelSelection,
) -> Result<Job, String> {
    async {
        let target = store
            .project_checklist_dispatch(project_id.clone(), task_id.clone())
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Approve and prepare repository delivery before queuing checklist work"
                )
            })?;
        anyhow::ensure!(
            target.release_id == expected_release_id && target.unit_id == expected_unit_id,
            "The delivery release or logical unit changed. Review the current work before queuing."
        );
        let scope = crate::conversation_runtime::selected_scope(
            &store,
            target.checkout_id,
            selection,
            None,
            SessionMode::ApprovedWrite,
        )
        .await?;
        store
            .queue_project_checklist_work(
                project_id,
                task_id,
                expected_release_id,
                expected_unit_id,
                item_id,
                scope,
            )
            .await
    }
    .await
    .map_err(ui_error)
}

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
