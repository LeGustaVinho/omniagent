#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod fixture;

use std::path::PathBuf;

use omniagent::{
    repository::{self, ProjectCheckout},
    store::Store,
};
use tauri::Manager;

#[tauri::command]
fn runtime_status() -> &'static str {
    "Desktop runtime is ready."
}

#[tauri::command]
async fn register_project(
    store: tauri::State<'_, Store>,
    directory: PathBuf,
) -> Result<ProjectCheckout, String> {
    repository::register(&store, directory)
        .await
        .map_err(|error| error.to_string())
}

fn main() {
    if let Some(code) = fixture::run_from_args() {
        std::process::exit(code);
    }

    tauri::Builder::default()
        .setup(|app| {
            let path = app.path().app_local_data_dir()?.join("omniagent.sqlite3");
            app.manage(Store::open(path)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![runtime_status, register_project])
        .run(tauri::generate_context!())
        .expect("failed to start OmniAgent");
}
