use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::{
    sync::{Mutex as AsyncMutex, MutexGuard, OwnedMutexGuard, oneshot, watch},
    task::{JoinHandle, JoinSet},
};

use crate::{
    conversations::SessionScope,
    diagnostics,
    providers::runtime::{
        ManagedTurn, ManagedTurnControl, native_compaction, start_scheduled_turn,
    },
    runner::Runner,
    store::Store,
    workflows::{
        automation::PendingWorkflowAgent, conversation_turns::prepare_next_conversation_stage,
    },
};

use super::{
    AdmissionEnvironment, admission::ScheduledAttempt, command::start_command, side_requests,
};

#[derive(Clone)]
pub struct JobScheduler {
    runner: Arc<dyn Runner>,
    bridge: Option<PathBuf>,
    active: ActiveTurns,
    stop: watch::Sender<bool>,
    admission: LaunchGate,
    task: Arc<AsyncMutex<Option<JoinHandle<()>>>>,
    native_compactions: Arc<AsyncMutex<JoinSet<()>>>,
}

#[derive(Clone)]
pub(crate) struct LaunchGate {
    boundary: Arc<AsyncMutex<()>>,
    stopping: watch::Receiver<bool>,
    maintenance: Arc<MaintenanceGate>,
}

#[derive(Default)]
struct MaintenanceGate {
    draining: AtomicBool,
    external_work: AtomicUsize,
}

pub struct ExternalWork(Arc<MaintenanceGate>);

impl Drop for ExternalWork {
    fn drop(&mut self) {
        self.0.external_work.fetch_sub(1, Ordering::AcqRel);
    }
}

impl LaunchGate {
    pub(crate) fn is_closed(&self) -> bool {
        *self.stopping.borrow() || self.stopping.has_changed().is_err()
    }

    pub(crate) async fn enter(&self) -> Option<MutexGuard<'_, ()>> {
        let guard = self.boundary.lock().await;
        (!self.is_closed()).then_some(guard)
    }

    pub(crate) async fn enter_admission(&self) -> Option<MutexGuard<'_, ()>> {
        let guard = self.enter().await?;
        (!self.maintenance.draining.load(Ordering::Acquire)).then_some(guard)
    }
}

pub(crate) struct UpdateDrain {
    gate: LaunchGate,
    signal: watch::Sender<()>,
}

impl UpdateDrain {
    pub(crate) async fn lock_launch_boundary(&self) -> Result<OwnedMutexGuard<()>> {
        let boundary = self.gate.boundary.clone().lock_owned().await;
        ensure!(!self.gate.is_closed(), "The scheduler is exiting");
        Ok(boundary)
    }

    pub(crate) fn confirm(&self, db: &rusqlite::Connection) -> Result<()> {
        ensure!(!self.gate.is_closed(), "The scheduler is exiting");
        ensure!(
            self.gate.maintenance.external_work.load(Ordering::Acquire) == 0
                && !has_database_work(db)?,
            "Managed attempts or terminals are still active"
        );
        Ok(())
    }
}

impl Drop for UpdateDrain {
    fn drop(&mut self) {
        self.gate
            .maintenance
            .draining
            .store(false, Ordering::Release);
        self.signal.send_replace(());
    }
}

fn has_database_work(db: &rusqlite::Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS (SELECT 1 FROM active_agent_attempts)
          OR EXISTS (SELECT 1 FROM job_writer_reservations)
          OR EXISTS (SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id
                     WHERE r.status IN ('starting', 'running', 'waiting_for_input'))",
        [],
        |row| row.get(0),
    )?)
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
    Process(watch::Sender<bool>),
}

impl ActiveControl {
    fn request_stop(&self, cancelled: bool) -> bool {
        match self {
            Self::Agent { control, .. } if !cancelled => control.yield_to_downstream().is_ok(),
            Self::Agent { control, .. } => control.interrupt().is_ok(),
            Self::Process(signal) => signal.send(true).is_ok(),
        }
    }
}

enum Worker {
    Agent(ManagedTurn),
    Process(JoinHandle<Result<()>>),
}

