use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use omniagent::{
    attachments::AttachmentLimits,
    conversations::{Conversation, ConversationHistory, MessageRevision},
    customizations::CustomizationInventory,
    evidence::FixtureInputEvidence,
    jobs::{
        AttemptSubagentAudit, ConversationSubagentAudit, ConversationSubagentQuota, ProjectSubagentPolicy,
        QueueHistoryPolicy, QueueHistorySettings, SubagentAuditPolicy, SubagentLimitMode,
    },
    providers::{
        Provider,
        plugins::{PluginInventory, probe_plugins},
        profiles::{CliProbe, ConfiguredExecutable, probe_cli},
    },
    repository::ProjectCheckout,
    runs::{RunRecord, RunSnapshot, SubscriptionUpdate},
    store::Store,
};
use tauri::{State, ipc::Channel};
use tokio::sync::oneshot;
use uuid::Uuid;

#[tauri::command]
pub async fn run_orchestration(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    conversation_id: Option<String>,
) -> Result<Option<omniagent::jobs::RunOrchestration>, String> {
    store
        .run_orchestration(project_id, run_id, conversation_id)
        .await
        .map_err(|error| omniagent::diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn conversation_question_timeline(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<Vec<omniagent::jobs::ConversationQuestionTimelineEntry>, String> {
    store
        .conversation_question_timeline(conversation_id, run_id)
        .await
        .map_err(|error| omniagent::diagnostics::preview(&error.to_string()))
}

#[derive(Default)]
pub struct EventSubscriptions(Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>);

#[tauri::command]
pub async fn list_conversations(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<Conversation>, String> {
    store
        .list_conversations(checkout_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn checkout_conversation(
    store: State<'_, Store>,
    checkout_id: String,
    conversation_id: String,
) -> Result<Conversation, String> {
    let conversation = store
        .conversation(conversation_id)
        .await
        .map_err(|error| error.to_string())?;
    if conversation.checkout_id != checkout_id {
        return Err("Conversation does not belong to this checkout".into());
    }
    Ok(conversation)
}

#[tauri::command]
pub async fn create_conversation(
    store: State<'_, Store>,
    checkout_id: String,
    title: String,
) -> Result<Conversation, String> {
    store
        .create_conversation(checkout_id, title)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn fork_conversation(
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: String,
    through_revision: Option<String>,
    title: String,
) -> Result<Conversation, String> {
    store
        .fork_conversation(
            conversation_id,
            expected_context_id,
            through_revision,
            title,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn rename_conversation(
    store: State<'_, Store>,
    checkout_id: String,
    conversation_id: String,
    title: String,
) -> Result<Conversation, String> {
    let conversation = store
        .conversation(conversation_id.clone())
        .await
        .map_err(|error| error.to_string())?;
    if conversation.checkout_id != checkout_id {
        return Err("Conversation does not belong to this checkout".into());
    }
    store
        .rename_conversation(conversation_id, title.trim().to_owned())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn conversation_messages(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<MessageRevision>, String> {
    store
        .conversation_messages(conversation_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn conversation_history(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<ConversationHistory, String> {
    store
        .conversation_history(conversation_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn rewind_conversation_history(
    store: State<'_, Store>,
    conversation_id: String,
    base_id: String,
    through_revision: Option<String>,
) -> Result<String, String> {
    store
        .rewind_conversation_history(conversation_id, base_id, through_revision)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn project_queue_history_policy(
    store: State<'_, Store>,
    project_id: String,
) -> Result<QueueHistoryPolicy, String> {
    store
        .project_queue_history_policy(project_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_project_queue_history_policy(
    store: State<'_, Store>,
    project_id: String,
    policy: QueueHistoryPolicy,
) -> Result<(), String> {
    store
        .set_project_queue_history_policy(project_id, policy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn conversation_queue_history_policy(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<QueueHistorySettings, String> {
    store
        .conversation_queue_history_policy(conversation_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_conversation_queue_history_policy(
    store: State<'_, Store>,
    conversation_id: String,
    policy: Option<QueueHistoryPolicy>,
) -> Result<QueueHistorySettings, String> {
    store
        .set_conversation_queue_history_policy(conversation_id, policy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn project_question_timeout(
    store: State<'_, Store>,
    project_id: String,
) -> Result<u32, String> {
    store
        .project_question_timeout(project_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_project_question_timeout(
    store: State<'_, Store>,
    project_id: String,
    minutes: u32,
) -> Result<(), String> {
    store
        .set_project_question_timeout(project_id, minutes)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn project_subagent_policy(
    store: State<'_, Store>,
    project_id: String,
) -> Result<ProjectSubagentPolicy, String> {
    store
        .project_subagent_policy(project_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_project_subagent_policy(
    store: State<'_, Store>,
    project_id: String,
    policy: ProjectSubagentPolicy,
) -> Result<(), String> {
    store
        .set_project_subagent_policy(project_id, policy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn conversation_subagent_quota(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: Option<String>,
) -> Result<ConversationSubagentQuota, String> {
    let result = match run_id {
        Some(run) => store.conversation_run_subagent_quota(conversation_id, run).await,
        None => store.conversation_subagent_quota(conversation_id).await,
    };
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_conversation_subagent_quota(
    store: State<'_, Store>,
    conversation_id: String,
    mode_override: Option<SubagentLimitMode>,
    requested: u32,
    native_requested: Option<u32>,
) -> Result<ConversationSubagentQuota, String> {
    store
        .set_conversation_subagent_quota(
            conversation_id,
            mode_override,
            requested,
            native_requested,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn project_subagent_audit(
    store: State<'_, Store>,
    project_id: String,
) -> Result<SubagentAuditPolicy, String> {
    store
        .project_subagent_audit(project_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_project_subagent_audit(
    store: State<'_, Store>,
    project_id: String,
    policy: SubagentAuditPolicy,
) -> Result<(), String> {
    store
        .set_project_subagent_audit(project_id, policy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn conversation_subagent_audit(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<ConversationSubagentAudit, String> {
    store
        .conversation_subagent_audit(conversation_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn attempt_subagent_audit(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
) -> Result<Option<AttemptSubagentAudit>, String> {
    store.attempt_subagent_audit(conversation_id, run_id).await.map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_conversation_subagent_audit(
    store: State<'_, Store>,
    conversation_id: String,
    policy: Option<SubagentAuditPolicy>,
) -> Result<ConversationSubagentAudit, String> {
    store
        .set_conversation_subagent_audit(conversation_id, policy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn conversation_draft(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<String, String> {
    store
        .conversation_draft(conversation_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_conversation_draft(
    store: State<'_, Store>,
    conversation_id: String,
    text: String,
) -> Result<(), String> {
    store
        .save_conversation_draft(conversation_id, text)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cli_executables(store: State<'_, Store>) -> Result<Vec<ConfiguredExecutable>, String> {
    let mut paths = Vec::new();
    for provider in Provider::ALL {
        paths.push(
            store
                .configured_executable(provider)
                .await
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(paths)
}

#[tauri::command]
pub async fn set_cli_executable(
    store: State<'_, Store>,
    provider: Provider,
    executable: Option<PathBuf>,
) -> Result<ConfiguredExecutable, String> {
    store
        .set_cli_executable(provider, executable)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn project_attachment_limits(
    store: State<'_, Store>,
    project_id: String,
) -> Result<AttachmentLimits, String> {
    store
        .project_attachment_limits(project_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_project_attachment_limits(
    store: State<'_, Store>,
    project_id: String,
    limits: AttachmentLimits,
) -> Result<(), String> {
    store
        .set_project_attachment_limits(project_id, limits)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn probe_cli_executable(
    store: State<'_, Store>,
    provider: Provider,
) -> Result<CliProbe, String> {
    let configured = store
        .configured_executable(provider)
        .await
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || probe_cli(configured))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cli_plugin_inventory(
    store: State<'_, Store>,
    provider: Provider,
    refresh: bool,
) -> Result<Option<PluginInventory>, String> {
    if !refresh {
        return store
            .latest_plugin_inventory(provider)
            .await
            .map_err(|error| error.to_string());
    }
    let configured = store
        .configured_executable(provider)
        .await
        .map_err(|error| error.to_string())?;
    let report = tauri::async_runtime::spawn_blocking(move || probe_plugins(configured))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    store
        .save_plugin_inventory(report)
        .await
        .map(Some)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn inspect_conversation_run(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: Option<String>,
    run_id: String,
) -> Result<(RunRecord, String), String> {
    store
        .inspect_conversation_run(project_id, conversation_id, run_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn registered_checkouts(store: State<'_, Store>) -> Result<Vec<ProjectCheckout>, String> {
    store
        .registered_checkouts()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn active_checkout(store: State<'_, Store>) -> Result<Option<ProjectCheckout>, String> {
    store
        .active_checkout()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn review_checkout_customizations(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<CustomizationInventory, String> {
    store
        .refresh_checkout_customizations(checkout_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn approve_checkout_customization(
    store: State<'_, Store>,
    checkout_id: String,
    entry_id: String,
    fingerprint: String,
) -> Result<CustomizationInventory, String> {
    store
        .approve_checkout_customization(checkout_id, entry_id, fingerprint)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn select_active_checkout(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<(), String> {
    store
        .select_active_checkout(checkout_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn workbench_layout(
    store: State<'_, Store>,
    checkout_id: String,
    workspace: String,
) -> Result<Option<String>, String> {
    store
        .workbench_layout(checkout_id, workspace)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_workbench_layout(
    store: State<'_, Store>,
    checkout_id: String,
    workspace: String,
    layout: String,
) -> Result<(), String> {
    store
        .save_workbench_layout(checkout_id, workspace, layout)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn run_snapshot(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<RunSnapshot, String> {
    store
        .run_snapshot(checkout_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn subscribe_events(
    store: State<'_, Store>,
    subscriptions: State<'_, EventSubscriptions>,
    checkout_id: String,
    after_seq: String,
    channel: Channel<SubscriptionUpdate>,
) -> Result<String, String> {
    let mut feed = store
        .subscribe_events(checkout_id, &after_seq)
        .map_err(|error| error.to_string())?;
    let id = Uuid::new_v4().to_string();
    let (cancel, mut cancelled) = oneshot::channel();
    let active = subscriptions.0.clone();
    active
        .lock()
        .expect("subscriptions lock")
        .insert(id.clone(), cancel);
    let subscription_id = id.clone();
    tauri::async_runtime::spawn(async move {
        'forward: loop {
            let batch = tokio::select! {
                biased;
                _ = &mut cancelled => break,
                batch = feed.next_batch() => batch,
            };
            match batch {
                Ok(events) => {
                    for event in events {
                        if channel.send(SubscriptionUpdate::Event { event }).is_err() {
                            break 'forward;
                        }
                    }
                }
                Err(error) => {
                    let _ = channel.send(SubscriptionUpdate::Failed {
                        message: error.to_string(),
                    });
                    break;
                }
            }
        }
        active
            .lock()
            .expect("subscriptions lock")
            .remove(&subscription_id);
    });
    Ok(id)
}

#[tauri::command]
pub async fn fixture_input_evidence(
    store: State<'_, Store>,
    checkout_id: String,
    run_id: String,
) -> Result<Option<FixtureInputEvidence>, String> {
    store
        .fixture_input(checkout_id, run_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn unsubscribe_events(subscriptions: State<'_, EventSubscriptions>, id: String) {
    if let Some(cancel) = subscriptions
        .0
        .lock()
        .expect("subscriptions lock")
        .remove(&id)
    {
        let _ = cancel.send(());
    }
}
