use omniagent::{
    diagnostics,
    jobs::{Job, JobRequest, QueueSettings, QueueState, RecoveryAction},
    store::Store,
};
use tauri::State;

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn set_job_queue_paused(
    store: State<'_, Store>,
    paused: bool,
) -> Result<QueueSettings, String> {
    store.set_job_queue_paused(paused).await.map_err(ui_error)
}

#[tauri::command]
pub async fn set_pending_job_state(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
    state: QueueState,
) -> Result<Job, String> {
    store
        .set_pending_job_state(project_id, job_id, state)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn pending_job_approval_request(
    store: State<'_, Store>,
    project_id: String,
    approval_id: String,
) -> Result<JobRequest, String> {
    store
        .pending_job_approval_request(project_id, approval_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn decide_job_approval(
    store: State<'_, Store>,
    project_id: String,
    approval_id: String,
    approved: bool,
) -> Result<(), String> {
    store
        .decide_job_approval(project_id, approval_id, approved)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn interrupt_queued_job_attempt(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
    run_id: String,
) -> Result<(), String> {
    store
        .request_project_job_interrupt(project_id, job_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn queue_job_recovery(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
    run_id: String,
    action: RecoveryAction,
) -> Result<Job, String> {
    store
        .queue_job_recovery(project_id, job_id, run_id, action)
        .await
        .map_err(ui_error)
}
