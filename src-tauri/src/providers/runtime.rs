use std::{
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::{runtime::Handle, sync::oneshot, task::JoinHandle};

use crate::{
    conversations::{AttemptPlan, MessageRevision, MessageRole},
    diagnostics,
    dispatch::{ContextSnapshot, Delivery, EventSource, NativeHistory},
    process::TerminalOutcome,
    process_io::CapturedProcess,
    repository,
    runner::{CONTRACT_VERSION, ExecutionPlatform, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};

use super::{
    AdapterUpdate, ApprovalDecision, CliSession, Provider, ProviderEvent, ProviderEventData,
    ProviderLocation, RequestId, SessionIntent, SessionRequest, TurnInput, TurnOutcome,
    WireMessage,
    capabilities::{CapabilityScope, ReportedCapabilities},
    configuration,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Interruption {
    PromptReturned(TurnOutcome),
    ProcessStopped(TerminalOutcome),
}

/// Settle the outstanding turn after requesting an interrupt. The watchdog
/// stays independent of synchronous pipe writes so a wedged child cannot
/// prevent the supervisor from closing the job.
pub fn interrupt_to_completion(
    process: &mut CapturedProcess,
    session: &mut dyn CliSession,
    mut publish: impl FnMut(ProviderEvent, EventSource) -> Result<()>,
) -> Result<Interruption> {
    let interruption = session.interrupt()?;
    let deadline = Instant::now() + interruption.grace;
    let control = process.control.clone();
    let (settled, wait) = mpsc::channel();
    thread::scope(|scope| {
        let watchdog = scope.spawn(move || {
            if wait
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                control.cancel()?;
            }
            Ok::<_, std::io::Error>(())
        });

        let observed = (|| {
            write_frames(process, interruption.outbound)?;
            while Instant::now() < deadline {
                let frame = match process
                    .output
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(frame) => frame?,
                    Err(_) => break,
                };
                let response = session.receive(frame)?;
                if let Some(outcome) =
                    deliver_update(process, response, EventSource::Provider, &mut publish)?
                {
                    return Ok::<_, anyhow::Error>(Some(outcome));
                }
            }
            Ok(None)
        })();
        if let Ok(Some(outcome)) = observed {
            let _ = settled.send(());
            watchdog.join().expect("interruption watchdog")?;
            return Ok(Interruption::PromptReturned(outcome));
        }

        let cancellation = process.control.cancel();
        let _ = settled.send(());
        let guarded = watchdog.join().expect("interruption watchdog");
        if let Err(error) = cancellation {
            guarded.context("watchdog could not stop the CLI job")?;
            return Err(error).context("could not stop the CLI job");
        }
        let exit = process.control.wait()?;
        deliver_update(
            process,
            session.process_finished(exit),
            EventSource::Supervisor,
            &mut publish,
        )?;
        observed?;
        Ok(Interruption::ProcessStopped(exit))
    })
}

fn deliver_update(
    process: &mut CapturedProcess,
    update: AdapterUpdate,
    source: EventSource,
    publish: &mut impl FnMut(ProviderEvent, EventSource) -> Result<()>,
) -> Result<Option<TurnOutcome>> {
    write_frames(process, update.outbound)?;
    let mut terminal = None;
    for event in update.events {
        if let ProviderEventData::TurnFinished { outcome } = event.data {
            terminal = Some(outcome);
        }
        publish(event, source)?;
    }
    Ok(terminal)
}

fn write_frames(process: &mut CapturedProcess, frames: Vec<WireMessage>) -> Result<()> {
    for request in frames {
        process.input.send(request.as_bytes())?;
    }
    Ok(())
}

enum Command {
    Interrupt,
    Approval(RequestId, ApprovalDecision, oneshot::Sender<Result<()>>),
    Question(RequestId, Value, oneshot::Sender<Result<()>>),
}

pub struct ManagedTurn {
    pub run_id: String,
    commands: mpsc::SyncSender<Command>,
    worker: JoinHandle<Result<RunStatus>>,
}

impl ManagedTurn {
    pub fn interrupt(&self) -> Result<()> {
        self.commands
            .try_send(Command::Interrupt)
            .map_err(|_| anyhow::anyhow!("attempt command queue is closed or busy"))
    }

    pub async fn approve(&self, id: RequestId, decision: ApprovalDecision) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(Command::Approval(id, decision, reply))
            .map_err(|_| anyhow::anyhow!("attempt command queue is closed or busy"))?;
        result
            .await
            .context("attempt stopped before acknowledging approval")?
    }

    pub async fn answer(&self, id: RequestId, answer: Value) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(Command::Question(id, answer, reply))
            .map_err(|_| anyhow::anyhow!("attempt command queue is closed or busy"))?;
        result
            .await
            .context("attempt stopped before acknowledging answer")?
    }

    pub async fn wait(self) -> Result<RunStatus> {
        let Self {
            commands, worker, ..
        } = self;
        let result = worker.await.context("managed turn worker failed")?;
        drop(commands);
        result
    }
}

