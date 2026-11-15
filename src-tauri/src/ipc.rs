use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use omniagent::{
    evidence::FixtureInputEvidence,
    providers::{
        Provider,
        plugins::{PluginInventory, probe_plugins},
        profiles::{CliProbe, ConfiguredExecutable, probe_cli},
    },
    repository::ProjectCheckout,
    runs::{RunSnapshot, SubscriptionUpdate},
    store::Store,
};
use tauri::{State, ipc::Channel};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Default)]
pub struct EventSubscriptions(Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>);

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
pub async fn active_checkout(store: State<'_, Store>) -> Result<Option<ProjectCheckout>, String> {
    store
        .active_checkout()
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
