use omniagent::{
    restore::InstallationLayout,
    store::Store,
    workflows::maintenance::{
        MaintenanceActivationRequest, MaintenanceConfiguration, MaintenanceHandoff,
        MaintenanceHandoffRequest, MaintenanceSettings, WorkflowMaintenance,
    },
};
use tauri::State;

use super::report;

#[tauri::command]
pub async fn workflow_maintenance(
    store: State<'_, Store>,
    project_id: String,
) -> Result<WorkflowMaintenance, String> {
    store.workflow_maintenance(project_id).await.map_err(report)
}

#[tauri::command]
pub async fn set_workflow_maintenance(
    store: State<'_, Store>,
    project_id: String,
    expected_revision: Option<String>,
    configuration: MaintenanceConfiguration,
) -> Result<MaintenanceSettings, String> {
    store
        .set_workflow_maintenance(project_id, expected_revision, configuration)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn create_maintenance_handoff(
    layout: State<'_, InstallationLayout>,
    store: State<'_, Store>,
    request: MaintenanceHandoffRequest,
) -> Result<MaintenanceHandoff, String> {
    store
        .create_maintenance_handoff(request, layout.maintenance_packages())
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn activate_maintenance_fixture(
    layout: State<'_, InstallationLayout>,
    store: State<'_, Store>,
    request: MaintenanceActivationRequest,
) -> Result<MaintenanceHandoff, String> {
    store
        .activate_maintenance_fixture(request, layout.maintenance_packages())
        .await
        .map_err(report)
}
