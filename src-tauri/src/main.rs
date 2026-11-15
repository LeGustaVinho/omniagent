#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod conversation_runtime;
mod desktop;
#[cfg(debug_assertions)]
mod desktop_provider_fixture;
mod fixture;
mod fixture_runtime;
mod ipc;
mod queue_runtime;

use std::{path::PathBuf, sync::Arc};

use omniagent::{
    jobs::JobScheduler,
    repository::{
        self, AttemptWorktreeEvidence, LiveWorktree, ManagedWorktree, ProjectCheckout,
        WorktreeStart,
    },
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

#[tauri::command]
async fn provision_task_worktree(
    app: tauri::AppHandle,
    store: tauri::State<'_, Store>,
    project_id: String,
    task_id: String,
    source_checkout_id: String,
) -> Result<ManagedWorktree, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?
        .join("worktrees");
    repository::provision_task_worktree(&store, root, project_id, task_id, source_checkout_id)
        .await
        .map_err(|error| omniagent::diagnostics::preview(&error.to_string()))
}

#[tauri::command]
async fn set_project_worktree_root(
    store: tauri::State<'_, Store>,
    project_id: String,
    root: Option<PathBuf>,
) -> Result<(), String> {
    store
        .set_project_worktree_root(project_id, root)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn managed_worktree_start(
    store: tauri::State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<WorktreeStart>, String> {
    store
        .worktree_start(project_id, run_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn attempt_worktree_evidence(
    store: tauri::State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<AttemptWorktreeEvidence>, String> {
    store
        .attempt_worktree_evidence(project_id, run_id)
        .await
        .map_err(|error| omniagent::diagnostics::preview(&error.to_string()))
}

#[tauri::command]
async fn live_managed_worktree(
    store: tauri::State<'_, Store>,
    project_id: String,
    owner_task_id: String,
) -> Result<Option<LiveWorktree>, String> {
    store
        .live_managed_worktree(project_id, owner_task_id)
        .await
        .map_err(|error| omniagent::diagnostics::preview(&error.to_string()))
}

fn main() {
    if let Some(code) = omniagent::mcp_bridge::run_from_args() {
        std::process::exit(code);
    }
    if let Some(code) = fixture::run_from_args() {
        std::process::exit(code);
    }
    let context = tauri::generate_context!();
    #[cfg(debug_assertions)]
    if context.config().identifier == desktop_provider_fixture::APP_IDENTIFIER
        && let Some(code) = desktop_provider_fixture::run_from_args()
    {
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
            if let Err(error) =
                tauri::async_runtime::block_on(store.retire_local_attachments_after_restart())
            {
                eprintln!(
                    "Could not recover local attachment staging: {}",
                    omniagent::diagnostics::preview(&error.to_string())
                );
            }
            let notices = store.job_notices();
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
            desktop::notify_job_activity(app.handle().clone(), notices)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            register_project,
            provision_task_worktree,
            set_project_worktree_root,
            managed_worktree_start,
            attempt_worktree_evidence,
            live_managed_worktree,
            desktop::choose_repository,
            ipc::active_checkout,
            ipc::registered_checkouts,
            omniagent::appearance::appearance_settings,
            omniagent::appearance::save_appearance_settings,
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
            ipc::inspect_conversation_run,
            ipc::list_conversations,
            ipc::checkout_conversation,
            ipc::create_conversation,
            ipc::fork_conversation,
            ipc::rename_conversation,
            ipc::conversation_messages,
            ipc::conversation_history,
            ipc::rewind_conversation_history,
            ipc::project_queue_history_policy,
            ipc::set_project_queue_history_policy,
            ipc::conversation_queue_history_policy,
            ipc::set_conversation_queue_history_policy,
            ipc::project_subagent_policy,
            ipc::set_project_subagent_policy,
            ipc::project_question_timeout,
            ipc::set_project_question_timeout,
            ipc::conversation_subagent_quota,
            ipc::set_conversation_subagent_quota,
            ipc::project_subagent_audit,
            ipc::set_project_subagent_audit,
            ipc::conversation_subagent_audit,
            ipc::set_conversation_subagent_audit,
            ipc::conversation_draft,
            ipc::save_conversation_draft,
            conversation_runtime::conversation_models,
            conversation_runtime::start_conversation_turn,
            conversation_runtime::enqueue_managed_agent_job,
            conversation_runtime::latest_conversation_run,
            conversation_runtime::latest_conversation_job,
            conversation_runtime::pending_conversation_instructions,
            conversation_runtime::steer_pending_instruction,
            conversation_runtime::resolve_uncertain_steering,
            conversation_runtime::request_goal_stop,
            conversation_runtime::resume_goal,
            conversation_runtime::confirm_managed_goal_stop,
            conversation_runtime::confirm_native_goal_stop,
            conversation_runtime::revise_pending_instruction,
            conversation_runtime::remove_pending_instruction,
            conversation_runtime::project_job_queue,
            queue_runtime::set_job_queue_paused,
            queue_runtime::set_job_queue_capacity,
            queue_runtime::project_job_rules,
            queue_runtime::set_project_job_rules,
            queue_runtime::job_failure,
            queue_runtime::downstream_wait,
            queue_runtime::command_job_run,
            queue_runtime::command_job_output,
            queue_runtime::set_pending_job_state,
            queue_runtime::pending_job_approval_request,
            queue_runtime::decide_job_approval,
            queue_runtime::interrupt_queued_job_attempt,
            queue_runtime::queue_job_recovery,
            conversation_runtime::cancel_conversation_job,
            conversation_runtime::recover_conversation_job,
            conversation_runtime::stop_conversation_turn,
            conversation_runtime::approve_conversation_input,
            conversation_runtime::answer_conversation_question,
            conversation_runtime::answer_saved_conversation_question,
            conversation_runtime::pending_conversation_inputs,
            conversation_runtime::saved_conversation_questions,
            conversation_runtime::conversation_events,
            conversation_runtime::conversation_input,
            ipc::fixture_input_evidence,
            ipc::subscribe_events,
            ipc::unsubscribe_events,
            fixture_runtime::start_fixture,
            fixture_runtime::send_fixture_input,
            fixture_runtime::cancel_fixture,
        ])
        .run(context)
        .expect("failed to start OmniAgent");
}
