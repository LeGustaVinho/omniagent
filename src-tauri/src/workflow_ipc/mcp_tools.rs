use super::report;
use omniagent::{
    jobs::Job,
    store::Store,
    workflows::mcp_tools::{
        McpAttemptEvidence, McpJobReview, McpServer, McpServerRequest, WorkflowMcpToolSubmission,
    },
};
use tauri::State;

#[tauri::command]
pub async fn workflow_mcp_servers(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<McpServer>, String> {
    store.workflow_mcp_servers(project_id).await.map_err(report)
}

#[tauri::command]
pub async fn register_workflow_mcp_server(
    store: State<'_, Store>,
    project_id: String,
    request: McpServerRequest,
) -> Result<McpServer, String> {
    store
        .register_workflow_mcp_server(project_id, request)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn submit_workflow_mcp_tool(
    store: State<'_, Store>,
    submission: WorkflowMcpToolSubmission,
) -> Result<Job, String> {
    store
        .submit_workflow_mcp_tool(submission)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_mcp_job_review(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
) -> Result<McpJobReview, String> {
    store
        .workflow_mcp_job_review(project_id, job_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn decide_workflow_mcp_job(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
    approve: bool,
) -> Result<(), String> {
    store
        .decide_workflow_mcp_job(project_id, job_id, approve)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn set_workflow_mcp_grant(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
    allowed: bool,
) -> Result<(), String> {
    store
        .set_workflow_mcp_job_grant(project_id, job_id, allowed)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_mcp_attempt_evidence(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<McpAttemptEvidence>, String> {
    store
        .workflow_mcp_attempt_evidence(project_id, run_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn reconcile_workflow_mcp_attempt(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<(), String> {
    store
        .reconcile_workflow_mcp_attempt(project_id, run_id)
        .await
        .map_err(report)
}
