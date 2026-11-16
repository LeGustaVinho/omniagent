use omniagent::{
    diagnostics,
    repository::verified_git::{DeliveryGitPolicy, VerifiedCommit},
    store::Store,
    tasks::{ChecklistItem, ProjectWorkHierarchy, TaskChecklist, TaskDelivery, TaskDeliveryState, TaskWorkflowState},
};
use serde::Deserialize;
use tauri::State;

fn report(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn project_work_hierarchy(
    store: State<'_, Store>,
    project_id: String,
) -> Result<ProjectWorkHierarchy, String> {
    store.project_work_hierarchy(project_id).await.map_err(report)
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum WorkspaceWork {
    Epic,
    Task { epic_id: Option<String> },
    Subtask { parent_task_id: String },
}

#[tauri::command]
pub async fn create_workspace_work(
    store: State<'_, Store>, project_id: String, title: String, placement: WorkspaceWork,
) -> Result<String, String> {
    let result = match placement {
        WorkspaceWork::Epic => store.create_project_epic(project_id, title).await.map(|work| work.id),
        WorkspaceWork::Task { epic_id: Some(epic) } => store.create_project_task_in_epic(project_id, epic, title).await.map(|work| work.id),
        WorkspaceWork::Task { epic_id: None } => store.create_project_task(project_id, title).await.map(|work| work.id),
        WorkspaceWork::Subtask { parent_task_id } => store.create_project_subtask(project_id, parent_task_id, title).await.map(|work| work.id),
    };
    result.map_err(report)
}

#[tauri::command]
pub async fn project_task_checklist(
    store: State<'_, Store>, project_id: String, task_id: String,
) -> Result<Option<TaskChecklist>, String> {
    store.project_checklist(project_id, task_id).await.map_err(report)
}

#[tauri::command]
pub async fn append_project_checklist_item(
    store: State<'_, Store>, project_id: String, task_id: String, body: String,
) -> Result<ChecklistItem, String> {
    if body.trim().is_empty() {
        return Err("A checklist item needs text".into());
    }
    store.ensure_project_checklist(project_id.clone(), task_id.clone()).await.map_err(report)?;
    store.append_project_checklist_item(project_id, task_id, body).await.map_err(report)
}

#[tauri::command]
pub async fn revise_project_checklist_item(
    store: State<'_, Store>, project_id: String, task_id: String, next: ChecklistItem,
) -> Result<ChecklistItem, String> {
    store.revise_project_checklist_item(project_id, task_id, next.id, next.revision, next.body, next.checked).await.map_err(report)
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
