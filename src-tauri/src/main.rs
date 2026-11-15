#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod conversation_runtime;
mod desktop;
mod fixture;
mod fixture_runtime;
mod ipc;

use std::{path::PathBuf, sync::Arc};

use omniagent::{
    jobs::JobScheduler,
    repository::{self, ProjectCheckout},
    runner::DirectWindowsRunner,
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
    if let Some(code) = omniagent::mcp_bridge::run_from_args() {
        std::process::exit(code);
    }
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
            let scheduler = tauri::async_runtime::block_on(async {
                JobScheduler::start(
                    store.clone(),
                    Arc::new(DirectWindowsRunner),
                    std::env::current_exe().ok(),
                )
            });
            app.manage(store);
            app.manage(scheduler);
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
            ipc::project_attachment_limits,
            ipc::set_project_attachment_limits,
            ipc::probe_cli_executable,
            ipc::cli_plugin_inventory,
            ipc::review_checkout_customizations,
            ipc::approve_checkout_customization,
            ipc::select_active_checkout,
            ipc::workbench_layout,
            ipc::save_workbench_layout,
            ipc::run_snapshot,
            ipc::list_conversations,
            ipc::create_conversation,
            ipc::conversation_messages,
            ipc::conversation_draft,
            ipc::save_conversation_draft,
            conversation_runtime::conversation_models,
            conversation_runtime::start_conversation_turn,
            conversation_runtime::latest_conversation_run,
            conversation_runtime::latest_conversation_job,
            conversation_runtime::cancel_conversation_job,
            conversation_runtime::stop_conversation_turn,
            conversation_runtime::approve_conversation_input,
            conversation_runtime::answer_conversation_question,
            conversation_runtime::pending_conversation_inputs,
            conversation_runtime::conversation_events,
            conversation_runtime::conversation_input,
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