/// Launch one process for the admitted attempt. The caller retains the handle
/// to submit decisions or cancel. Dropping it requests a stop.
pub fn start_managed_turn(store: Store, runner: Arc<dyn Runner>, plan: AttemptPlan) -> ManagedTurn {
    let id = plan.run.id.clone();
    let handle = Handle::current();
    let (commands, receiver) = mpsc::sync_channel(16);
    let worker = tokio::task::spawn_blocking(move || {
        let mut execution = TurnExecution {
            app: handle,
            store,
            plan,
            final_answer: String::new(),
            streamed_answer: String::new(),
            reported: ReportedCapabilities::default(),
            capability_scope: None,
            executable_revision: None,
            platform: None,
        };
        let outcome = execution.run(runner.as_ref(), &receiver);
        let status = match outcome {
            Ok(status) => status,
            Err(error) => {
                let message = diagnostics::preview(&error.to_string());
                let _ = execution.record(
                    ProviderEvent {
                        location: ProviderLocation::default(),
                        data: ProviderEventData::Error { message },
                    },
                    EventSource::Supervisor,
                );
                RunStatus::Failed
            }
        };
        let answer = if execution.final_answer.is_empty() {
            execution.streamed_answer
        } else {
            execution.final_answer
        };
        execution
            .app
            .block_on(execution.store.finish_agent_attempt(
                execution.plan.run.id,
                status,
                Some(answer),
            ))?;
        Ok(status)
    });
    ManagedTurn {
        run_id: id,
        commands,
        worker,
    }
}

struct TurnExecution {
    app: Handle,
    store: Store,
    plan: AttemptPlan,
    final_answer: String,
    streamed_answer: String,
    reported: ReportedCapabilities,
    capability_scope: Option<CapabilityScope>,
    executable_revision: Option<String>,
    platform: Option<ExecutionPlatform>,
}

