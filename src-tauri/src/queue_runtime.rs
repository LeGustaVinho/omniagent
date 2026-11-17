use omniagent::{
    diagnostics,
    jobs::{
        CommandJobOutput, DownstreamWait, Job, JobFailure, JobRequest, JobRule, QueueSettings,
        QueueState, RecoveryAction, InterruptedWriterReview, WriterReconciliationReceipt,
    },
    runs::RunRecord,
    store::Store,
};
use tauri::State;

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn downstream_wait(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<DownstreamWait>, String> {
    store
        .downstream_wait(project_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn command_job_run(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<RunRecord>, String> {
    store
        .command_job_run(project_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn command_job_output(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<CommandJobOutput>, String> {
    store
        .command_job_output(project_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn job_failure(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<JobFailure>, String> {
    store
        .job_failure(project_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn set_job_queue_paused(
    store: State<'_, Store>,
    paused: bool,
) -> Result<QueueSettings, String> {
    store.set_job_queue_paused(paused).await.map_err(ui_error)
}

#[tauri::command]
pub async fn set_job_queue_capacity(
    store: State<'_, Store>,
    capacity: u32,
) -> Result<QueueSettings, String> {
    store
        .set_job_queue_capacity(capacity)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn project_job_rules(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<JobRule>, String> {
    store.project_job_rules(project_id).await.map_err(ui_error)
}

#[tauri::command]
pub async fn set_project_job_rules(
    store: State<'_, Store>,
    project_id: String,
    expected: Vec<JobRule>,
    rules: Vec<JobRule>,
) -> Result<Vec<JobRule>, String> {
    store
        .set_project_job_rules(project_id, expected, rules)
        .await
        .map_err(ui_error)
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

#[tauri::command]
pub async fn interrupted_writer_review(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
    run_id: String,
) -> Result<Option<InterruptedWriterReview>, String> {
    store.interrupted_writer_review(project_id, job_id, run_id).await.map_err(ui_error)
}

#[tauri::command]
pub async fn reconcile_interrupted_writer(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
    run_id: String,
    expected_revision: String,
    external_effects_reconciled: bool,
) -> Result<WriterReconciliationReceipt, String> {
    store.reconcile_interrupted_writer(project_id, job_id, run_id, expected_revision, external_effects_reconciled)
        .await.map_err(ui_error)
}
