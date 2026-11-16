use omniagent::{
    diagnostics,
    repository::verified_git::{DeliveryGitPolicy, VerifiedCommit},
    store::Store,
    tasks::{TaskDelivery, TaskDeliveryState, TaskWorkflowState},
};
use tauri::State;

fn report(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn delivery_git_policy(
    store: State<'_, Store>,
    project_id: String,
) -> Result<DeliveryGitPolicy, String> {
    store.delivery_git_policy(project_id).await.map_err(report)
}

#[tauri::command]
pub async fn set_delivery_git_policy(
    store: State<'_, Store>,
    project_id: String,
    policy: DeliveryGitPolicy,
) -> Result<DeliveryGitPolicy, String> {
    store
        .set_delivery_git_policy(project_id, policy)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn verified_delivery_commit(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    evidence_id: String,
) -> Result<Option<VerifiedCommit>, String> {
    store
        .verified_delivery_commit(project_id, task_id, evidence_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn project_task_delivery_state(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<Option<TaskDeliveryState>, String> {
    store
        .project_task_delivery_state(project_id, task_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn project_epic_delivery_state(
    store: State<'_, Store>,
    project_id: String,
    epic_id: String,
) -> Result<Option<TaskWorkflowState>, String> {
    store
        .project_epic_delivery_state(project_id, epic_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn project_task_deliveries(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<Vec<TaskDelivery>, String> {
    store
        .project_task_deliveries(project_id, task_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn record_manual_task_delivery(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    note: Option<String>,
) -> Result<TaskDelivery, String> {
    store
        .record_manual_task_delivery(project_id, task_id, note)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn reopen_task_delivery(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<(), String> {
    store
        .reopen_task_delivery(project_id, task_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn cancel_task_delivery(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<(), String> {
    store
        .cancel_task_delivery(project_id, task_id)
        .await
        .map_err(report)
}
