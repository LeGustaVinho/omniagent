use omniagent::{
    store::Store,
    workflows::{WorkflowArtifact, decision_nodes::WorkflowDecisionSubmission},
};
use tauri::State;

use super::report;

#[tauri::command]
pub async fn resolve_workflow_stage_decision(
    store: State<'_, Store>,
    submission: WorkflowDecisionSubmission,
) -> Result<WorkflowArtifact, String> {
    store
        .resolve_workflow_stage_decision(submission)
        .await
        .map_err(report)
}
