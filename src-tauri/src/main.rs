#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;
mod fixture;
mod fixture_runtime;
mod ipc;

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
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            desktop::show(app)
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .on_window_event(desktop::hide_main_on_close)
        .setup(|app| {
            let path = app.path().app_local_data_dir()?.join("omniagent.sqlite3");
            let store = Store::open(path)?;
            tauri::async_runtime::block_on(store.recover_agent_attempts_after_restart())?;
            app.manage(store);
            app.manage(ipc::EventSubscriptions::default());
            app.manage(fixture_runtime::FixtureRuns::default());
            desktop::create_tray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            register_project,
            desktop::choose_repository,
            ipc::active_checkout,
            ipc::cli_executables,
            ipc::set_cli_executable,
            ipc::probe_cli_executable,
            ipc::cli_plugin_inventory,
            ipc::select_active_checkout,
            ipc::workbench_layout,
            ipc::save_workbench_layout,
            ipc::run_snapshot,
            ipc::fixture_input_evidence,
            ipc::subscribe_events,
            ipc::unsubscribe_events,
            fixture_runtime::start_fixture,
            fixture_runtime::send_fixture_input,
            fixture_runtime::cancel_fixture,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start OmniAgent");
}
