use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    diagnostics,
    jobs::{
        Job, JobContext, JobInspection, JobScheduler, PendingInstruction, ProjectQueue, QueueState,
        RecoveryAction,
    },
    model_catalog::ModelSelection,
    providers::{
        ApprovalDecision, Provider, ProviderEvent, ProviderEventData, RequestId, SessionMode,
        profiles::{DESKTOP_PERMISSION_REVISION, DesktopModel, DesktopProfile, desktop_profile},
        questions::{QuestionOrigin, QuestionReply},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunRecord,
    store::Store,
};
use serde::Serialize;
use serde_json::Value;
use tauri::State;

async fn profile(
    store: &Store,
    provider: Provider,
    model_id: Option<String>,
) -> Result<DesktopProfile> {
    let configured = store.configured_executable(provider).await?;
    let model_id = model_id.filter(|_| provider == Provider::Antigravity);
    Ok(
        tauri::async_runtime::spawn_blocking(move || desktop_profile(configured, model_id))
            .await??,
    )
}

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn conversation_models(
    store: State<'_, Store>,
    provider: Provider,
    model_id: Option<String>,
) -> Result<Vec<DesktopModel>, String> {
    profile(&store, provider, model_id)
        .await
        .map(|profile| profile.models)
        .map_err(ui_error)
}

