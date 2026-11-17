use anyhow::Context;
use omniagent::{
    installation::InstallationIdentity,
    jobs::JobScheduler,
    maintenance_update::{
        SignedUpdatePreview, TrustedUpdateFeed, UpdateActivationReceipt, UpdateJournal,
        VerifiedMaintenanceUpdate,
    },
    restore::InstallationLayout,
    store::Store,
    workflows::maintenance::{
        MaintenanceActivationRequest, MaintenanceConfiguration, MaintenanceHandoff,
        MaintenanceHandoffRequest, MaintenanceSettings, WorkflowMaintenance,
    },
};
use serde::Serialize;
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
    scheduler: State<'_, JobScheduler>,
    request: MaintenanceHandoffRequest,
) -> Result<MaintenanceHandoff, String> {
    let _work = scheduler.admit_external_work().await.map_err(report)?;
    store
        .create_maintenance_handoff(request, layout.maintenance_packages())
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn activate_maintenance_fixture(
    layout: State<'_, InstallationLayout>,
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    request: MaintenanceActivationRequest,
) -> Result<MaintenanceHandoff, String> {
    let _work = scheduler.admit_external_work().await.map_err(report)?;
    store
        .activate_maintenance_fixture(request, layout.maintenance_packages())
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn preview_signed_update(
    app: tauri::AppHandle,
    store: State<'_, Store>,
    layout: State<'_, InstallationLayout>,
    request: MaintenanceActivationRequest,
) -> Result<SignedUpdatePreview, String> {
    prepare_update(&app, &store, &layout, request)
        .await
        .map(|verified| verified.preview())
        .map_err(report)
}

#[tauri::command]
pub async fn activate_signed_update(
    app: tauri::AppHandle,
    store: State<'_, Store>,
    layout: State<'_, InstallationLayout>,
    identity: State<'_, InstallationIdentity>,
    scheduler: State<'_, JobScheduler>,
    request: MaintenanceActivationRequest,
) -> Result<UpdateActivationReceipt, String> {
    let verified = prepare_update(&app, &store, &layout, request)
        .await
        .map_err(report)?;
    let backups = layout.app_data().join("update-backups");
    tokio::fs::create_dir_all(&backups)
        .await
        .context("Create the pre-update backup directory")
        .map_err(report)?;
    verified
        .install_after_drain(
            &store,
            &identity,
            &scheduler,
            layout.maintenance_packages(),
            backups,
        )
        .await
        .map_err(report)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedUpdateStatus {
    configured: bool,
    latest: Option<UpdateActivationReceipt>,
}

#[tauri::command]
pub async fn signed_update_status(
    identity: State<'_, InstallationIdentity>,
) -> Result<SignedUpdateStatus, String> {
    let identity = identity.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let configured = TrustedUpdateFeed::application()?.is_some();
        let latest = UpdateJournal::open(&identity)?.latest()?;
        Ok(SignedUpdateStatus { configured, latest })
    })
    .await
    .context("Read the installed update history")
    .map_err(report)?
    .map_err(report)
}

async fn prepare_update(
    app: &tauri::AppHandle,
    store: &Store,
    layout: &InstallationLayout,
    request: MaintenanceActivationRequest,
) -> anyhow::Result<VerifiedMaintenanceUpdate> {
    let trust = TrustedUpdateFeed::application()?
        .context("This installer has no trusted update feed configured")?;
    VerifiedMaintenanceUpdate::prepare(app, store, &trust, request, layout.maintenance_packages())
        .await
}
