use anyhow::Result;
use omniagent::{
    installation::InstallationIdentity,
    jobs::JobScheduler,
    maintenance_update::{UpdateActivationReceipt, VerifiedMaintenanceUpdate},
    restore::InstallationLayout,
    store::Store,
    workflows::maintenance::MaintenanceActivationRequest,
};

use super::signed_update_desktop_fixture::verify_identifier;

#[tauri::command]
pub async fn signed_fixture_activate(
    app: tauri::AppHandle,
    store: tauri::State<'_, Store>,
    identity: tauri::State<'_, InstallationIdentity>,
    layout: tauri::State<'_, InstallationLayout>,
    scheduler: tauri::State<'_, JobScheduler>,
    request: MaintenanceActivationRequest,
    manifest: serde_json::Value,
) -> Result<UpdateActivationReceipt, String> {
    let run = async {
        verify_identifier(&app.config().identifier)?;
        let packages = layout.maintenance_packages();
        let backups = layout.app_data().join("update-backups");
        std::fs::create_dir_all(&backups)?;
        VerifiedMaintenanceUpdate::prepare_signed_desktop_fixture(
            &app, &store, request, &packages, manifest,
        )
        .await?
        .install_after_drain(&store, &identity, &scheduler, packages, backups)
        .await
    };
    run.await.map_err(|error| format!("{error:#}"))
}
