use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{
    sync::{Mutex as AsyncMutex, MutexGuard, watch},
    task::{JoinHandle, JoinSet},
};

use crate::{
    diagnostics,
    providers::runtime::{ManagedTurn, ManagedTurnControl, start_scheduled_turn},
    runner::Runner,
    store::Store,
};

use super::{AdmissionEnvironment, admission::ScheduledAttempt, command::start_command};

#[derive(Clone)]
pub struct JobScheduler {
    runner: Arc<dyn Runner>,
    bridge: Option<PathBuf>,
    active: ActiveTurns,
    stop: watch::Sender<bool>,
    admission: LaunchGate,
    task: Arc<AsyncMutex<Option<JoinHandle<()>>>>,
}

#[derive(Clone)]
pub(crate) struct LaunchGate {
    boundary: Arc<AsyncMutex<()>>,
    stopping: watch::Receiver<bool>,
}

impl LaunchGate {
    pub(crate) fn is_closed(&self) -> bool {
        *self.stopping.borrow() || self.stopping.has_changed().is_err()
    }

    pub(crate) async fn enter(&self) -> Option<MutexGuard<'_, ()>> {
        let guard = self.boundary.lock().await;
        (!self.is_closed()).then_some(guard)
    }
}

type ActiveTurns = Arc<Mutex<HashMap<String, LiveTurn>>>;

struct LiveTurn {
    control: ActiveControl,
    interrupt_sent: bool,
}

enum ActiveControl {
    Agent {
        conversation: String,
        control: ManagedTurnControl,
    },
    Command(watch::Sender<bool>),
}

impl ActiveControl {
    fn request_stop(&self, cancelled: bool) -> bool {
        match self {
            Self::Agent { control, .. } if !cancelled => control.yield_to_downstream().is_ok(),
            Self::Agent { control, .. } => control.interrupt().is_ok(),
            Self::Command(signal) => signal.send(true).is_ok(),
        }
    }
}

enum Worker {
    Agent(ManagedTurn),
    Command(JoinHandle<Result<()>>),
}

impl Worker {
    async fn wait(self) -> Result<()> {
        match self {
            Self::Agent(worker) => worker.wait().await.map(|_| ()),
            Self::Command(worker) => worker.await.context("Command job worker failed")?,
        }
    }
}

impl JobScheduler {
    pub fn start(store: Store, runner: Arc<dyn Runner>, bridge: Option<PathBuf>) -> Self {
        let (stop, stopping) = watch::channel(false);
        let admission = LaunchGate {
            boundary: Arc::new(AsyncMutex::new(())),
            stopping,
        };
        let active = ActiveTurns::default();
        let task = tokio::spawn(drive(
            store,
            runner.clone(),
            bridge.clone(),
            active.clone(),
            admission.clone(),
        ));
        Self {
            runner,
            bridge,
            active,
            stop,
            admission,
            task: Arc::new(AsyncMutex::new(Some(task))),
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
            .and_then(|turn| match &turn.control {
                ActiveControl::Agent {
                    conversation: owner,
                    control,
                } if owner == conversation => Some(control.clone()),
                _ => None,
            })
            .context("This conversation run is no longer active")
    }

    /// Reports whether the scheduler attempted the native route. Delivery settles in run events.
    pub async fn steer_pending_instruction(
        &self,
        store: &Store,
        conversation: String,
        run: String,
        instruction: String,
        revision: String,
    ) -> Result<bool> {
        let Ok(control) = self.active_turn(&conversation, &run) else {
            return Ok(false);
        };
        let Some(delivery) = store
            .claim_native_instruction(conversation, instruction, revision.clone(), run.clone())
            .await?
        else {
            return Ok(false);
        };
        if let Err(error) = control.steer(delivery.target, delivery.input).await {
            if store.release_unsent_steering(run, revision).await? {
                return Ok(false);
            }
            return Err(error);
        }
        Ok(true)
    }

    pub async fn stop_admission(&self) -> bool {
        let Some(_boundary) = self.admission.enter().await else {
            return false;
        };
        self.stop.send_replace(true);
        true
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop_admission().await;
        let mut task = self.task.lock().await;
        if let Some(worker) = task.as_mut() {
            let result = worker.await;
            task.take();
            result.context("job scheduler task failed")?;
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
    admission: LaunchGate,
) {
    let mut signal = store.job_signal.subscribe();
    let mut stopping = admission.stopping.clone();
    let mut workers = JoinSet::new();
    loop {
        stopping.borrow_and_update();
        let stop = admission.is_closed();
        if stop {
            if workers.is_empty() {
                break;
            }
        } else {
            // Observe changes before reading, so a submission between the read and wait wakes us.
            signal.borrow_and_update();
            match pending_stops(&store).await {
                Ok(stops) => {
                    let mut live = active.lock().expect("job scheduler lock");
                    for (id, cancelled) in stops {
                        if let Some(turn) = live.get_mut(&id)
                            && !turn.interrupt_sent
                        {
                            turn.interrupt_sent = turn.control.request_stop(cancelled);
                        }
                    }
                }
                Err(error) => report(&error),
            }
            if let Some(_boundary) = admission.enter().await {
                match store
                    .claim_scheduled_attempt(environment(runner.as_ref(), bridge.as_ref()), true)
                    .await
                {
                    Ok(Some(plan)) => {
                        let (run_id, control, worker) = match plan {
                            ScheduledAttempt::Agent(plan) => {
                                let run_id = plan.run.id.clone();
                                let conversation = plan.conversation_id.clone();
                                let worker = start_scheduled_turn(
                                    store.clone(),
                                    runner.clone(),
                                    *plan,
                                    bridge.clone(),
                                    admission.clone(),
                                );
                                (
                                    run_id,
                                    ActiveControl::Agent {
                                        conversation,
                                        control: worker.control(),
                                    },
                                    Worker::Agent(worker),
                                )
                            }
                            ScheduledAttempt::Command(attempt) => {
                                let run_id = attempt.run.id.clone();
                                let (control, worker) = start_command(
                                    store.clone(),
                                    runner.clone(),
                                    attempt,
                                    admission.clone(),
                                );
                                (
                                    run_id,
                                    ActiveControl::Command(control),
                                    Worker::Command(worker),
                                )
                            }
                        };
                        active.lock().expect("job scheduler lock").insert(
                            run_id.clone(),
                            LiveTurn {
                                control,
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
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => report(&error),
                }
            }
        }
        tokio::select! {
            _ = stopping.changed(), if !stop => {}
            _ = signal.changed(), if !stop => {},
            _ = workers.join_next(), if !workers.is_empty() => {},
            // Recheck local executable and runner readiness after external changes.
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

async fn pending_stops(store: &Store) -> Result<Vec<(String, bool)>> {
    store
        .execute(|db| {
            let mut query = db.prepare(
            "SELECT a.run_id, a.cancel_requested FROM job_attempts a JOIN runs r ON r.id = a.run_id
             WHERE r.status IN ('starting', 'running', 'waiting_for_input') AND (
                  a.cancel_requested = 1 OR EXISTS (
                      SELECT 1 FROM job_downstream_waits w WHERE w.parent_run_id = a.run_id
                      AND w.state = 'yielding' AND w.requested_at <= unixepoch('subsec') - 1
                  )
             )",
        )?;
            Ok(query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
}

fn report(error: &anyhow::Error) {
    eprintln!(
        "Job scheduler: {}",
        diagnostics::preview(&error.to_string())
    );
}
