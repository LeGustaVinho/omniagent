use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{
    sync::watch,
    task::{JoinHandle, JoinSet},
};

use crate::{
    diagnostics,
    providers::runtime::{ManagedTurnControl, start_managed_turn},
    runner::Runner,
    runs::RunStatus,
    store::Store,
};

use super::AdmissionEnvironment;

#[derive(Clone)]
pub struct JobScheduler {
    runner: Arc<dyn Runner>,
    bridge: Option<PathBuf>,
    active: ActiveTurns,
    stop: watch::Sender<bool>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

type ActiveTurns = Arc<Mutex<HashMap<String, LiveTurn>>>;

struct LiveTurn {
    conversation: String,
    control: ManagedTurnControl,
    interrupt_sent: bool,
}

impl JobScheduler {
    pub fn start(store: Store, runner: Arc<dyn Runner>, bridge: Option<PathBuf>) -> Self {
        let (stop, stopping) = watch::channel(false);
        let active = ActiveTurns::default();
        let task = tokio::spawn(drive(
            store,
            runner.clone(),
            bridge.clone(),
            active.clone(),
            stopping,
        ));
        Self {
            runner,
            bridge,
            active,
            stop,
            task: Arc::new(tokio::sync::Mutex::new(Some(task))),
        }
    }

    pub fn environment(&self) -> AdmissionEnvironment {
        environment(self.runner.as_ref(), self.bridge.as_ref())
    }

    pub fn active_turn(&self, conversation: &str, run: &str) -> Result<ManagedTurnControl> {
        self.active
            .lock()
            .expect("job scheduler lock")
            .get(run)
            .filter(|turn| turn.conversation == conversation)
            .map(|turn| turn.control.clone())
            .context("This conversation run is no longer active")
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop.send_replace(true);
        if let Some(task) = self.task.lock().await.take() {
            task.await.context("job scheduler task failed")?;
        }
        Ok(())
    }
}

fn environment(runner: &dyn Runner, bridge: Option<&PathBuf>) -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: bridge.is_some_and(|path| path.is_absolute() && path.is_file()),
    }
}

async fn drive(
    store: Store,
    runner: Arc<dyn Runner>,
    bridge: Option<PathBuf>,
    active: ActiveTurns,
    mut stopping: watch::Receiver<bool>,
) {
    let mut signal = store.job_signal.subscribe();
    let mut workers = JoinSet::new();
    loop {
        let stop = *stopping.borrow_and_update();
        if stop {
            let mut live = active.lock().expect("job scheduler lock");
            for turn in live.values_mut() {
                if !turn.interrupt_sent {
                    turn.interrupt_sent = turn.control.interrupt().is_ok();
                }
            }
            if workers.is_empty() {
                break;
            }
        } else {
            // Observe changes before reading, so a submission between the read and wait wakes us.
            signal.borrow_and_update();
            match pending_interrupts(&store).await {
                Ok(ids) => {
                    let mut live = active.lock().expect("job scheduler lock");
                    for id in ids {
                        if let Some(turn) = live.get_mut(&id)
                            && !turn.interrupt_sent
                        {
                            turn.interrupt_sent = turn.control.interrupt().is_ok();
                        }
                    }
                }
                Err(error) => report(&error),
            }
            if !*stopping.borrow() {
                match store
                    .claim_next_job(environment(runner.as_ref(), bridge.as_ref()))
                    .await
                {
                    Ok(Some(plan)) => {
                        let run_id = plan.run.id.clone();
                        if *stopping.borrow() {
                            if let Err(error) = store
                                .finish_agent_attempt(run_id, RunStatus::Cancelled, None)
                                .await
                            {
                                report(&error);
                            }
                        } else {
                            let conversation = plan.conversation_id.clone();
                            let worker = start_managed_turn(
                                store.clone(),
                                runner.clone(),
                                plan,
                                bridge.clone(),
                            );
                            active.lock().expect("job scheduler lock").insert(
                                run_id.clone(),
                                LiveTurn {
                                    conversation,
                                    control: worker.control(),
                                    interrupt_sent: false,
                                },
                            );
                            let active = active.clone();
                            workers.spawn(async move {
                                let result = worker.wait().await;
                                active.lock().expect("job scheduler lock").remove(&run_id);
                                if let Err(error) = result {
                                    report(&error);
                                }
                            });
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => report(&error),
                }
            }
        }
        tokio::select! {
            change = stopping.changed(), if !stop => {
                if change.is_err() { stopping = watch::channel(true).1; }
            }
            _ = signal.changed(), if !stop => {},
            _ = workers.join_next(), if !workers.is_empty() => {},
            // Recheck local executable and runner readiness after external changes.
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

async fn pending_interrupts(store: &Store) -> Result<Vec<String>> {
    store.execute(|db| {
        let mut query = db.prepare("SELECT j.run_id FROM job_attempts j JOIN active_agent_attempts a ON a.run_id = j.run_id WHERE j.cancel_requested = 1")?;
        Ok(query.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?)
    }).await
}

fn report(error: &anyhow::Error) {
    eprintln!(
        "Job scheduler: {}",
        diagnostics::preview(&error.to_string())
    );
}
