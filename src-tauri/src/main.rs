#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod fixture;

use omniagent::store::Store;
use tauri::Manager;

#[tauri::command]
fn runtime_status() -> &'static str {
    "Desktop runtime is ready."
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
        .invoke_handler(tauri::generate_handler![runtime_status])
        .run(tauri::generate_context!())
        .expect("failed to start OmniAgent");
}
