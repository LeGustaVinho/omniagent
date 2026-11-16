use omniagent::{
    diagnostics,
    repository::{
        delivery_history::VerifiedDeliveryUnitHistory,
        destination_drift::{
            DestinationDriftOutcome, DestinationDriftPatch, DestinationDriftPolicy,
        },
        verified_merge::{DeliveryMergePreview, VerifiedMerge},
    },
    store::Store,
    tasks::DeliveryMilestonePolicy,
};
use tauri::State;

fn report(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn delivery_milestone_policy(
    store: State<'_, Store>,
    project_id: String,
) -> Result<DeliveryMilestonePolicy, String> {
    store
        .delivery_milestone_policy(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn set_delivery_milestone_policy(
    store: State<'_, Store>,
    project_id: String,
    policy: DeliveryMilestonePolicy,
) -> Result<DeliveryMilestonePolicy, String> {
    store
        .set_delivery_milestone_policy(project_id, policy)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn delivery_destination_drift_policy(
    store: State<'_, Store>,
    project_id: String,
) -> Result<DestinationDriftPolicy, String> {
    store
        .delivery_destination_drift_policy(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn set_delivery_destination_drift_policy(
    store: State<'_, Store>,
    project_id: String,
    policy: DestinationDriftPolicy,
) -> Result<DestinationDriftPolicy, String> {
    store
        .set_delivery_destination_drift_policy(project_id, policy)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn accept_delivery_destination_drift(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    workflow_run_id: String,
    preview_id: String,
    verification_id: String,
) -> Result<DestinationDriftOutcome, String> {
    store
        .accept_delivery_destination_drift(
            project_id,
            task_id,
            workflow_run_id,
            preview_id,
            verification_id,
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn delivery_destination_drift_patch(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    workflow_run_id: String,
    preview_id: String,
) -> Result<DestinationDriftPatch, String> {
    store
        .delivery_destination_drift_patch(project_id, task_id, workflow_run_id, preview_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn delivery_merge_preview(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    workflow_run_id: String,
    unit_id: String,
    commit_evidence_id: String,
) -> Result<DeliveryMergePreview, String> {
    store
        .delivery_merge_preview(
            project_id,
            task_id,
            workflow_run_id,
            unit_id,
            commit_evidence_id,
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn verified_delivery_merge(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    workflow_run_id: String,
) -> Result<Option<VerifiedMerge>, String> {
    store
        .verified_delivery_merge_for_workflow(project_id, task_id, workflow_run_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn verified_delivery_merge_for_commit(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    commit_evidence_id: String,
) -> Result<Option<VerifiedMerge>, String> {
    store
        .verified_delivery_merge(project_id, task_id, commit_evidence_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn verified_work_delivery_units(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<Vec<VerifiedDeliveryUnitHistory>, String> {
    store
        .verified_work_delivery_units(project_id, task_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn verified_attempt_deliveries(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Vec<VerifiedDeliveryUnitHistory>, String> {
    store
        .verified_attempt_deliveries(project_id, run_id)
        .await
        .map_err(report)
}