impl Worker {
    async fn wait(self) -> Result<()> {
        match self {
            Self::Agent(worker) => worker.wait().await.map(|_| ()),
            Self::Process(worker) => worker.await.context("Process job worker failed")?,
        }
    }
}

impl JobScheduler {
    pub fn start(store: Store, runner: Arc<dyn Runner>, bridge: Option<PathBuf>) -> Self {
        let (stop, stopping) = watch::channel(false);
        let admission = LaunchGate {
            boundary: Arc::new(AsyncMutex::new(())),
            stopping,
            maintenance: Arc::new(MaintenanceGate::default()),
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
            native_compactions: Arc::new(AsyncMutex::new(JoinSet::new())),
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

    pub async fn admit_external_work(&self) -> Result<ExternalWork> {
        let _boundary = self
            .admission
            .enter_admission()
            .await
            .context("The scheduler is exiting or preparing an application update")?;
        let gate = self.admission.maintenance.clone();
        gate.external_work.fetch_add(1, Ordering::AcqRel);
        Ok(ExternalWork(gate))
    }

    pub(crate) async fn drain_for_update(
        &self,
        store: &Store,
        timeout: Duration,
    ) -> Result<UpdateDrain> {
        let drain = {
            let _boundary = self
                .admission
                .enter()
                .await
                .context("The scheduler is exiting")?;
            ensure!(
                !self
                    .admission
                    .maintenance
                    .draining
                    .swap(true, Ordering::AcqRel),
                "Another application update is already preparing to install"
            );
            UpdateDrain {
                gate: self.admission.clone(),
                signal: store.job_signal.clone(),
            }
        };
        tokio::time::timeout(timeout, async {
            loop {
                ensure!(!self.admission.is_closed(), "The scheduler is exiting");
                let workers_active = !self.active.lock().expect("job scheduler lock").is_empty();
                let external_active = self
                    .admission
                    .maintenance
                    .external_work
                    .load(Ordering::Acquire)
                    > 0;
                if !workers_active && !external_active {
                    let mut native = self.native_compactions.lock().await;
                    while let Some(result) = native.try_join_next() {
                        result.context("Native compaction worker failed")?;
                    }
                    if native.is_empty() && !store.execute(|db| has_database_work(db)).await? {
                        return Ok::<_, anyhow::Error>(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context(
            "Application update could not drain active jobs or terminals before the deadline",
        )??;
        Ok(drain)
    }

    pub async fn run_native_compaction(
        &self,
        store: Store,
        conversation: String,
        expected_context: String,
        scope: SessionScope,
    ) -> Result<()> {
        let boundary = self
            .admission
            .enter_admission()
            .await
            .context("The scheduler is exiting or preparing an application update")?;
        let mut workers = self.native_compactions.lock().await;
        while let Some(result) = workers.try_join_next() {
            result.context("Native compaction worker failed")?;
        }
        let runner = self.runner.clone();
        let gate = self.admission.clone();
        let (settled, result) = oneshot::channel();
        workers.spawn(async move {
            let outcome = async {
                let boundary = gate
                    .enter_admission()
                    .await
                    .context("The scheduler is exiting or preparing an application update")?;
                let plan = store
                    .reserve_native_compaction(conversation, expected_context, scope)
                    .await?;
                drop(boundary);
                let app = tokio::runtime::Handle::current();
                tokio::task::spawn_blocking(move || {
                    native_compaction::execute(&app, &store, runner.as_ref(), &gate, plan)
                })
                .await
                .context("Native compaction worker stopped unexpectedly")?
            }
            .await;
            if let Err(Err(error)) = settled.send(outcome) {
                eprintln!(
                    "Could not settle native compaction: {}",
                    diagnostics::preview(&error.to_string())
                );
            }
        });
        drop(workers);
        drop(boundary);
        result
            .await
            .context("Native compaction worker stopped unexpectedly")?
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop_admission().await;
        let mut task = self.task.lock().await;
        if let Some(worker) = task.as_mut() {
            let result = worker.await;
            task.take();
            result.context("job scheduler task failed")?;
        }
        let mut native = self.native_compactions.lock().await;
        while let Some(result) = native.join_next().await {
            result.context("Native compaction worker failed")?;
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
    let mut conversation_worker = JoinSet::new();
    let mut next_conversation_scan = tokio::time::Instant::now();
    let mut workflow_worker = JoinSet::new();
    loop {
        stopping.borrow_and_update();
        let stop = admission.is_closed();
        if stop {
            if workers.is_empty() && conversation_worker.is_empty() && workflow_worker.is_empty() {
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
            if let Some(_boundary) = admission.enter_admission().await {
                if let Err(error) = store.refresh_due_workflow_waits().await {
                    report(&error);
                }
                let conversation_progress = match store.advance_conversation_pipeline_turns().await
                {
                    Ok(progressed) => progressed,
                    Err(error) => {
                        report(&error);
                        false
                    }
                };
                if conversation_worker.is_empty()
                    && tokio::time::Instant::now() >= next_conversation_scan
                {
                    next_conversation_scan =
                        tokio::time::Instant::now() + Duration::from_millis(500);
                    let store = store.clone();
                    let identity = runner.identity().to_owned();
                    let gate = admission.clone();
                    gate.maintenance
                        .external_work
                        .fetch_add(1, Ordering::AcqRel);
                    let external = ExternalWork(gate.maintenance.clone());
                    conversation_worker.spawn(async move {
                        let _external = external;
                        if !gate.is_closed() {
                            if let Err(error) =
                                prepare_next_conversation_stage(store, identity, gate).await
                            {
                                report(&error);
                            }
                        }
                    });
                }
                if workflow_worker.is_empty() {
                    match store.next_workflow_agent().await {
                        Ok(Some(stage)) => {
                            let gate = admission.clone();
                            gate.maintenance
                                .external_work
                                .fetch_add(1, Ordering::AcqRel);
                            let external = ExternalWork(gate.maintenance.clone());
                            workflow_worker.spawn(prepare_workflow_agent(
                                store.clone(),
                                stage,
                                runner.identity().into(),
                                gate,
                                external,
                            ));
                        }
                        Ok(None) => {}
                        Err(error) => report(&error),
                    }
                }
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
                                    ActiveControl::Process(control),
                                    Worker::Process(worker),
                                )
                            }
                            ScheduledAttempt::Side(attempt) => {
                                let run_id = attempt.run.id.clone();
                                let (control, worker) = side_requests::start(
                                    store.clone(),
                                    runner.clone(),
                                    *attempt,
                                    admission.clone(),
                                );
                                (
                                    run_id,
                                    ActiveControl::Process(control),
                                    Worker::Process(worker),
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
                if conversation_progress {
                    continue;
                }
            }
        }
        tokio::select! {
            _ = stopping.changed(), if !stop => {}
            _ = signal.changed(), if !stop => {},
            _ = workers.join_next(), if !workers.is_empty() => {},
            result = conversation_worker.join_next(), if !conversation_worker.is_empty() => {
                if let Some(Err(error)) = result { report(&error.into()); }
            }
            result = workflow_worker.join_next(), if !workflow_worker.is_empty() => {
                if let Some(Err(error)) = result {
                    report(&error.into());
                }
            }
            // Recheck local executable and runner readiness after external changes.
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

async fn prepare_workflow_agent(
    store: Store,
    stage: PendingWorkflowAgent,
    runner: String,
    admission: LaunchGate,
    _external: ExternalWork,
) {
    match stage.reconcile(&store).await {
        Ok(false) => {}
        Ok(true) => return,
        Err(error) => {
            report(&error);
            return;
        }
    }
    let prepared = stage.prepare(&store, runner).await;
    let Some(_boundary) = admission.enter_admission().await else {
        return;
    };
    let result = match prepared {
        Ok(submission) => stage.enqueue(&store, submission).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result
        && let Err(error) = stage.defer(&store, &error).await
    {
        report(&error);
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
