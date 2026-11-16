use omniagent::{
    store::Store,
    workflows::{
        ApprovalRuleSettings, WorkflowApprovalPreview, WorkflowApprovalSubmission,
        WorkflowArtifact, WorkflowHostRequest, WorkflowHostSubmission,
    },
};
use tauri::State;

use super::report;

#[tauri::command]
pub async fn preview_workflow_approval(
    store: State<'_, Store>,
    submission: WorkflowApprovalSubmission,
) -> Result<WorkflowApprovalPreview, String> {
    store
        .workflow_approval_preview(submission)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn set_workflow_approval_rule(
    store: State<'_, Store>,
    submission: WorkflowApprovalSubmission,
    expected_revision: Option<String>,
    settings: ApprovalRuleSettings,
) -> Result<WorkflowApprovalPreview, String> {
    store
        .set_workflow_approval_rule(submission, expected_revision, settings)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn evaluate_workflow_approval(
    store: State<'_, Store>,
    submission: WorkflowApprovalSubmission,
    expected_revision: String,
) -> Result<WorkflowApprovalPreview, String> {
    store
        .evaluate_workflow_approval(submission, expected_revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn approve_workflow_implementation(
    store: State<'_, Store>,
    submission: WorkflowApprovalSubmission,
) -> Result<WorkflowArtifact, String> {
    store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: submission.project_id,
            run_id: submission.run_id,
            node_id: submission.node_id,
            inputs: submission.inputs,
            request: WorkflowHostRequest::ApproveImplementation,
        })
        .await
        .map_err(report)
}