impl TurnExecution {
    fn record(&mut self, event: ProviderEvent, source: EventSource) -> Result<()> {
        let mut scope_changed = false;
        // Child traffic belongs to the run timeline, not the parent transcript.
        if event.location.parent_item_id.is_none() {
            if source == EventSource::Provider {
                scope_changed = self.reported.observe(&event.data);
            }
            match &event.data {
                ProviderEventData::AssistantDelta { text } => self.streamed_answer.push_str(text),
                ProviderEventData::AssistantMessage { text } => self.final_answer.push_str(text),
                _ => {}
            }
        }
        self.app.block_on(self.store.record_agent_observation(
            self.plan.run.id.clone(),
            source,
            event,
        ))?;
        if scope_changed && let Some(scope) = &mut self.capability_scope {
            scope.reported_model = self.reported.model.clone();
            scope.reported_effort = self.reported.effort.clone();
            let descriptors = self.reported.descriptors(scope);
            let assessment = self.app.block_on(
                self.store
                    .resolve_capability_assessment(scope.clone(), descriptors),
            )?;
            self.app.block_on(self.store.record_agent_observation(
                self.plan.run.id.clone(),
                EventSource::Application,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::CapabilityAssessmentUpdated {
                        assessment: Box::new(assessment),
                    },
                },
            ))?;
        }
        Ok(())
    }

    fn run(
        &mut self,
        runner: &dyn Runner,
        commands: &mpsc::Receiver<Command>,
    ) -> Result<RunStatus> {
        let scope = &self.plan.scope;
        ensure!(
            scope.runner == runner.identity(),
            "selected runner differs from admission"
        );
        let checkout = self.app.block_on(repository::verify_checkout(
            &self.store,
            self.plan.run.checkout_id.clone(),
        ))?;
        ensure!(
            checkout == scope.checkout,
            "checkout differs from admission"
        );
        let adapter = scope.provider.adapter();
        let mut session = adapter.create_session(SessionRequest {
            checkout: scope.checkout.clone(),
            intent: self.plan.intent.clone(),
            mode: scope.mode,
            model: scope.model.clone(),
            effort: scope.effort.clone(),
        })?;
        let args = session.arguments();
        let args = args
            .iter()
            .map(|argument| argument.as_os_str())
            .collect::<Vec<_>>();
        self.executable_revision = runner.executable_revision(&scope.executable);
        self.platform = runner.execution_platform();
        let mut process = runner.launch(LaunchRequest {
            version: CONTRACT_VERSION,
            executable: &scope.executable,
            checkout: &scope.checkout,
            arguments: &args,
        })?;
        self.app
            .block_on(self.store.mark_agent_running(self.plan.run.id.clone()))?;
        let control = process.control.clone();
        let run = self.drive(&mut process, session.as_mut(), commands);
        let stopped = control.cancel();
        let ended = control.wait();
        stopped?;
        ended?;
        run
    }

    fn drive(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
        commands: &mpsc::Receiver<Command>,
    ) -> Result<RunStatus> {
        bounded_write(process, session.start()?.outbound)?;
        let startup_deadline = Instant::now() + Duration::from_secs(20);
        let mut sent = false;
        let mut ready = false;
        let mut configuration_deadline = None;
        loop {
            let command = commands.try_recv();
            match command {
                Ok(Command::Interrupt) | Err(mpsc::TryRecvError::Disconnected) => {
                    if !sent {
                        return Ok(RunStatus::Cancelled);
                    }
                    let result = interrupt_to_completion(process, session, |event, source| {
                        self.record(event, source)
                    })?;
                    return Ok(match result {
                        Interruption::PromptReturned(outcome) => status(outcome),
                        Interruption::ProcessStopped(_) => RunStatus::Cancelled,
                    });
                }
                Ok(Command::Approval(id, answer, reply)) => {
                    self.deliver_decision(process, session.respond_approval(&id, answer), reply)?;
                }
                Ok(Command::Question(id, answer, reply)) => {
                    self.deliver_decision(process, session.respond_question(&id, answer), reply)?;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if ready
                && !sent
                && (self.reported.configuration_reported
                    || configuration_deadline.is_none_or(|deadline| Instant::now() >= deadline))
            {
                self.dispatch(process, session)?;
                sent = true;
            }
            if !ready {
                ensure!(
                    Instant::now() < startup_deadline,
                    "CLI session startup timed out"
                );
            }
            let frame = match process.output.recv_timeout(Duration::from_millis(50)) {
                Ok(frame) => frame?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    process.control.cancel()?;
                    let terminal = process.control.wait()?;
                    let terminal = self.record_update(
                        process,
                        session.process_finished(terminal),
                        EventSource::Supervisor,
                    )?;
                    return Ok(terminal.unwrap_or(RunStatus::Failed));
                }
            };
            let update = session.receive(frame)?;
            let session_ready = !ready
                && update.events.iter().any(|event| {
                    matches!(event.data, ProviderEventData::SessionReady { .. })
                        || (self.plan.scope.provider == Provider::Claude
                            && matches!(event.data, ProviderEventData::Handshake { .. }))
                });
            let failed_start = !sent
                && update
                    .events
                    .iter()
                    .any(|event| matches!(event.data, ProviderEventData::Error { .. }));
            if let Some(status) = self.record_update(process, update, EventSource::Provider)? {
                ensure!(sent, "CLI finished before prompt dispatch");
                return Ok(status);
            }
            if failed_start {
                return Ok(RunStatus::Failed);
            }
            if session_ready {
                ready = true;
                if let Some(probe) = session.query_effective_configuration()? {
                    bounded_write(process, vec![probe])?;
                    configuration_deadline = Some(Instant::now() + Duration::from_secs(2));
                }
            }
        }
    }

    fn dispatch(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
    ) -> Result<()> {
        let update = session.send(format_input(&self.plan.input)?)?;
        let native_history = match (&self.plan.intent, &self.plan.resume_cursor_id) {
            (SessionIntent::Start, None) => NativeHistory::Fresh,
            (
                SessionIntent::Resume {
                    provider_session_id,
                },
                Some(context_id),
            ) => NativeHistory::ProviderCursor {
                session_id: provider_session_id.clone(),
                context_id: context_id.clone(),
            },
            _ => anyhow::bail!("inconsistent resume cursor"),
        };
        let configuration = configuration::inspect(
            self.plan.scope.provider,
            &self.plan.scope.checkout,
            self.reported.configuration_revision.as_deref(),
        )?;
        let scope = CapabilityScope::from_dispatch(
            &self.plan.scope,
            self.plan.run.checkout_id.clone(),
            self.executable_revision.clone(),
            self.platform.clone(),
            configuration,
            self.reported.model.clone(),
            self.reported.effort.clone(),
        );
        let descriptors = self.reported.descriptors(&scope);
        let capabilities = self.app.block_on(
            self.store
                .resolve_capability_assessment(scope.clone(), descriptors),
        )?;
        self.capability_scope = Some(scope);
        let snapshot = ContextSnapshot {
            version: 2,
            context_id: self.plan.context_id.clone(),
            scope: self.plan.scope.clone(),
            base_commit: repository::base_commit(&self.plan.scope.checkout)?,
            submitted_messages: self.plan.input.clone(),
            native_history,
            capabilities: Some(capabilities),
        };
        let frames = update
            .outbound
            .iter()
            .map(|frame| frame.as_bytes().to_vec())
            .collect();
        let dispatch_id = self.app.block_on(self.store.prepare_dispatch(
            self.plan.run.id.clone(),
            snapshot,
            frames,
        ))?;
        let result = bounded_write(process, update.outbound);
        let delivery = if result.is_ok() {
            Delivery::Submitted
        } else {
            Delivery::Unconfirmed
        };
        self.app
            .block_on(self.store.settle_dispatch(dispatch_id.clone(), delivery))?;
        result?;
        self.record(
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::DispatchSubmitted { dispatch_id },
            },
            EventSource::Application,
        )?;
        for event in update.events {
            self.record(event, EventSource::Application)?;
        }
        Ok(())
    }

    fn record_update(
        &mut self,
        process: &mut CapturedProcess,
        update: AdapterUpdate,
        source: EventSource,
    ) -> Result<Option<RunStatus>> {
        let mut finished = None;
        for event in update.events {
            if let ProviderEventData::TurnFinished { outcome } = event.data {
                finished = Some(status(outcome));
            }
            self.record(event, source)?;
        }
        bounded_write(process, update.outbound)?;
        Ok(finished)
    }

    fn deliver_decision(
        &mut self,
        process: &mut CapturedProcess,
        update: Result<AdapterUpdate>,
        reply: oneshot::Sender<Result<()>>,
    ) -> Result<()> {
        match update {
            Ok(update) => {
                let applied = self.record_update(process, update, EventSource::Application);
                let _ = reply.send(
                    applied
                        .as_ref()
                        .map(|_| ())
                        .map_err(|error| anyhow::anyhow!("{error}")),
                );
                applied?;
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
        Ok(())
    }
}

fn status(outcome: TurnOutcome) -> RunStatus {
    match outcome {
        TurnOutcome::Completed => RunStatus::Succeeded,
        TurnOutcome::Failed => RunStatus::Failed,
        TurnOutcome::Interrupted => RunStatus::Cancelled,
    }
}

fn format_input(input: &[MessageRevision]) -> Result<TurnInput> {
    let (last, history) = input.split_last().context("attempt has no input")?;
    ensure!(
        last.role == MessageRole::User,
        "last input must be a user message"
    );
    let text = if history.is_empty() {
        last.text.clone()
    } else {
        format!(
            "Conversation context supplied by OmniAgent (JSON message history):\n{}\n\nCurrent user message:\n{}",
            serde_json::to_string(history)?,
            last.text,
        )
    };
    Ok(TurnInput {
        message_id: last.message_id.clone(),
        text,
    })
}

fn bounded_write(process: &mut CapturedProcess, frames: Vec<WireMessage>) -> Result<()> {
    if frames.is_empty() {
        return Ok(());
    }
    let control = process.control.clone();
    let (finished, wait) = mpsc::channel();
    thread::scope(|scope| {
        let watchdog = scope.spawn(move || {
            if wait.recv_timeout(Duration::from_secs(10)).is_err() {
                control.cancel()?;
            }
            Ok::<_, std::io::Error>(())
        });
        let result = write_frames(process, frames);
        let _ = finished.send(());
        watchdog.join().expect("CLI write watchdog")?;
        result
    })
}
