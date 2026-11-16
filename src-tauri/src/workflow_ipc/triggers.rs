use omniagent::{
    store::Store,
    workflows::triggers::{WorkflowTriggerActivation, WorkflowTriggerRule, WorkflowTriggerSpec},
};
use tauri::State;

use super::report;

#[tauri::command]
pub async fn workflow_trigger_rules(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<WorkflowTriggerRule>, String> {
    store
        .workflow_trigger_rules(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_workflow_trigger_rule(
    store: State<'_, Store>,
    project_id: String,
    rule_id: Option<String>,
    expected_revision: Option<i64>,
    spec: WorkflowTriggerSpec,
) -> Result<WorkflowTriggerRule, String> {
    store
        .save_workflow_trigger_rule(project_id, rule_id, expected_revision, spec)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_trigger_activations(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<WorkflowTriggerActivation>, String> {
    store
        .workflow_trigger_activations(project_id)
        .await
        .map_err(report)
}
