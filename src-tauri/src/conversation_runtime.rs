use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::{
        Conversation, ConversationHistory, ConversationHistoryInspection,
        ConversationLibraryChange, ConversationLibraryEntry, ConversationLibraryMoveTarget,
        ConversationLibrarySection, ConversationLibrarySectionChange, SessionScope,
    },
    diagnostics,
    dispatch::{ConversationArtifactRevision, ConversationSelection, QueuedSummary},
    jobs::{
        ComposerSuggestionMode, GoalControlState, GoalStopAction, Job, JobContext, JobInspection,
        JobScheduler, PendingInstruction, ProjectQueue, QueueState, RecoveryAction,
        SavedSideRequest, SidePurpose, SideSubmission,
    },
    model_catalog::{ModelOption, ModelSelection},
    providers::{
        ApprovalDecision, Provider, ProviderEvent, ProviderEventData, RequestId, SessionMode,
        configuration::InstructionInventory,
        profiles::{DESKTOP_PERMISSION_REVISION, DesktopModel, DesktopProfile, desktop_profile},
        questions::{QuestionOrigin, QuestionReply},
        side::OutputFixture,
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
pub async fn conversation_history_inspection(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<ConversationHistoryInspection, String> {
    store
        .inspect_conversation_history(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_history_revision(
    store: State<'_, Store>,
    conversation_id: String,
    context_id: String,
) -> Result<ConversationHistory, String> {
    store
        .conversation_history_revision(conversation_id, context_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn fork_conversation_with_message_edit(
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: String,
    through_revision: String,
    title: String,
    text: String,
) -> Result<Conversation, String> {
    store
        .fork_conversation_with_message_edit(
            conversation_id,
            expected_context_id,
            through_revision,
            title,
            text,
        )
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn fork_retained_conversation(
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: String,
    source_context_id: String,
    through_revision: String,
    title: String,
) -> Result<Conversation, String> {
    store
        .fork_retained_conversation(
            conversation_id,
            expected_context_id,
            source_context_id,
            through_revision,
            title,
        )
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_next_prompt_instructions(
    store: State<'_, Store>,
    conversation_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
) -> Result<InstructionInventory, String> {
    async {
        let conversation = store.conversation(conversation_id.clone()).await?;
        let scope =
            selected_scope(&store, conversation.checkout_id, selection, effort, mode).await?;
        store
            .conversation_next_prompt_instructions(conversation_id, scope)
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_library(
    store: State<'_, Store>,
    query: Option<String>,
    project_id: String,
    archived: Option<bool>,
) -> Result<Vec<ConversationLibraryEntry>, String> {
    store
        .conversation_library_view(query, Some(project_id), archived.unwrap_or_default())
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_library_sections(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<ConversationLibrarySection>, String> {
    store
        .conversation_library_sections(Some(project_id))
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn organize_conversation_library_section(
    store: State<'_, Store>,
    project_id: String,
    section_id: String,
    change: ConversationLibrarySectionChange,
) -> Result<(), String> {
    store
        .organize_conversation_library_section(project_id, section_id, change)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn move_conversations_to_library_section(
    store: State<'_, Store>,
    project_id: String,
    section_id: Option<String>,
    selection: Vec<ConversationLibraryMoveTarget>,
) -> Result<usize, String> {
    store
        .move_conversations_to_library_section(project_id, section_id, selection)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn organize_conversation(
    store: State<'_, Store>,
    checkout_id: String,
    conversation_id: String,
    change: ConversationLibraryChange,
) -> Result<bool, String> {
    store
        .organize_conversation(checkout_id, conversation_id, change)
        .await
        .map_err(ui_error)
}

pub(crate) async fn selected_scope(
    store: &Store,
    checkout_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
) -> Result<SessionScope> {
    let (scope, _) =
        selected_scope_with_model_option(store, checkout_id, selection, effort, mode).await?;
    Ok(scope)
}

pub(crate) async fn selected_scope_with_model_option(
    store: &Store,
    checkout_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
) -> Result<(SessionScope, ModelOption)> {
    let DesktopProfile {
        executable,
        version,
        models,
        account_binding,
    } = profile(store, selection.provider, Some(selection.model_id.clone())).await?;
    let selected = choose_live_model(&models, &selection)?;
    ensure!(
        effort
            .as_ref()
            .is_none_or(|value| selected.efforts.contains(value)),
        "The selected thinking level is no longer available"
    );
    Ok((
        SessionScope {
            provider: selection.provider,
            profile_id: selection.profile_id,
            account_binding,
            checkout: repository::checkout_root(store, checkout_id).await?,
            executable,
            cli_version: version,
            runner: DirectWindowsRunner.identity().into(),
            permission_revision: DESKTOP_PERMISSION_REVISION.into(),
            mode,
            model: Some(selection.model_id),
            effort,
        },
        selected.option.clone(),
    ))
}

fn choose_live_model<'a>(
    models: &'a [DesktopModel],
    selection: &ModelSelection,
) -> Result<&'a DesktopModel> {
    models
        .iter()
        .find(|model| model.option.selection == *selection && model.option.selectable)
        .context("The selected model is no longer available")
}

pub(crate) async fn validate_model_choice(store: &Store, selection: &ModelSelection) -> Result<()> {
    let available = profile(store, selection.provider, Some(selection.model_id.clone())).await?;
    choose_live_model(&available.models, selection)?;
    Ok(())
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
        conversation.require_direct_turn()?;
        let scope =
            selected_scope(&store, conversation.checkout_id, selection, effort, mode).await?;
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
pub async fn enqueue_managed_agent_job(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    parent_run_id: Option<String>,
    selection: ModelSelection,
    mode: SessionMode,
    instructions: String,
) -> Result<Job, String> {
    async {
        let scope = selected_scope(&store, checkout_id.clone(), selection, None, mode).await?;
        store
            .enqueue_selected_agent_job(project_id, checkout_id, parent_run_id, scope, instructions)
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn capture_conversation_excerpt(
    store: State<'_, Store>,
    selection: ConversationSelection,
) -> Result<ConversationArtifactRevision, String> {
    store
        .capture_conversation_excerpt(selection)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn enqueue_conversation_summary(
    store: State<'_, Store>,
    source: ConversationSelection,
    selection: ModelSelection,
    effort: Option<String>,
) -> Result<QueuedSummary, String> {
    async {
        let conversation = store.conversation(source.conversation_id.clone()).await?;
        let scope = selected_scope(
            &store,
            conversation.checkout_id,
            selection,
            effort,
            SessionMode::ReadOnly,
        )
        .await?;
        store.enqueue_conversation_summary(source, scope).await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_artifact_revision(
    store: State<'_, Store>,
    conversation_id: String,
    artifact_id: String,
) -> Result<Option<ConversationArtifactRevision>, String> {
    store
        .conversation_artifact_revision(conversation_id, artifact_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn enqueue_conversation_side_request(
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: Option<String>,
    expected_source_run_id: Option<String>,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    purpose: SidePurpose,
    question: String,
    output_fixture: Option<OutputFixture>,
) -> Result<SideSubmission, String> {
    async {
        let chat = store.conversation(conversation_id.clone()).await?;
        let scope = selected_scope(&store, chat.checkout_id, selection, effort, mode).await?;
        store
            .enqueue_side_request_with_fixture(
                conversation_id,
                expected_context_id,
                expected_source_run_id,
                scope,
                purpose,
                question,
                output_fixture,
            )
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_output_fixture_audio(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: String,
    run_id: String,
) -> Result<Option<String>, String> {
    store
        .side_output_fixture_audio(project_id, conversation_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_side_requests(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<SavedSideRequest>, String> {
    store
        .saved_side_requests(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn composer_suggestion_mode(
    store: State<'_, Store>,
) -> Result<ComposerSuggestionMode, String> {
    store.composer_suggestion_mode().await.map_err(ui_error)
}

#[tauri::command]
pub async fn set_composer_suggestion_mode(
    store: State<'_, Store>,
    mode: ComposerSuggestionMode,
) -> Result<ComposerSuggestionMode, String> {
    store
        .set_composer_suggestion_mode(mode)
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
pub async fn request_goal_stop(
    store: State<'_, Store>,
    project_id: String,
    revision_id: String,
    action: GoalStopAction,
) -> Result<GoalControlState, String> {
    store
        .request_goal_stop(project_id, revision_id, action)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn resume_goal(
    store: State<'_, Store>,
    project_id: String,
    revision_id: String,
    review_token: String,
) -> Result<(), String> {
    store
        .resume_goal_after_review(project_id, revision_id, review_token)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn confirm_managed_goal_stop(
    store: State<'_, Store>,
    project_id: String,
    revision_id: String,
    run_id: String,
) -> Result<GoalControlState, String> {
    store
        .confirm_managed_goal_stop(project_id, revision_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn confirm_native_goal_stop(
    store: State<'_, Store>,
    project_id: String,
    revision_id: String,
    run_id: String,
) -> Result<GoalControlState, String> {
    store
        .confirm_native_goal_stop(project_id, revision_id, run_id)
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
pub async fn conversation_approvals(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<Vec<omniagent::conversations::ConversationApprovalEvidence>, String> {
    store
        .conversation_approvals(conversation_id, run_id)
        .await
        .map_err(ui_error)
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
        .filter(|row| {
            !matches!(
                row.event.data,
                ProviderEventData::ContextRead { .. }
                    | ProviderEventData::AssistantFinal { .. }
                    | ProviderEventData::ReasoningUsage { .. }
            ) || row.source == omniagent::dispatch::EventSource::Provider
        })
        .map(|row| row.event)
        .filter(|event| {
            matches!(
                event.data,
                ProviderEventData::AssistantDelta { .. }
                    | ProviderEventData::DispatchSubmitted { .. }
                    | ProviderEventData::ContextRead { .. }
                    | ProviderEventData::AssistantMessage { .. }
                    | ProviderEventData::AssistantFinal { .. }
                    | ProviderEventData::PlanProposed { .. }
                    | ProviderEventData::Error { .. }
                    | ProviderEventData::Tool { .. }
                    | ProviderEventData::Reasoning { .. }
                    | ProviderEventData::ReasoningUsage { .. }
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
#[serde(rename_all = "camelCase")]
pub struct ConversationInput {
    delivery: omniagent::dispatch::Delivery,
    turn_id: String,
    frames: Vec<String>,
    attachments: Vec<omniagent::dispatch::AttachmentEvidence>,
    removed_attachment_count: usize,
    history_payload_deleted: bool,
    instructions: Vec<omniagent::dispatch::InstructionDispatch>,
    conversation_references: Vec<omniagent::conversations::SubmittedConversationReference>,
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
                turn_id: dispatch.turn_id,
                frames,
                attachments: dispatch.attachments,
                removed_attachment_count: dispatch.removed_attachment_count,
                history_payload_deleted: dispatch.history_payload_deleted,
                instructions: dispatch.instructions,
                conversation_references: dispatch.snapshot.conversation_references,
            })
        })
        .transpose()
}

#[tauri::command]
pub async fn conversation_attachment_content(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
    index: usize,
) -> Result<Option<String>, String> {
    store
        .attachment_dispatch_content(conversation_id, run_id, index)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_dispatch_inventory(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<Option<omniagent::dispatch::DispatchInventory>, String> {
    store
        .dispatch_inventory(conversation_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_profile_evidence(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: String,
    run_id: String,
) -> Result<Option<omniagent::dispatch::DispatchProfileEvidence>, String> {
    store
        .dispatch_profile_evidence(project_id, conversation_id, run_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn desktop_capability_qualifications(
    store: State<'_, Store>,
    cursor: Option<omniagent::dispatch::QualificationCursor>,
    validation_checkout_id: Option<String>,
) -> Result<omniagent::dispatch::QualificationPage, String> {
    store
        .desktop_capability_qualifications(cursor, validation_checkout_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_generated_image_evidence(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: String,
    run_id: String,
    event_sequence: String,
) -> Result<Option<String>, String> {
    let sequence = event_sequence
        .parse::<i64>()
        .map_err(|_| "Invalid event sequence".to_owned())?;
    store
        .dispatch_generated_image(project_id, conversation_id, run_id, sequence)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn qualify_conversation_generated_image(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: String,
    run_id: String,
    event_sequence: String,
) -> Result<omniagent::providers::capabilities::Verification, String> {
    let sequence = event_sequence
        .parse::<i64>()
        .map_err(|_| "Invalid event sequence".to_owned())?;
    store
        .qualify_conversation_generated_image(project_id, conversation_id, run_id, sequence)
        .await
        .map_err(ui_error)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationChecklistProgress {
    access: omniagent::tasks::ChecklistProgressAccess,
    checklist: omniagent::tasks::TaskChecklist,
}

#[tauri::command]
pub async fn conversation_checklist_progress(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<ConversationChecklistProgress>, String> {
    let Some(access) = store
        .run_checklist_progress(project_id.clone(), run_id)
        .await
        .map_err(ui_error)?
    else {
        return Ok(None);
    };
    let checklist = store
        .project_checklist(project_id, access.task_id.clone())
        .await
        .map_err(ui_error)?;
    Ok(checklist.map(|checklist| ConversationChecklistProgress { access, checklist }))
}

#[tauri::command]
pub async fn set_conversation_checklist_item_checked(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    item_id: String,
    expected_revision: i64,
    checked: bool,
) -> Result<omniagent::tasks::ChecklistProgressReceipt, String> {
    store
        .set_project_checklist_item_checked(
            project_id,
            task_id,
            item_id,
            expected_revision,
            checked,
        )
        .await
        .map_err(ui_error)
}
