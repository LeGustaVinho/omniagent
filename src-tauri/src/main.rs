#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod conversation_runtime;
mod composer_customization_ipc;
mod conversation_goal_runtime;
mod conversation_continuation_runtime;
mod conversation_fork_runtime;
mod conversation_attachment_ipc;
mod composer_suggestion_ipc;
mod conversation_compaction_ipc;
mod history_deletion_ipc;
mod conversation_steering_ipc;
mod agile_runtime;
mod backup_ipc;
mod delivery_merge_ipc;
mod desktop;
mod git_review_runtime;
mod ide_source_runtime;
mod project_search_runtime;
mod manual_terminal;
#[cfg(debug_assertions)]
mod desktop_provider_fixture;
mod fixture;
mod fixture_runtime;
mod goal_verification_ipc;
mod ipc;
mod markdown_analysis_ipc;
mod model_defaults_runtime;
mod note_runtime;
mod note_index_runtime;
mod queue_runtime;
mod side_question_ipc;
mod task_delivery_ipc;
mod task_assignment_ipc;
mod task_participants_ipc;
mod project_description_runtime;
mod workflow_ipc;

use std::{path::PathBuf, sync::Arc};

use omniagent::repository::verification::ipc as candidate;
use omniagent::{
    jobs::JobScheduler,
    repository::{
        self, AttemptWorktreeEvidence, BranchStrategy, LiveWorktree, ManagedWorktree,
        ProjectCheckout, TaskBranchStrategy, WorktreeStart,
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
async fn project_branch_strategy(
    store: tauri::State<'_, Store>,
    project_id: String,
) -> Result<BranchStrategy, String> {
    store
        .project_branch_strategy(project_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn set_project_branch_strategy(
    store: tauri::State<'_, Store>,
    project_id: String,
    strategy: BranchStrategy,
) -> Result<BranchStrategy, String> {
    store
        .set_project_branch_strategy(project_id, strategy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn task_branch_strategy(
    store: tauri::State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<TaskBranchStrategy, String> {
    store
        .task_branch_strategy(project_id, task_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn set_task_branch_strategy(
    store: tauri::State<'_, Store>,
    project_id: String,
    task_id: String,
    strategy: Option<BranchStrategy>,
) -> Result<TaskBranchStrategy, String> {
    store
        .set_task_branch_strategy(project_id, task_id, strategy)
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

#[tauri::command]
async fn discard_pending_worktree_changes(
    store: tauri::State<'_, Store>,
    project_id: String,
    checkout_id: String,
    scope_task_id: String,
) -> Result<(), String> {
    store
        .discard_pending_worktree_changes(project_id, checkout_id, scope_task_id)
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
        .on_webview_event(conversation_attachment_ipc::retain_attachment_drop)
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
            app.manage(conversation_attachment_ipc::AttachmentDropReceipts::default());
            app.manage(scheduler);
            app.manage(ipc::EventSubscriptions::default());
            app.manage(note_runtime::NoteWatches::default());
            app.manage(note_index_runtime::NoteIndexWatches::default());
            app.manage(ide_source_runtime::SourceWatches::default());
            app.manage(manual_terminal::ManualTerminals::default());
            app.manage(fixture_runtime::FixtureRuns::default());
            desktop::create_tray(app)?;
            desktop::notify_job_activity(app.handle().clone(), notices)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            backup_ipc::create_application_backup,
            register_project,
            provision_task_worktree,
            set_project_worktree_root,
            project_branch_strategy,
            set_project_branch_strategy,
            task_branch_strategy,
            set_task_branch_strategy,
            managed_worktree_start,
            attempt_worktree_evidence,
            live_managed_worktree,
            discard_pending_worktree_changes,
            candidate::candidate_gate_policy,
            candidate::set_candidate_gate_policy,
            candidate::select_worktree_candidate,
            candidate::current_selected_candidate,
            candidate::inspect_candidate_verification,
            candidate::inspect_candidate_acceptance,
            task_delivery_ipc::delivery_git_policy,
            task_delivery_ipc::set_delivery_git_policy,
            task_delivery_ipc::verified_delivery_commit,
            task_delivery_ipc::verified_delivery_patch,
            delivery_merge_ipc::delivery_milestone_policy,
            delivery_merge_ipc::set_delivery_milestone_policy,
            delivery_merge_ipc::delivery_destination_drift_policy,
            delivery_merge_ipc::set_delivery_destination_drift_policy,
            delivery_merge_ipc::accept_delivery_destination_drift,
            delivery_merge_ipc::delivery_destination_drift_patch,
            delivery_merge_ipc::delivery_merge_preview,
            delivery_merge_ipc::verified_delivery_merge,
            delivery_merge_ipc::verified_delivery_merge_for_commit,
            delivery_merge_ipc::verified_work_delivery_units,
            delivery_merge_ipc::verified_attempt_deliveries,
            task_delivery_ipc::project_task_delivery_state,
            task_delivery_ipc::project_epic_delivery_state,
            task_delivery_ipc::project_task_deliveries,
            task_delivery_ipc::record_manual_task_delivery,
            task_delivery_ipc::reopen_task_delivery,
            task_delivery_ipc::cancel_task_delivery,
            task_delivery_ipc::project_work_hierarchy,
            agile_runtime::transition_project_work,
            agile_runtime::project_work_events,
            agile_runtime::project_work_fields,
            agile_runtime::update_project_work_fields,
            agile_runtime::project_checklist_mode,
            agile_runtime::set_project_checklist_mode,
            agile_runtime::task_checklist_mode_settings,
            agile_runtime::set_task_checklist_mode,
            agile_runtime::project_checklist_dispatch,
            agile_runtime::queue_project_checklist_work,
            project_description_runtime::project_description_editor,
            project_description_runtime::save_project_description_draft,
            project_description_runtime::discard_project_description_draft,
            project_description_runtime::save_project_description_editor,
            task_delivery_ipc::create_workspace_work,
            task_delivery_ipc::project_task_checklist,
            task_delivery_ipc::append_project_checklist_item,
            task_delivery_ipc::revise_project_checklist_item,
            task_participants_ipc::project_task_runs,
            task_assignment_ipc::project_task_assignments,
            task_assignment_ipc::set_project_task_assignment,
            task_assignment_ipc::catalog_model_ratings,
            workflow_ipc::start_repository_delivery,
            workflow_ipc::launch::launch_workflow_manual,
            workflow_ipc::launch::workflow_launch,
            workflow_ipc::command::list_workflow_command_registrations,
            workflow_ipc::command::register_workflow_command,
            workflow_ipc::command::submit_workflow_command,
            workflow_ipc::command::workflow_command_result,
            workflow_ipc::mcp_tools::workflow_mcp_servers,
            workflow_ipc::mcp_tools::register_workflow_mcp_server,
            workflow_ipc::mcp_tools::submit_workflow_mcp_tool,
            workflow_ipc::mcp_tools::workflow_mcp_job_review,
            workflow_ipc::mcp_tools::decide_workflow_mcp_job,
            workflow_ipc::mcp_tools::set_workflow_mcp_grant,
            workflow_ipc::mcp_tools::workflow_mcp_attempt_evidence,
            workflow_ipc::mcp_tools::reconcile_workflow_mcp_attempt,
            workflow_ipc::maintenance::workflow_maintenance,
            workflow_ipc::maintenance::set_workflow_maintenance,
            workflow_ipc::maintenance::create_maintenance_handoff,
            workflow_ipc::maintenance::activate_maintenance_fixture,
            workflow_ipc::list_repository_deliveries,
            workflow_ipc::inspect_repository_delivery,
            workflow_ipc::execute_repository_delivery_host,
            workflow_ipc::approval::preview_workflow_approval,
            workflow_ipc::approval::workflow_approval_stage,
            workflow_ipc::approval::set_workflow_approval_rule,
            workflow_ipc::approval::evaluate_workflow_approval,
            workflow_ipc::approval::approve_workflow_implementation,
            workflow_ipc::submit_repository_delivery_agent,
            workflow_ipc::repository_delivery_plan_proposals,
            workflow_ipc::request_repository_plan_revision,
            workflow_ipc::repository_plan_history,
            workflow_ipc::publish_repository_delivery_evidence,
            workflow_ipc::publish_repository_delivery_candidate,
            workflow_ipc::execute_repository_delivery_unit,
            workflow_ipc::repository_delivery_tasks,
            workflow_ipc::repository_delivery_units,
            workflow_ipc::authoring::workflow_authoring_bases,
            workflow_ipc::authoring::conversation_workflow_authoring,
            workflow_ipc::authoring::start_workflow_authoring,
            workflow_ipc::authoring::inspect_workflow_authoring,
            workflow_ipc::authoring::save_workflow_authoring_draft,
            workflow_ipc::authoring::request_workflow_authoring_changes,
            workflow_ipc::triggers::workflow_trigger_rules,
            workflow_ipc::triggers::save_workflow_trigger_rule,
            workflow_ipc::triggers::workflow_trigger_activations,
            workflow_ipc::authoring::publish_reviewed_workflow,
            workflow_ipc::authoring::list_workflow_packages,
            workflow_ipc::authoring::inspect_workflow_package,
            workflow_ipc::authoring::preview_workflow_package_revision,
            workflow_ipc::authoring::simulate_workflow_package_revision,
            workflow_ipc::authoring::create_workflow_package,
            workflow_ipc::authoring::save_workflow_package,
            workflow_ipc::authoring::request_workflow_package_changes,
            workflow_ipc::authoring::start_workflow_package_authoring,
            workflow_ipc::authoring::start_workflow_editor_authoring,
            workflow_ipc::authoring::publish_reviewed_workflow_package,
            workflow_ipc::authoring::workflow_package_catalog,
            workflow_ipc::authoring::read_published_workflow_package_artifact,
            workflow_ipc::authoring::export_workflow_exchange,
            workflow_ipc::templates::workflow_factory_templates,
            workflow_ipc::templates::create_workflow_factory_draft,
            workflow_ipc::templates::workflow_diff_candidates,
            workflow_ipc::templates::execute_workflow_diff_stage,
            workflow_ipc::authoring::describe_workflow_reusable_boundary,
            workflow_ipc::authoring::derive_workflow_reusable,
            workflow_ipc::authoring::describe_workflow_reusable_node,
            workflow_ipc::authoring::preview_workflow_exchange,
            workflow_ipc::authoring::import_reviewed_workflow_exchange,
            workflow_ipc::graph_editor::workflow_graph_registry,
            workflow_ipc::graph_editor::project_workflow_graph_drafts,
            workflow_ipc::inspection::inspect_workflow_execution,
            workflow_ipc::inspection::inspect_workflow_execution_job,
            workflow_ipc::comparison::comparison_source_commit,
            workflow_ipc::comparison::model_comparisons,
            workflow_ipc::comparison::start_model_comparison,
            workflow_ipc::comparison::model_comparison_environments,
            workflow_ipc::comparison::model_comparison_outcomes,
            workflow_ipc::comparison::model_comparison_job_inputs,
            workflow_ipc::comparison::model_comparison_syntheses,
            workflow_ipc::comparison::start_model_comparison_synthesis,
            workflow_ipc::role_templates::list_conversation_templates,
            workflow_ipc::role_templates::conversation_template_revision,
            workflow_ipc::role_templates::save_conversation_template,
            workflow_ipc::role_templates::conversation_template_profile,
            workflow_ipc::stage_requirements::workflow_stage_inventory,
            workflow_ipc::stage_requirements::workflow_stage_matching_profiles,
            workflow_ipc::stage_requirements::workflow_stage_assessment,
            workflow_ipc::goal::preview_workflow_stage_goal,
            workflow_ipc::goal::verify_workflow_goal_candidate,
            workflow_ipc::goal::preview_workflow_goal_requalification,
            workflow_ipc::goal::submit_workflow_goal_requalification,
            desktop::choose_repository,
            desktop::choose_repository_markdown,
            desktop::list_repository_markdown_revisions,
            desktop::read_repository_markdown,
            desktop::repository_note_catalog,
            ide_source_runtime::list_repository_source_files,
            ide_source_runtime::open_repository_source_file,
            ide_source_runtime::save_repository_source_file,
            ide_source_runtime::read_repository_source_revision,
            ide_source_runtime::list_repository_source_drafts,
            ide_source_runtime::read_repository_source_draft,
            ide_source_runtime::save_repository_source_draft,
            ide_source_runtime::discard_repository_source_draft,
            ide_source_runtime::watch_repository_source,
            ide_source_runtime::unwatch_repository_source,
            project_search_runtime::ripgrep_status,
            project_search_runtime::set_ripgrep_executable,
            project_search_runtime::search_repository_files,
            manual_terminal::start_manual_terminal,
            manual_terminal::write_manual_terminal,
            manual_terminal::resize_manual_terminal,
            manual_terminal::close_manual_terminal,
            git_review_runtime::git_checkout_review,
            git_review_runtime::git_checkout_history,
            git_review_runtime::git_checkout_diff,
            git_review_runtime::set_git_path_staged,
            git_review_runtime::commit_git_index,
            git_review_runtime::git_saved_attempts,
            note_index_runtime::rebuild_repository_note_index,
            note_index_runtime::reconcile_repository_note_index,
            note_index_runtime::watch_repository_note_index,
            note_index_runtime::unwatch_repository_note_index,
            note_index_runtime::repository_indexed_note,
            note_index_runtime::search_repository_note_index,
            note_index_runtime::repository_note_edges,
            note_index_runtime::repository_note_relationships,
            note_index_runtime::resolve_repository_note_links,
            note_index_runtime::suggest_repository_note_links,
            note_index_runtime::preview_markdown_rename,
            note_index_runtime::apply_markdown_rename,
            note_runtime::open_repository_document,
            note_runtime::save_repository_document,
            note_runtime::read_repository_attachment,
            markdown_analysis_ipc::analyze_note_markdown,
            markdown_analysis_ipc::replace_note_property,
            markdown_analysis_ipc::repair_note_wikilink,
            note_runtime::read_repository_markdown_draft,
            note_runtime::list_repository_markdown_drafts,
            note_runtime::save_repository_markdown_draft,
            note_runtime::discard_repository_markdown_draft,
            note_runtime::watch_repository_note,
            note_runtime::unwatch_repository_note,
            ipc::active_checkout,
            ipc::registered_checkouts,
            omniagent::appearance::appearance_settings,
            omniagent::appearance::save_appearance_settings,
            ipc::cli_executables,
            ipc::desktop_cli_prerequisites,
            ipc::set_cli_executable,
            ipc::project_attachment_limits,
            ipc::set_project_attachment_limits,
            ipc::probe_cli_executable,
            ipc::cli_plugin_inventory,
            ipc::preview_claude_user_plugin_action,
            ipc::apply_claude_user_plugin_action,
            ipc::review_checkout_customizations,
            ipc::approve_checkout_customization,
            ipc::select_active_checkout,
            ipc::workbench_layout,
            ipc::save_workbench_layout,
            ipc::run_snapshot,
            ipc::inspect_conversation_run,
            ipc::run_orchestration,
            ipc::project_task_goals,
            goal_verification_ipc::goal_verification,
            goal_verification_ipc::list_goal_criterion_evidence,
            goal_verification_ipc::record_goal_criterion_evidence,
            ipc::list_conversations,
            conversation_runtime::conversation_library,
            conversation_goal_runtime::conversation_goal_overview,
            conversation_goal_runtime::start_conversation_goal,
            conversation_continuation_runtime::conversation_continuation_settings,
            conversation_continuation_runtime::set_conversation_continuation_policy,
            conversation_continuation_runtime::submit_conversation_continuation,
            conversation_continuation_runtime::confirm_conversation_continuation,
            conversation_continuation_runtime::conversation_continuation_checkouts,
            conversation_continuation_runtime::conversation_reference_source,
            conversation_continuation_runtime::preview_conversation_reference,
            conversation_continuation_runtime::conversation_reference_draft,
            conversation_continuation_runtime::edit_conversation_reference_draft,
            conversation_attachment_ipc::conversation_attachment_draft,
            conversation_attachment_ipc::pick_conversation_attachments,
            conversation_attachment_ipc::accept_conversation_attachment_drop,
            conversation_attachment_ipc::edit_conversation_attachment_draft,
            conversation_attachment_ipc::review_conversation_attachment_draft,
            composer_suggestion_ipc::composer_native_suggestion,
            conversation_compaction_ipc::conversation_compaction_state,
            conversation_compaction_ipc::set_conversation_compaction_settings,
            conversation_compaction_ipc::request_conversation_compaction,
            conversation_compaction_ipc::request_native_conversation_compaction,
            history_deletion_ipc::conversation_history_deletion_diffs,
            history_deletion_ipc::preview_conversation_history_deletion,
            history_deletion_ipc::confirm_conversation_history_deletion,
            composer_customization_ipc::composer_customizations,
            ipc::checkout_conversation,
            ipc::create_conversation,
            ipc::fork_conversation,
            ipc::rename_conversation,
            ipc::conversation_messages,
            ipc::conversation_history,
            ipc::rewind_conversation_history,
            conversation_runtime::conversation_history_inspection,
            conversation_runtime::conversation_history_revision,
            conversation_runtime::conversation_next_prompt_instructions,
            conversation_runtime::fork_conversation_with_message_edit,
            conversation_runtime::fork_retained_conversation,
            conversation_fork_runtime::conversation_fork_checkout_settings,
            conversation_fork_runtime::set_conversation_fork_checkout_policy,
            conversation_fork_runtime::preview_conversation_fork_checkout,
            conversation_fork_runtime::fork_conversation_with_checkout,
            ipc::project_queue_history_policy,
            ipc::set_project_queue_history_policy,
            ipc::conversation_queue_history_policy,
            ipc::set_conversation_queue_history_policy,
            ipc::project_subagent_policy,
            ipc::set_project_subagent_policy,
            ipc::project_question_timeout,
            ipc::set_project_question_timeout,
            ipc::settings_patch_allowlist,
            ipc::set_settings_patch_allowlist,
            ipc::settings_proposals,
            ipc::review_settings_proposal,
            ipc::template_display_label,
            ipc::set_template_display_label,
            ipc::conversation_subagent_quota,
            ipc::set_conversation_subagent_quota,
            ipc::project_subagent_audit,
            ipc::set_project_subagent_audit,
            ipc::conversation_subagent_audit,
            ipc::attempt_subagent_audit,
            ipc::set_conversation_subagent_audit,
            ipc::conversation_draft,
            ipc::save_conversation_draft,
            conversation_runtime::conversation_models,
            model_defaults_runtime::project_model_defaults,
            model_defaults_runtime::checkout_model_defaults,
            model_defaults_runtime::set_project_model_default,
            model_defaults_runtime::execution_profile_default_model,
            model_defaults_runtime::set_execution_profile_default_model,
            conversation_runtime::start_conversation_turn,
            conversation_runtime::enqueue_managed_agent_job,
            conversation_runtime::enqueue_conversation_side_request,
            conversation_runtime::conversation_side_requests,
            side_question_ipc::delete_conversation_side_question,
            conversation_runtime::composer_suggestion_mode,
            conversation_runtime::set_composer_suggestion_mode,
            conversation_runtime::latest_conversation_run,
            conversation_runtime::latest_conversation_job,
            conversation_runtime::pending_conversation_instructions,
            conversation_runtime::steer_pending_instruction,
            conversation_steering_ipc::preview_conversation_steering,
            conversation_steering_ipc::steer_queued_conversation_instruction,
            conversation_steering_ipc::conversation_instruction_deliveries,
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
            queue_runtime::interrupted_writer_review,
            queue_runtime::reconcile_interrupted_writer,
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
            conversation_runtime::conversation_approvals,
            ipc::conversation_question_timeline,
            ipc::conversation_agent_tree,
            conversation_runtime::conversation_input,
            conversation_runtime::conversation_attachment_content,
            conversation_runtime::capture_conversation_excerpt,
            conversation_runtime::enqueue_conversation_summary,
            conversation_runtime::conversation_artifact_revision,
            conversation_runtime::conversation_dispatch_inventory,
            conversation_runtime::conversation_profile_evidence,
            conversation_runtime::conversation_generated_image_evidence,
            conversation_runtime::qualify_conversation_generated_image,
            conversation_runtime::conversation_output_fixture_audio,
            conversation_runtime::conversation_checklist_progress,
            conversation_runtime::set_conversation_checklist_item_checked,
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
