use std::path::PathBuf;

use omniagent::{
    backup::BackupReceipt,
    installation::InstallationIdentity,
    restore::{self, InstallationLayout, RestorePreview, RestoreReceipt, RestoreStatus},
    store::Store,
};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn create_application_backup(
    app: AppHandle,
    store: State<'_, Store>,
    identity: State<'_, InstallationIdentity>,
    layout: State<'_, InstallationLayout>,
) -> Result<Option<BackupReceipt>, String> {
    let Some(destination) = choose_folder(app, "Choose where to save the backup").await? else {
        return Ok(None);
    };
    store
        .create_application_backup(
            identity.inner().clone(),
            layout.maintenance_packages(),
            destination,
        )
        .await
        .map(Some)
        .map_err(|error| report(&error))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedBackup {
    archive: PathBuf,
    preview: RestorePreview,
}

#[tauri::command]
pub async fn preview_application_restore(
    app: AppHandle,
    identity: State<'_, InstallationIdentity>,
) -> Result<Option<SelectedBackup>, String> {
    let Some(archive) = choose_folder(app, "Choose an OmniAgent backup folder").await? else {
        return Ok(None);
    };
    with_installation(identity, move |identity| {
        let preview = restore::preview_restore(identity, &archive)?;
        Ok(Some(SelectedBackup { archive, preview }))
    })
    .await
}

#[tauri::command]
pub async fn stage_application_restore(
    identity: State<'_, InstallationIdentity>,
    archive: PathBuf,
    manifest_sha256: String,
) -> Result<RestoreReceipt, String> {
    with_installation(identity, move |identity| {
        restore::stage_restore(identity, &archive, &manifest_sha256)
    })
    .await
}

#[tauri::command]
pub async fn application_restore_status(
    identity: State<'_, InstallationIdentity>,
) -> Result<RestoreStatus, String> {
    with_installation(identity, restore::restore_status).await
}

async fn with_installation<T: Send + 'static>(
    identity: State<'_, InstallationIdentity>,
    work: impl FnOnce(&InstallationIdentity) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    let identity = identity.inner().clone();
    tauri::async_runtime::spawn_blocking(move || work(&identity))
        .await
        .map_err(|error| report(&error))?
        .map_err(|error| report(&error))
}

async fn choose_folder(app: AppHandle, title: &'static str) -> Result<Option<PathBuf>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut picker = app.dialog().file().set_title(title);
        if let Some(window) = app.get_webview_window("main") {
            picker = picker.set_parent(&window);
        }
        picker
            .blocking_pick_folder()
            .map(|folder| folder.into_path())
            .transpose()
            .map_err(|error| report(&error))
    })
    .await
    .map_err(|error| report(&error))?
}

fn report(error: &impl std::fmt::Display) -> String {
    omniagent::diagnostics::preview(&format!("{error:#}"))
}
