use std::path::PathBuf;

use omniagent::{backup::BackupReceipt, installation::InstallationIdentity, store::Store};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn create_application_backup(
    app: AppHandle,
    store: State<'_, Store>,
) -> Result<Option<BackupReceipt>, String> {
    let Some(destination) = choose_folder(app.clone(), "Choose where to save the backup").await?
    else {
        return Ok(None);
    };
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|error| report(&error))?;
    let identity = tauri::async_runtime::spawn_blocking({
        let root = root.clone();
        move || InstallationIdentity::load_or_create(&root)
    })
    .await
    .map_err(|error| report(&error))?
    .map_err(|error| report(&error))?;
    store
        .create_application_backup(identity, root.join("maintenance-packages"), destination)
        .await
        .map(Some)
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
