use tauri::State;

use crate::store::Store;

use super::{
    CandidateAcceptance, CandidateGate, CandidateGatePolicy, CandidateSelection, CandidateTarget,
    CandidateVerification, SelectedCandidate,
};

#[tauri::command]
pub async fn candidate_gate_policy(
    store: State<'_, Store>,
    project_id: String,
) -> Result<CandidateGatePolicy, String> {
    store
        .candidate_gate_policy(project_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_candidate_gate_policy(
    store: State<'_, Store>,
    project_id: String,
    gates: Vec<CandidateGate>,
) -> Result<String, String> {
    store
        .set_candidate_gate_policy(project_id, gates)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn select_worktree_candidate(
    store: State<'_, Store>,
    target: CandidateTarget,
    selection: CandidateSelection,
) -> Result<SelectedCandidate, String> {
    store
        .select_worktree_candidate(target, selection)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn current_selected_candidate(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    unit_id: String,
) -> Result<Option<SelectedCandidate>, String> {
    store
        .current_selected_candidate(project_id, task_id, unit_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn inspect_candidate_verification(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    verification_id: String,
) -> Result<Option<CandidateVerification>, String> {
    store
        .candidate_verification(project_id, task_id, verification_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn inspect_candidate_acceptance(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    acceptance_id: String,
) -> Result<Option<CandidateAcceptance>, String> {
    store
        .candidate_acceptance(project_id, task_id, acceptance_id)
        .await
        .map_err(|error| error.to_string())
}
