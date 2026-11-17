use omniagent::repository::verification::SelectedCandidate;
use omniagent::{
    store::Store,
    workflows::{
        NodeInputs, WorkflowArtifact, WorkflowHostRequest, WorkflowHostSubmission,
        authoring::package::WorkflowPackageDraft,
        templates::{WorkflowFactoryEntry, WorkflowFactoryTemplate},
    },
};
use tauri::State;

use super::report;

#[tauri::command]
pub fn workflow_factory_templates() -> Vec<WorkflowFactoryEntry> {
    omniagent::workflows::templates::workflow_factory_templates()
}

#[tauri::command]
pub async fn create_workflow_factory_draft(
    store: State<'_, Store>,
    project_id: String,
    template: WorkflowFactoryTemplate,
    revision: u16,
) -> Result<WorkflowPackageDraft, String> {
    store
        .create_workflow_factory_draft(project_id, template, revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_diff_candidates(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    node_id: String,
) -> Result<Vec<SelectedCandidate>, String> {
    store
        .workflow_diff_candidates(project_id, run_id, node_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn execute_workflow_diff_stage(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    node_id: String,
    inputs: NodeInputs,
    action: WorkflowHostRequest,
) -> Result<WorkflowArtifact, String> {
    if !matches!(
        &action,
        WorkflowHostRequest::ReadGit(_)
            | WorkflowHostRequest::VerifyReviewDiff
            | WorkflowHostRequest::PublishDiffReport
    ) {
        return Err("Choose a frozen diff stage action".into());
    }
    store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id,
            run_id,
            node_id,
            inputs,
            request: action,
        })
        .await
        .map_err(report)
}