#[tauri::command]
pub async fn start_conversation_turn(
    store: State<'_, Store>,
    conversation_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    text: String,
    expected_context_id: Option<String>,
) -> Result<Job, String> {
    async {
        ensure!(
            matches!(mode, SessionMode::ReadOnly | SessionMode::Plan),
            "Direct conversation turns require a read-only or planning profile"
        );
        let conversation = store.conversation(conversation_id.clone()).await?;
        let DesktopProfile {
            executable,
            version,
            models,
        } = profile(&store, selection.provider, Some(selection.model_id.clone())).await?;
        let selected = models
            .into_iter()
            .find(|model| model.option.selection == selection && model.option.selectable)
            .context("The selected model is no longer available")?;
        ensure!(
            effort
                .as_ref()
                .is_none_or(|value| selected.efforts.contains(value)),
            "The selected thinking level is no longer available"
        );
        let checkout = repository::checkout_root(&store, conversation.checkout_id).await?;
        let scope = SessionScope {
            provider: selection.provider,
            profile_id: selection.profile_id,
            account_binding: None,
            checkout,
            executable,
            cli_version: version,
            runner: DirectWindowsRunner.identity().into(),
            permission_revision: DESKTOP_PERMISSION_REVISION.into(),
            mode,
            model: Some(selection.model_id),
            effort,
        };
        store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id,
                    context_id: expected_context_id.unwrap_or(conversation.context_id),
                },
                scope,
                text,
            )
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn latest_conversation_job(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
) -> Result<Option<JobInspection>, String> {
    store
        .latest_conversation_job(conversation_id, scheduler.environment())
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn pending_conversation_instructions(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
) -> Result<Vec<PendingInstruction>, String> {
    store
        .pending_conversation_instructions(conversation_id, scheduler.environment())
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn steer_pending_instruction(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
    run_id: String,
    instruction_id: String,
    revision_id: String,
) -> Result<bool, String> {
    scheduler
        .steer_pending_instruction(&store, conversation_id, run_id, instruction_id, revision_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn resolve_uncertain_steering(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
    instruction_id: String,
    revision_id: String,
    confirmed_undelivered: bool,
) -> Result<(), String> {
    store
        .resolve_uncertain_steering(
            conversation_id,
            instruction_id,
            revision_id,
            run_id,
            confirmed_undelivered,
        )
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn revise_pending_instruction(
    store: State<'_, Store>,
    conversation_id: String,
    instruction_id: String,
    revision_id: String,
    text: String,
) -> Result<(), String> {
    store
        .revise_pending_instruction(conversation_id, instruction_id, revision_id, text, None)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn remove_pending_instruction(
    store: State<'_, Store>,
    conversation_id: String,
    instruction_id: String,
    revision_id: String,
) -> Result<(), String> {
    store
        .remove_pending_instruction(conversation_id, instruction_id, revision_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn project_job_queue(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    project_id: String,
) -> Result<ProjectQueue, String> {
    store
        .inspect_project_queue(project_id, scheduler.environment())
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn cancel_conversation_job(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
    job_id: String,
) -> Result<(), String> {
    async {
        let inspection = store
            .latest_conversation_job(conversation_id, scheduler.environment())
            .await?
            .filter(|entry| entry.job.id == job_id)
            .context("This job is no longer pending in the conversation")?;
        store
            .set_pending_job_state(inspection.job.project_id, job_id, QueueState::Cancelled)
            .await?;
        Ok(())
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn recover_conversation_job(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
    job_id: String,
    run_id: String,
    action: RecoveryAction,
) -> Result<Job, String> {
    async {
        let inspection = store
            .latest_conversation_job(conversation_id, scheduler.environment())
            .await?
            .filter(|entry| entry.job.id == job_id)
            .context("This job is no longer current in the conversation")?;
        store
            .queue_job_recovery(inspection.job.project_id, job_id, run_id, action)
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn latest_conversation_run(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Option<RunRecord>, String> {
    store
        .latest_conversation_run(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn stop_conversation_turn(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<(), String> {
    store
        .request_job_interrupt(conversation_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn approve_conversation_input(
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
    run_id: String,
    request_id: RequestId,
    decision: ApprovalDecision,
) -> Result<(), String> {
    let control = scheduler
        .active_turn(&conversation_id, &run_id)
        .map_err(ui_error)?;
    control
        .approve(request_id, decision)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn answer_conversation_question(
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
    run_id: String,
    request_id: RequestId,
    answer: Value,
) -> Result<(), String> {
    let control = scheduler
        .active_turn(&conversation_id, &run_id)
        .map_err(ui_error)?;
    control.answer(request_id, answer).await.map_err(ui_error)
}

#[tauri::command]
pub async fn answer_saved_conversation_question(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
    request_id: RequestId,
    answer: QuestionReply,
) -> Result<(), String> {
    store
        .answer_saved_question(
            conversation_id,
            QuestionOrigin { run_id, request_id },
            answer,
        )
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn pending_conversation_inputs(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<Vec<ProviderEvent>, String> {
    store
        .pending_conversation_inputs(conversation_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn saved_conversation_questions(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<Vec<omniagent::jobs::SavedConversationQuestion>, String> {
    store
        .saved_conversation_questions(conversation_id, run_id)
        .await
        .map_err(ui_error)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPage {
    cursor: String,
    more: bool,
    events: Vec<ProviderEvent>,
}

#[tauri::command]
pub async fn conversation_events(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
    after: String,
) -> Result<ConversationPage, String> {
    let cursor = after
        .parse::<i64>()
        .map_err(|_| "Invalid event cursor".to_owned())?;
    let rows = store
        .agent_observations(conversation_id, run_id, cursor)
        .await
        .map_err(ui_error)?;
    let next = rows.last().map_or(cursor, |row| row.seq);
    let more = rows.len() == 128;
    let events = rows
        .into_iter()
        .map(|row| row.event)
        .filter(|event| {
            matches!(
                event.data,
                ProviderEventData::AssistantDelta { .. }
                    | ProviderEventData::AssistantMessage { .. }
                    | ProviderEventData::PlanProposed { .. }
                    | ProviderEventData::Error { .. }
                    | ProviderEventData::Tool { .. }
                    | ProviderEventData::Reasoning { .. }
                    | ProviderEventData::Usage { .. }
                    | ProviderEventData::ContextWindowUsage { .. }
                    | ProviderEventData::RuntimeInspected { .. }
                    | ProviderEventData::DownstreamWaitRequested { .. }
                    | ProviderEventData::DownstreamContinuation { .. }
                    | ProviderEventData::ChildActivity { .. }
                    | ProviderEventData::SubagentAuditGap { .. }
            )
        })
        .collect();
    Ok(ConversationPage {
        cursor: next.to_string(),
        more,
        events,
    })
}

#[derive(Serialize)]
pub struct ConversationInput {
    delivery: omniagent::dispatch::Delivery,
    frames: Vec<String>,
}

#[tauri::command]
pub async fn conversation_input(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<Option<ConversationInput>, String> {
    let dispatch = store
        .agent_dispatch(conversation_id, run_id)
        .await
        .map_err(ui_error)?;
    dispatch
        .map(|dispatch| {
            let frames = dispatch
                .frames
                .into_iter()
                .map(String::from_utf8)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| "The dispatch contains a non-UTF-8 frame".to_owned())?;
            Ok(ConversationInput {
                delivery: dispatch.delivery,
                frames,
            })
        })
        .transpose()
}
