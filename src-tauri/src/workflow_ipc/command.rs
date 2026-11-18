use omniagent::{
    store::Store,
    workflows::{CommandRegistration, CommandRegistrationRequest, WorkflowArtifact},
};
use tauri::State;

use super::report;

#[tauri::command]
pub async fn list_workflow_command_registrations(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<CommandRegistration>, String> {
    store
        .list_workflow_command_registrations(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn register_workflow_command(
    store: State<'_, Store>,
    project_id: String,
    request: CommandRegistrationRequest,
) -> Result<CommandRegistration, String> {
    store
        .register_workflow_command(project_id, request)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_command_result(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<WorkflowArtifact>, String> {
    store
        .workflow_command_result(project_id, run_id)
        .await
        .map_err(report)
}
