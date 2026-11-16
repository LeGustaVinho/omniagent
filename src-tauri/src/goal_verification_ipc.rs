use omniagent::{
    diagnostics,
    jobs::{GoalCriterionEvidence, GoalEvidenceChoice, GoalVerification},
    store::Store,
};
use tauri::State;

#[tauri::command]
pub async fn goal_verification(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<GoalVerification>, String> {
    store
        .goal_verification(project_id, run_id)
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn list_goal_criterion_evidence(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Vec<GoalEvidenceChoice>, String> {
    store
        .list_goal_criterion_evidence(project_id, run_id)
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn record_goal_criterion_evidence(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    ordinal: usize,
    evidence: GoalCriterionEvidence,
) -> Result<GoalVerification, String> {
    store
        .record_goal_criterion_evidence(project_id, run_id, ordinal, evidence)
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}
