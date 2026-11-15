use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use omniagent::{
    evidence::FixtureInputEvidence,
    runs::{RunSnapshot, SubscriptionUpdate},
    store::Store,
};
use tauri::{State, ipc::Channel};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Default)]
pub struct EventSubscriptions(Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>);

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
