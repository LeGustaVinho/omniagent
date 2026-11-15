use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    diagnostics,
    model_catalog::ModelSelection,
    providers::{
        ApprovalDecision, Provider, ProviderEvent, ProviderEventData, RequestId, SessionMode,
        profiles::{DesktopModel, DesktopProfile, desktop_profile},
        runtime::{ManagedTurnControl, start_managed_turn},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunRecord,
    store::Store,
};
use serde::Serialize;
use serde_json::Value;
use tauri::State;

#[derive(Clone, Default)]
pub struct ConversationRuns(Arc<Mutex<HashMap<String, (String, ManagedTurnControl)>>>);

impl ConversationRuns {
    fn active(&self, conversation: &str, run: &str) -> Result<ManagedTurnControl> {
        self.0
            .lock()
            .expect("conversation run lock")
            .get(run)
            .filter(|(owner, _)| owner == conversation)
            .map(|(_, control)| control.clone())
            .context("This conversation run is no longer active")
    }

    pub fn stop_all(&self) {
        for (_, control) in self.0.lock().expect("conversation run lock").values() {
            let _ = control.interrupt();
        }
    }
}

async fn profile(store: &Store, provider: Provider) -> Result<DesktopProfile> {
    let configured = store.configured_executable(provider).await?;
    Ok(tauri::async_runtime::spawn_blocking(move || desktop_profile(configured)).await??)
}

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn conversation_models(
    store: State<'_, Store>,
    provider: Provider,
) -> Result<Vec<DesktopModel>, String> {
    profile(&store, provider)
        .await
        .map(|profile| profile.models)
        .map_err(ui_error)
}

#[tauri::command]
pub async fn start_conversation_turn(
    store: State<'_, Store>,
    runs: State<'_, ConversationRuns>,
    conversation_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    text: String,
) -> Result<RunRecord, String> {
    async {
        ensure!(
            matches!(mode, SessionMode::ReadOnly | SessionMode::Plan),
            "Direct conversation turns require a read-only or planning profile"
        );
        let DesktopProfile {
            executable,
            version,
            models,
        } = profile(&store, selection.provider).await?;
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
        let conversation = store.conversation(conversation_id.clone()).await?;
        let checkout = repository::checkout_root(&store, conversation.checkout_id).await?;
        let scope = SessionScope {
            provider: selection.provider,
            profile_id: selection.profile_id,
            account_binding: None,
            checkout,
            executable,
            cli_version: version,
            runner: DirectWindowsRunner.identity().into(),
            permission_revision: "desktop-direct-v1".into(),
            mode,
            model: Some(selection.model_id),
            effort,
        };
        let bridge = std::env::current_exe().ok();
        let plan = store
            .prepare_conversation_draft_attempt(
                conversation_id.clone(),
                conversation.context_id,
                scope,
                text,
            )
            .await?;
        let run = plan.run.clone();
        let worker = start_managed_turn(
            store.inner().clone(),
            Arc::new(DirectWindowsRunner),
            plan,
            bridge,
        );
        let handles = runs.0.clone();
        handles
            .lock()
            .expect("conversation run lock")
            .insert(run.id.clone(), (conversation_id, worker.control()));
        let run_id = run.id.clone();
        tauri::async_runtime::spawn(async move {
            let _ = worker.wait().await;
            handles
                .lock()
                .expect("conversation run lock")
                .remove(&run_id);
        });
        Ok(run)
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
pub fn stop_conversation_turn(
    runs: State<'_, ConversationRuns>,
    conversation_id: String,
    run_id: String,
) -> Result<(), String> {
    runs.active(&conversation_id, &run_id)
        .and_then(|control| control.interrupt())
        .map_err(ui_error)
}

#[tauri::command]
pub async fn approve_conversation_input(
    runs: State<'_, ConversationRuns>,
    conversation_id: String,
    run_id: String,
    request_id: RequestId,
    decision: ApprovalDecision,
) -> Result<(), String> {
    let control = runs.active(&conversation_id, &run_id).map_err(ui_error)?;
    control
        .approve(request_id, decision)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn answer_conversation_question(
    runs: State<'_, ConversationRuns>,
    conversation_id: String,
    run_id: String,
    request_id: RequestId,
    answer: Value,
) -> Result<(), String> {
    let control = runs.active(&conversation_id, &run_id).map_err(ui_error)?;
    control.answer(request_id, answer).await.map_err(ui_error)
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
                    | ProviderEventData::ChildActivity { .. }
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
