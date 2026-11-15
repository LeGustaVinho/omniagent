use std::{
    io,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::{runtime::Handle, sync::oneshot, task::JoinHandle};

use crate::{
    attachments::{
        AttachmentAssessment, AttachmentBudget, AttachmentDecision, AttachmentDelivery,
        AttachmentDispatch, AttachmentFormat, AttachmentInput, AttachmentSource,
        CapturedAttachment, CapturedPayload, StagedAttachments,
    },
    conversations::{AttemptPlan, MessageRevision, MessageRole, PermissionRoute},
    diagnostics,
    dispatch::{ContextSnapshot, Delivery, EventSource, NativeHistory},
    jobs::{DispatchRejection, FailureCause, FailureObservation, FailurePhase, LaunchGate},
    mcp_bridge::{PERMISSION_PREFIX, Registration, RunBridge},
    process::TerminalOutcome,
    process_io::CapturedProcess,
    repository::{self, CheckoutLayout},
    runner::{CONTRACT_VERSION, ExecutionPlatform, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};

use super::{
    AdapterUpdate, ApprovalDecision, CliSession, Provider, ProviderEvent, ProviderEventData,
    ProviderLocation, RequestId, SessionIntent, SessionMode, SessionRequest, TurnInput,
    TurnOutcome, WireMessage,
    capabilities::{
        CapabilityAssessment, CapabilityScope, CapabilitySubject, ReportedCapabilities,
        Verification,
    },
    configuration,
    launch_scope::{ScopedLaunch, ScopedLaunchEvidence},
    planning::{PlanSource, STRUCTURED_PLANNING_PROMPT, StructuredPlan},
    plugins::{PluginInventory, probe_run_plugins},
    skills::{SkillCatalog, explicit_skill_request},
    steering::{SteerOutcome, SteerTarget},
    suggestions::SuggestionOutcome,
};

mod requirements;

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
    YieldToDownstream,
    Approval(RequestId, ApprovalDecision, oneshot::Sender<Result<()>>),
    Question(RequestId, Value, oneshot::Sender<Result<()>>),
    Steer(SteerTarget, TurnInput, oneshot::Sender<Result<()>>),
}

enum Decision {
    Permission(RequestId, ApprovalDecision),
    Question(RequestId),
}

pub struct ManagedTurn {
    pub run_id: String,
    control: ManagedTurnControl,
    worker: JoinHandle<Result<RunStatus>>,
}

#[derive(Clone)]
pub struct ManagedTurnControl {
    commands: mpsc::SyncSender<Command>,
}

impl std::ops::Deref for ManagedTurn {
    type Target = ManagedTurnControl;

    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

impl ManagedTurnControl {
    pub fn interrupt(&self) -> Result<()> {
        self.commands
            .try_send(Command::Interrupt)
            .map_err(|_| anyhow::anyhow!("attempt command queue is closed or busy"))
    }

    pub(crate) fn yield_to_downstream(&self) -> Result<()> {
        self.commands
            .try_send(Command::YieldToDownstream)
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

    /// Submission records an attempt or a local rejection. Provider delivery settles in run events.
    pub async fn steer(&self, target: SteerTarget, input: TurnInput) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(Command::Steer(target, input, reply))
            .map_err(|_| anyhow::anyhow!("attempt command queue is closed or busy"))?;
        result
            .await
            .context("attempt stopped before recording steering")?
    }
}

impl ManagedTurn {
    pub fn control(&self) -> ManagedTurnControl {
        self.control.clone()
    }

    pub async fn wait(self) -> Result<RunStatus> {
        let Self {
            control, worker, ..
        } = self;
        let result = worker.await.context("managed turn worker failed")?;
        drop(control);
        result
    }
}

/// Launch one process for the admitted attempt. The caller retains the handle
/// to submit decisions or cancel. Dropping it requests a stop.
pub fn start_managed_turn(
    store: Store,
    runner: Arc<dyn Runner>,
    plan: AttemptPlan,
    bridge_executable: Option<PathBuf>,
) -> ManagedTurn {
    start_turn(store, runner, plan, bridge_executable, false, None)
}

pub(crate) fn start_scheduled_turn(
    store: Store,
    runner: Arc<dyn Runner>,
    plan: AttemptPlan,
    bridge_executable: Option<PathBuf>,
    admission: LaunchGate,
) -> ManagedTurn {
    start_turn(
        store,
        runner,
        plan,
        bridge_executable,
        false,
        Some(admission),
    )
}

/// Start a supervised turn whose selected profile permits native prompt suggestions.
pub fn start_managed_turn_with_native_suggestions(
    store: Store,
    runner: Arc<dyn Runner>,
    plan: AttemptPlan,
    bridge_executable: Option<PathBuf>,
) -> ManagedTurn {
    start_turn(store, runner, plan, bridge_executable, true, None)
}

fn start_turn(
    store: Store,
    runner: Arc<dyn Runner>,
    plan: AttemptPlan,
    bridge_executable: Option<PathBuf>,
    native_suggestions: bool,
    admission: Option<LaunchGate>,
) -> ManagedTurn {
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
            proposed_plan: None,
            last_parent_message: None,
            reported: ReportedCapabilities::default(),
            reported_skills: None,
            plugin_inventory: None,
            scoped_launch: None,
            capability_scope: None,
            executable_revision: None,
            platform: None,
            staged_attachments: StagedAttachments::default(),
            native_suggestions,
            admission,
            failure: FailureObservation {
                phase: FailurePhase::Preparation,
                ..Default::default()
            },
            quiescence_confirmed: false,
            downstream_requested: false,
        };
        let outcome = execution
            .run(runner.as_ref(), &receiver, bridge_executable)
            .and_then(|status| {
                if status == RunStatus::Succeeded
                    && !execution.plan.question_route.is_native()
                    && !execution.downstream_requested
                {
                    execution.failure.phase = FailurePhase::ResultValidation;
                    execution.complete_structured_planning()?;
                }
                Ok(status)
            });
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
                if execution.exiting() {
                    RunStatus::Interrupted
                } else {
                    RunStatus::Failed
                }
            }
        };
        if status == RunStatus::Interrupted
            && execution.exiting()
            && execution.failure.cause.is_none()
        {
            execution.failure.cause = Some(FailureCause::Shutdown);
        }
        let answer = execution.proposed_plan.unwrap_or_else(|| {
            if execution.final_answer.is_empty() {
                execution.streamed_answer
            } else {
                execution.final_answer
            }
        });
        execution
            .app
            .block_on(execution.store.finish_observed_agent_attempt(
                execution.plan.run.id,
                status,
                Some(answer),
                Some(execution.failure),
                execution.quiescence_confirmed,
            ))?;
        Ok(status)
    });
    ManagedTurn {
        run_id: id,
        control: ManagedTurnControl { commands },
        worker,
    }
}

struct TurnExecution {
    failure: FailureObservation,
    quiescence_confirmed: bool,
    downstream_requested: bool,
    admission: Option<LaunchGate>,
    app: Handle,
    store: Store,
    plan: AttemptPlan,
    final_answer: String,
    streamed_answer: String,
    proposed_plan: Option<String>,
    last_parent_message: Option<String>,
    reported: ReportedCapabilities,
    reported_skills: Option<SkillCatalog>,
    plugin_inventory: Option<PluginInventory>,
    scoped_launch: Option<ScopedLaunchEvidence>,
    capability_scope: Option<(CapabilityScope, CheckoutLayout)>,
    executable_revision: Option<String>,
    platform: Option<ExecutionPlatform>,
    staged_attachments: StagedAttachments,
    native_suggestions: bool,
}

impl TurnExecution {
    fn record(&mut self, event: ProviderEvent, source: EventSource) -> Result<()> {
        let mut scope_changed = event.location.parent_item_id.is_some()
            && source == EventSource::Provider
            && matches!(event.data, ProviderEventData::ChildActivity { .. })
            && self.reported.observe(&event.data);
        // Child traffic belongs to the run timeline, not the parent transcript.
        if event.location.parent_item_id.is_none() {
            if source == EventSource::Provider
                || matches!(
                    event.data,
                    ProviderEventData::BridgeRegistration { .. }
                        | ProviderEventData::QuestionReplySettled { .. }
                )
            {
                scope_changed = self.reported.observe(&event.data);
            }
            if source == EventSource::Provider
                && let ProviderEventData::SkillsReported { catalog } = &event.data
            {
                scope_changed |= self.reported_skills.as_ref() != Some(catalog);
                self.reported_skills = Some(catalog.clone());
            }
            match &event.data {
                ProviderEventData::AssistantDelta { text } => self.streamed_answer.push_str(text),
                ProviderEventData::AssistantMessage { text } => {
                    self.final_answer.push_str(text);
                    if !self.plan.question_route.is_native() {
                        self.last_parent_message = Some(text.clone());
                    }
                }
                ProviderEventData::PlanProposed { text, source }
                    if *source != PlanSource::StructuredTurn =>
                {
                    self.proposed_plan = Some(text.clone())
                }
                _ => {}
            }
        }
        self.app.block_on(self.store.record_agent_observation(
            self.plan.run.id.clone(),
            source,
            event,
        ))?;
        if scope_changed && let Some((scope, checkout)) = &mut self.capability_scope {
            scope.reported_model = self.reported.model.clone();
            scope.reported_effort = self.reported.effort.clone();
            scope.configuration.provider_revision = self.reported.configuration_revision.clone();
            configuration::resolve_effective_configuration(
                &mut scope.configuration,
                checkout,
                self.reported.configuration_qualification.as_ref(),
                self.scoped_launch
                    .as_ref()
                    .context("missing scoped launch evidence")?,
                self.plugin_inventory.as_ref(),
                self.reported_skills.as_ref(),
            )?;
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

    fn record_bridge_registration(&mut self, registration: Registration) -> Result<()> {
        if self.reported.bridge_registration.as_ref() != Some(&registration) {
            self.record(
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::BridgeRegistration { registration },
                },
                EventSource::Application,
            )?;
        }
        Ok(())
    }

    fn run(
        &mut self,
        runner: &dyn Runner,
        commands: &mpsc::Receiver<Command>,
        bridge_executable: Option<PathBuf>,
    ) -> Result<RunStatus> {
        if self.exiting() {
            return Ok(RunStatus::Interrupted);
        }
        self.app.block_on(
            self.store
                .job_dispatch_requirements(self.plan.run.id.clone()),
        )?;
        let scope = self.plan.scope.clone();
        if let Err(error) = self.verify_effective_profile(runner) {
            if self.exiting() {
                return Ok(RunStatus::Interrupted);
            }
            self.reject_dispatch(DispatchRejection::ProfileUnavailable)?;
            return Err(error);
        }
        let checkout = match self.app.block_on(repository::verify_checkout(
            &self.store,
            self.plan.run.checkout_id.clone(),
            scope.checkout.clone(),
        )) {
            Ok(checkout) => checkout,
            Err(error) => {
                self.reject_dispatch(DispatchRejection::WorktreeUnavailable)?;
                self.failure.cause = Some(FailureCause::WorktreeUnavailable);
                return Err(error);
            }
        };
        let adapter = scope.provider.adapter();
        let mut session = adapter.create_session(SessionRequest {
            checkout: scope.checkout.clone(),
            intent: self.plan.intent.clone(),
            mode: scope.mode,
            model: scope.model.clone(),
            effort: scope.effort.clone(),
        })?;
        if self.native_suggestions && self.plan.question_route.is_native() {
            session.enable_native_suggestions();
        }
        session.set_checkout_root(&checkout.root);
        session.set_question_route(self.plan.question_route);
        let review = self.app.block_on(self.store.resolve_launch_review(
            self.plan.run.checkout_id.clone(),
            &checkout,
            scope.provider,
        ))?;
        let bridge = bridge_executable
            .map(|executable| -> Result<_> {
                let bridge = self.app.block_on(RunBridge::start_for_run(
                    &self.store,
                    self.plan.run.id.clone(),
                    scope.provider == Provider::Claude && scope.mode == SessionMode::ApprovedWrite,
                ))?;
                let registration = bridge.launch(&executable)?;
                Ok((bridge, registration))
            })
            .transpose();
        let bridge = match bridge {
            Ok(Some((bridge, registration))) => {
                self.record_bridge_registration(bridge.registration())?;
                Some((bridge, registration))
            }
            Ok(None) => {
                self.record_bridge_registration(Registration::Unavailable {
                    reason: "The selected runner has no MCP helper".into(),
                })?;
                None
            }
            Err(error) => {
                self.record_bridge_registration(Registration::Unavailable {
                    reason: diagnostics::preview(&error.to_string()),
                })?;
                None
            }
        };
        let registration = bridge.as_ref().map(|(_, registration)| registration);
        ensure!(
            registration.is_some() || !self.plan.requires_bridge,
            "This job requires the OmniAgent MCP bridge"
        );
        if let Some(registration) = registration {
            session.configure_mcp_bridge(registration.clone());
        }
        let mut launch = ScopedLaunch::prepare(
            scope.provider,
            &scope.cli_version,
            &checkout,
            &review,
            session.arguments(),
            registration,
        )?;
        if let Some(recovery) = &self.plan.native_job_recovery {
            ensure!(
                launch.evidence.approved == recovery.approved_customizations,
                "Executable customizations changed since the provider checkpoint"
            );
        }
        launch.identify_mcp_executables(runner, &review, registration)?;
        if scope.provider == Provider::Claude
            && self.plan.attachments.iter().any(|(_, attachment)| {
                matches!(
                    attachment.metadata.source,
                    AttachmentSource::SelectedFile { .. }
                ) && matches!(attachment.payload, CapturedPayload::Reference { .. })
            })
            && let Ok(directory) = self
                .staged_attachments
                .configure_claude_read_directory(runner)
        {
            launch
                .arguments
                .splice(0..0, ["--add-dir".into(), directory.into_os_string()]);
        }
        if let Some(instructions) = launch.protocol_instructions.take() {
            session.set_workspace_instructions(instructions)?;
        }
        self.record(
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::ScopedLaunchPrepared {
                    evidence: Box::new(launch.evidence.clone()),
                },
            },
            EventSource::Application,
        )?;
        self.scoped_launch = Some(launch.evidence.clone());
        let args = launch
            .arguments
            .iter()
            .map(|argument| argument.as_os_str())
            .collect::<Vec<_>>();
        self.executable_revision = runner.executable_revision(&scope.executable);
        if let Some(recovery) = &self.plan.native_job_recovery {
            ensure!(
                self.executable_revision.as_ref() == Some(&recovery.executable_revision),
                "The CLI executable changed since the provider checkpoint"
            );
        }
        self.platform = runner.execution_platform();
        let admission = self.admission.clone();
        let plugins = probe_run_plugins(&scope, |request| {
            let _boundary = match &admission {
                Some(gate) => Some(self.app.block_on(gate.enter()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Interrupted, "the scheduler is exiting")
                })?),
                None => None,
            };
            runner.launch(request)
        });
        if self.exiting() {
            return Ok(RunStatus::Interrupted);
        }
        self.plugin_inventory = Some(
            self.app
                .block_on(self.store.save_plugin_inventory(plugins))?,
        );
        self.failure.phase = FailurePhase::EvidenceCapture;
        let managed_writer = self
            .app
            .block_on(repository::capture_worktree_start(
                &self.store,
                self.plan.run.id.clone(),
            ))
            .inspect_err(|_| self.failure.cause = Some(FailureCause::EvidenceUnavailable))?;
        self.failure.phase = FailurePhase::Launch;
        let mut process = {
            let _boundary = match &admission {
                Some(gate) => match self.app.block_on(gate.enter()) {
                    Some(guard) => Some(guard),
                    None => return Ok(RunStatus::Interrupted),
                },
                None => None,
            };
            if !self.app.block_on(
                self.store
                    .record_agent_launch_intent(self.plan.run.id.clone()),
            )? {
                return Ok(RunStatus::Cancelled);
            }
            runner
                .launch(LaunchRequest {
                    version: CONTRACT_VERSION,
                    executable: &scope.executable,
                    checkout: &scope.checkout,
                    arguments: &args,
                })
                .inspect_err(|_| self.failure.cause = Some(FailureCause::LaunchRejected))?
        };
        let control = process.control.clone();
        let endpoint = bridge.as_ref().map(|(endpoint, _)| endpoint);
        self.failure.phase = FailurePhase::Startup;
        let run = self
            .app
            .block_on(self.store.mark_agent_running(self.plan.run.id.clone()))
            .and_then(|()| {
                self.drive(
                    &mut process,
                    session.as_mut(),
                    commands,
                    &checkout,
                    endpoint,
                    runner,
                )
            });
        let prior_failure = self.failure;
        self.failure.phase = FailurePhase::ProcessStop;
        self.failure.cause = None;
        let stopped = control.cancel();
        let ended = control.wait();
        if let Ok(exit) = ended.as_ref() {
            self.failure.process = Some((*exit).into());
        }
        let registration = endpoint.map(RunBridge::registration);
        drop(bridge);
        self.downstream_requested = self
            .app
            .block_on(self.store.has_yielding_wait(self.plan.run.id.clone()))?;
        if managed_writer || self.exiting() || self.downstream_requested {
            control
                .confirm_quiescence()
                .inspect_err(|_| self.failure.cause = Some(FailureCause::StopUnconfirmed))?;
            self.quiescence_confirmed = true;
        }
        if managed_writer {
            self.failure.phase = FailurePhase::EvidenceCapture;
            self.failure.cause = Some(FailureCause::EvidenceUnavailable);
            self.app.block_on(repository::save_worktree_candidate(
                &self.store,
                self.plan.run.id.clone(),
            ))?;
        }
        self.failure.phase = FailurePhase::ProcessStop;
        self.failure.cause = None;
        stopped?;
        ended?;
        self.failure = FailureObservation {
            process: self.failure.process,
            ..prior_failure
        };
        if let Some(registration) = registration {
            let registration = match registration {
                Registration::Configured
                    if self.plan.requires_bridge || matches!(run, Ok(RunStatus::Succeeded)) =>
                {
                    Registration::Unavailable {
                        reason: "The CLI did not list the scoped tools during this attempt".into(),
                    }
                }
                registration => registration,
            };
            self.record_bridge_registration(registration)?;
        }
        run
    }

    fn exiting(&self) -> bool {
        self.admission.as_ref().is_some_and(LaunchGate::is_closed)
    }

    fn drive(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
        commands: &mpsc::Receiver<Command>,
        checkout: &CheckoutLayout,
        bridge: Option<&RunBridge>,
        runner: &dyn Runner,
    ) -> Result<RunStatus> {
        if self.exiting() {
            return Ok(RunStatus::Interrupted);
        }
        bounded_write(process, session.start()?.outbound)?;
        let startup_deadline = Instant::now() + Duration::from_secs(20);
        let mut sent = false;
        let mut ready = false;
        let mut inventory_deadline = None;
        let mut wait_for_configuration = false;
        let mut wait_for_skills = false;
        loop {
            let registration = bridge.map(RunBridge::registration);
            if let Some(registration) = &registration {
                self.record_bridge_registration(registration.clone())?;
            }
            let exiting = self.exiting();
            let command = if exiting {
                Ok(Command::Interrupt)
            } else {
                commands.try_recv()
            };
            match command {
                Ok(Command::Interrupt | Command::YieldToDownstream)
                | Err(mpsc::TryRecvError::Disconnected) => {
                    let yielding = matches!(command, Ok(Command::YieldToDownstream));
                    let outcome = if sent {
                        match interrupt_to_completion(process, session, |event, source| {
                            self.record(event, source)
                        })? {
                            Interruption::PromptReturned(outcome) => status(outcome),
                            Interruption::ProcessStopped(_) => RunStatus::Cancelled,
                        }
                    } else {
                        RunStatus::Cancelled
                    };
                    return Ok(
                        if (exiting || yielding) && outcome == RunStatus::Cancelled {
                            RunStatus::Interrupted
                        } else {
                            outcome
                        },
                    );
                }
                Ok(Command::Approval(id, answer, reply)) => {
                    if matches!(&id, RequestId::Text(id) if id.starts_with(PERMISSION_PREFIX)) {
                        let result = self.app.block_on(self.store.prepare_permission_decision(
                            self.plan.run.id.clone(),
                            self.plan.run.turn_id.clone(),
                            id,
                            answer,
                            PermissionRoute::ClaudePrompt,
                        ));
                        let _ = reply.send(result.map(|_| ()));
                        continue;
                    }
                    self.deliver_decision(
                        process,
                        session.respond_approval(&id, answer),
                        Decision::Permission(id, answer),
                        reply,
                    )?;
                }
                Ok(Command::Question(id, answer, reply)) => {
                    self.deliver_decision(
                        process,
                        session.respond_question(&id, answer),
                        Decision::Question(id),
                        reply,
                    )?;
                }
                Ok(Command::Steer(target, input, reply)) => {
                    let update = session.steer(target, input);
                    if let Ok(update) = update {
                        let applied = self.write_steering(process, update);
                        let _ = reply.send(
                            applied
                                .as_ref()
                                .map(|_| ())
                                .map_err(|error| anyhow::anyhow!("{error}")),
                        );
                        applied?;
                    } else {
                        let _ = reply.send(update.map(|_| ()));
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let inventory_complete = (!wait_for_configuration
                || self.reported.configuration_reported)
                && (!wait_for_skills || self.reported_skills.is_some());
            let bridge_ready = !self.plan.requires_bridge
                || matches!(
                    registration,
                    Some(Registration::ToolsListed | Registration::ScopeVerified)
                );
            if !sent
                && !bridge_ready
                && (Instant::now() >= startup_deadline
                    || matches!(registration, Some(Registration::Unavailable { .. })))
            {
                self.record_bridge_registration(Registration::Unavailable {
                    reason: "The CLI did not list required OmniAgent MCP tools before dispatch"
                        .into(),
                })?;
                anyhow::bail!("The CLI did not list required OmniAgent MCP tools before dispatch");
            }
            if ready
                && !sent
                && bridge_ready
                && (inventory_complete
                    || inventory_deadline.is_some_and(|deadline| Instant::now() >= deadline))
            {
                self.failure.phase = FailurePhase::Dispatch;
                if !self.dispatch(process, session, checkout, runner)? {
                    return Ok(RunStatus::Interrupted);
                }
                sent = true;
                self.failure.phase = FailurePhase::Execution;
            }
            if !ready && Instant::now() >= startup_deadline {
                self.failure.cause = Some(FailureCause::StartupTimeout);
                anyhow::bail!("CLI session startup timed out");
            }
            let frame = match process.output.recv_timeout(Duration::from_millis(50)) {
                Ok(frame) => frame?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    process.control.cancel()?;
                    let terminal = process.control.wait()?;
                    self.failure.process = Some(terminal.into());
                    let terminal = self.record_update(
                        process,
                        session.process_finished(terminal),
                        EventSource::Supervisor,
                    )?;
                    let status = terminal.unwrap_or(RunStatus::Failed);
                    if status == RunStatus::Failed {
                        self.failure.cause = Some(match self.failure.process {
                            Some(exit) if !exit.cancelled && exit.code != 0 => {
                                FailureCause::ProcessExit
                            }
                            _ => FailureCause::TransportClosed,
                        });
                    }
                    return Ok(status);
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
            let provider_error = update.events.iter().any(|event| {
                event.location.parent_item_id.is_none()
                    && matches!(event.data, ProviderEventData::Error { .. })
            });
            if let Some(status) = self.record_update(process, update, EventSource::Provider)? {
                if status == RunStatus::Failed && provider_error {
                    self.failure.cause = Some(FailureCause::ProviderError);
                }
                ensure!(sent, "CLI finished before prompt dispatch");
                if status == RunStatus::Succeeded && self.native_suggestions {
                    self.collect_suggestion(process, session, commands)?;
                }
                return Ok(status);
            }
            if failed_start {
                self.failure.cause = provider_error.then_some(FailureCause::ProviderError);
                return Ok(RunStatus::Failed);
            }
            if session_ready {
                ready = true;
                let configuration = session.query_effective_configuration()?;
                let skills = session.query_skill_inventory()?;
                wait_for_configuration = configuration.is_some();
                wait_for_skills = skills.is_some();
                bounded_write(process, configuration.into_iter().chain(skills).collect())?;
                inventory_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }
    }

    fn dispatch(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
        checkout: &CheckoutLayout,
        runner: &dyn Runner,
    ) -> Result<bool> {
        let native_history = if let Some(recovery) = &self.plan.native_job_recovery {
            NativeHistory::ProviderRecovery {
                session_id: recovery.provider_session_id.clone(),
                source_run_id: recovery.source_run_id.clone(),
                source_turn_id: recovery.provider_turn_id.clone(),
                context_id: recovery.context_id.clone(),
            }
        } else {
            match (&self.plan.intent, &self.plan.resume_cursor_id) {
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
            }
        };
        let (mut configuration, filesystem_skills, instructions) =
            configuration::inspect(&self.plan.scope, checkout, &self.reported)?;
        configuration::resolve_effective_configuration(
            &mut configuration,
            checkout,
            self.reported.configuration_qualification.as_ref(),
            self.scoped_launch
                .as_ref()
                .context("missing scoped launch evidence")?,
            self.plugin_inventory.as_ref(),
            self.reported_skills.as_ref(),
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
        self.reported.native_attachments = AttachmentFormat::ALL
            .into_iter()
            .filter_map(|format| {
                session
                    .attachment_encoding(format)
                    .map(|encoding| (format, encoding))
            })
            .collect();
        let descriptors = self.reported.descriptors(&scope);
        let capabilities = self.app.block_on(
            self.store
                .resolve_capability_assessment(scope.clone(), descriptors),
        )?;
        self.check_dispatch_requirements(&capabilities, &instructions, checkout)?;
        let mut input = format_input(&self.plan.input)?;
        if !self.plan.question_route.is_native() {
            input.text.push_str("\n\n");
            input.text.push_str(STRUCTURED_PLANNING_PROMPT);
        }
        let (mut assessment, attachments) =
            self.prepare_attachments(runner, session, checkout, &capabilities, &native_history)?;
        if let Some(blocked) = assessment.take_if(|report| !report.blockers.is_empty()) {
            return self.record_attachment_assessment(blocked).map(|()| true);
        }
        input.attachments = attachments
            .iter()
            .map(|attachment| attachment.input.clone())
            .collect();
        if attachments.iter().any(|attachment| {
            attachment.message_revision_id
                != self
                    .plan
                    .input
                    .last()
                    .expect("formatted user message")
                    .revision_id
        }) {
            let sources: Vec<_> = attachments
                .iter()
                .map(|item| (&item.message_revision_id, item.metadata.source.file_name()))
                .collect();
            input
                .text
                .push_str("\n\nAttachment order (message revision and file name):\n");
            input.text.push_str(&serde_json::to_string(&sources)?);
        }
        if let Some(report) = &mut assessment {
            report.check_context(scope.provider, &input.text, &attachments)?;
        }
        if let Some(blocked) = assessment.take_if(|report| !report.blockers.is_empty()) {
            return self.record_attachment_assessment(blocked).map(|()| true);
        }
        let update = session.send(input)?;
        if let Some(mut report) = assessment {
            report.check_encoded_request(
                update
                    .outbound
                    .iter()
                    .map(|frame| frame.as_bytes().len() as u64)
                    .sum(),
            );
            self.record_attachment_assessment(report)?;
        }
        self.capability_scope = Some((scope, checkout.clone()));
        let reported_skills = self.reported_skills.clone().unwrap_or_else(|| {
            SkillCatalog::unavailable(match self.plan.scope.provider {
                Provider::Codex => "Codex did not report its skill inventory before dispatch",
                Provider::Claude => "Restricted Claude launch excludes implicit repository skills",
                Provider::Antigravity => {
                    "ACP reports available commands without skill source paths"
                }
            })
        });
        let requested_skill = self.plan.input.last().and_then(|message| {
            explicit_skill_request(
                self.plan.scope.provider,
                &message.text,
                reported_skills.entries.iter().chain(&filesystem_skills),
            )
        });
        let snapshot = ContextSnapshot {
            version: 10,
            context_id: self.plan.context_id.clone(),
            scope: self.plan.scope.clone(),
            base_commit: repository::base_commit(&self.plan.scope.checkout)?,
            submitted_messages: self.plan.input.clone(),
            native_history,
            capabilities: Some(capabilities),
            filesystem_skills,
            reported_skills: Some(reported_skills),
            instructions: Some(instructions),
            scoped_launch: self.scoped_launch.clone(),
            requested_skill,
            plugin_inventory: self.plugin_inventory.clone(),
            attachments,
            question_route: self.plan.question_route,
            answered_question: self.plan.answered_question.clone(),
        };
        let frames = update
            .outbound
            .iter()
            .map(|frame| frame.as_bytes().to_vec())
            .collect();
        let dispatch_id = {
            let _boundary = match &self.admission {
                Some(gate) => match self.app.block_on(gate.enter()) {
                    Some(guard) => Some(guard),
                    None => return Ok(false),
                },
                None => None,
            };
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
            dispatch_id
        };
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
        Ok(true)
    }

    fn complete_structured_planning(&mut self) -> Result<()> {
        let text = self
            .last_parent_message
            .as_deref()
            .unwrap_or(&self.streamed_answer);
        let result = StructuredPlan::decode(text);
        let data = match result {
            Ok(StructuredPlan::Question { form, normalized }) => ProviderEventData::Question {
                request_id: RequestId::Text(format!("structured:{}", self.plan.run.turn_id)),
                form,
                normalized: Some(normalized),
                route: self.plan.question_route,
            },
            Ok(StructuredPlan::Plan(text)) => ProviderEventData::PlanProposed {
                text,
                source: PlanSource::StructuredTurn,
            },
            Err(error) => {
                self.failure.cause = Some(FailureCause::InvalidResult);
                self.record(
                    ProviderEvent {
                        location: ProviderLocation::default(),
                        data: ProviderEventData::QuestionUnavailable {
                            request_id: None,
                            reason: diagnostics::preview(&format!("{error:#}")),
                        },
                    },
                    EventSource::Application,
                )?;
                return Err(error).context("invalid structured planning result");
            }
        };
        self.record(
            ProviderEvent {
                location: ProviderLocation::default(),
                data,
            },
            EventSource::Application,
        )
    }

    fn prepare_attachments(
        &mut self,
        runner: &dyn Runner,
        session: &dyn CliSession,
        checkout: &CheckoutLayout,
        capabilities: &CapabilityAssessment,
        history: &NativeHistory,
    ) -> Result<(Option<AttachmentAssessment>, Vec<AttachmentDispatch>)> {
        if self.plan.attachments.is_empty() {
            return Ok((None, Vec::new()));
        }
        let project = self.app.block_on(
            self.store
                .checkout_attachment_limits(self.plan.run.checkout_id.clone()),
        )?;
        let evidence = capabilities
            .entries
            .iter()
            .find(|entry| entry.descriptor.subject == CapabilitySubject::AttachmentLimits)
            .and_then(|entry| entry.evidence.as_ref())
            .filter(|evidence| evidence.verified());
        let mut assessment = AttachmentAssessment {
            project,
            qualified: evidence.and_then(|evidence| evidence.attachment_limits.clone()),
            qualification_id: evidence.map(|evidence| evidence.id.clone()),
            files: Vec::new(),
            blockers: Vec::new(),
            encoded_request_bytes: None,
            estimated_context_input_tokens: None,
            provider_context: self.reported.context_usage,
            provider_history_usage_unknown: !matches!(history, NativeHistory::Fresh)
                && self.reported.context_usage.is_none(),
        };
        let mut attachments = Vec::new();
        let mut budget = AttachmentBudget::new(project);
        for (index, (revision, attachment)) in self.plan.attachments.iter().enumerate() {
            let delivery = attachment.delivery();
            let metadata = &attachment.metadata;
            let encoding = if delivery == AttachmentDelivery::Native {
                session.attachment_encoding(metadata.format)
            } else {
                None
            };
            let mut decision = AttachmentDecision {
                message_revision_id: revision.clone(),
                metadata: metadata.clone(),
                delivery,
                encoding,
                qualification_id: None,
            };
            let result = (|| {
                budget.check(&metadata.source, delivery, metadata.size)?;
                if let Some(limits) = &assessment.qualified {
                    limits.check_file(index, metadata, encoding)?;
                }
                prepare_attachment(
                    &mut self.staged_attachments,
                    runner,
                    session,
                    checkout,
                    capabilities,
                    attachment,
                )
            })();
            match result {
                Ok((input, qualification_id)) => {
                    decision.qualification_id = qualification_id.clone();
                    attachments.push(AttachmentDispatch {
                        message_revision_id: revision.clone(),
                        metadata: metadata.clone(),
                        qualification_id,
                        input,
                    });
                }
                Err(error) => assessment
                    .blockers
                    .push(decision.blocker(format!("{error:#}"))),
            }
            assessment.files.push(decision);
        }
        Ok((Some(assessment), attachments))
    }

    fn record_attachment_assessment(&mut self, assessment: AttachmentAssessment) -> Result<()> {
        let blockers = assessment.blockers.clone();
        self.record(
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::AttachmentAssessed {
                    assessment: Box::new(assessment),
                },
            },
            EventSource::Application,
        )?;
        if !blockers.is_empty() {
            let reasons = blockers
                .iter()
                .map(|item| {
                    format!(
                        "{}: {}",
                        item.source.file_name().unwrap_or("attachment"),
                        item.reason
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.record(
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::AttachmentBlocked { blockers },
                },
                EventSource::Application,
            )?;
            anyhow::bail!("{reasons}");
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

    fn write_steering(
        &mut self,
        process: &mut CapturedProcess,
        update: AdapterUpdate,
    ) -> Result<()> {
        let AdapterUpdate { outbound, events } = update;
        match (events.as_slice(), outbound.as_slice()) {
            (
                [
                    event @ ProviderEvent {
                        data: ProviderEventData::SteeringSettled { .. },
                        ..
                    },
                ],
                [],
            ) => self.record(event.clone(), EventSource::Application),
            (
                [
                    event @ ProviderEvent {
                        data:
                            ProviderEventData::SteeringPrepared {
                                message_id,
                                target,
                                frame,
                            },
                        ..
                    },
                ],
                [wire],
            ) if frame == wire.as_bytes() => {
                self.record(event.clone(), EventSource::Application)?;
                let delivery = bounded_write(process, outbound);
                if delivery.is_err() {
                    self.record(
                        target.settlement(message_id.clone(), SteerOutcome::Unconfirmed),
                        EventSource::Application,
                    )?;
                }
                delivery
            }
            _ => anyhow::bail!("adapter omitted steering evidence or supplied a mismatched frame"),
        }
    }

    fn collect_suggestion(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
        commands: &mpsc::Receiver<Command>,
    ) -> Result<()> {
        if let Ok(update) = session.request_suggestion() {
            if bounded_write(process, update.outbound).is_err() {
                return self.finish_suggestion(session, SuggestionOutcome::Failed);
            }
            self.record_suggestion_events(update.events)?;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.suggestion_pending() {
            match commands.try_recv() {
                Ok(Command::Interrupt | Command::YieldToDownstream)
                | Err(mpsc::TryRecvError::Disconnected) => {
                    return self.finish_suggestion(session, SuggestionOutcome::Cancelled);
                }
                Ok(
                    Command::Approval(_, _, reply)
                    | Command::Question(_, _, reply)
                    | Command::Steer(_, _, reply),
                ) => {
                    let _ = reply.send(Err(anyhow::anyhow!("the main turn has already ended")));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return self.finish_suggestion(session, SuggestionOutcome::TimedOut);
            };
            let update = match process
                .output
                .recv_timeout(remaining.min(Duration::from_millis(50)))
            {
                Ok(Ok(frame)) => session.receive(frame),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                _ => return self.finish_suggestion(session, SuggestionOutcome::Failed),
            };
            match update {
                Ok(update) => self.record_suggestion_events(update.events)?,
                Err(_) => return self.finish_suggestion(session, SuggestionOutcome::Failed),
            }
        }
        Ok(())
    }

    fn finish_suggestion(
        &mut self,
        session: &mut dyn CliSession,
        outcome: SuggestionOutcome,
    ) -> Result<()> {
        if let Some(event) = session.abandon_suggestion(outcome) {
            self.record(event, EventSource::Supervisor)?;
        }
        Ok(())
    }

    fn record_suggestion_events(&mut self, events: Vec<ProviderEvent>) -> Result<()> {
        for event in events {
            if matches!(event.data, ProviderEventData::PromptSuggestion { .. }) {
                self.record(event, EventSource::Provider)?;
            }
        }
        Ok(())
    }

    fn deliver_decision(
        &mut self,
        process: &mut CapturedProcess,
        update: Result<AdapterUpdate>,
        decision: Decision,
        reply: oneshot::Sender<Result<()>>,
    ) -> Result<()> {
        match update {
            Ok(update) => {
                let applied: Result<()> = match decision {
                    Decision::Permission(id, approval) => (|| {
                        let seq = self.app.block_on(self.store.prepare_permission_decision(
                            self.plan.run.id.clone(),
                            self.plan.run.turn_id.clone(),
                            id,
                            approval,
                            PermissionRoute::Native,
                        ))?;
                        let applied = self.record_update(process, update, EventSource::Application);
                        let delivery = if applied.is_ok() {
                            Delivery::Submitted
                        } else {
                            Delivery::Unconfirmed
                        };
                        let settled = self
                            .app
                            .block_on(self.store.settle_permission_decision(seq, delivery));
                        applied?;
                        settled
                    })(),
                    Decision::Question(id) => self.write_question_reply(process, update, id),
                };
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
    fn write_question_reply(
        &mut self,
        process: &mut CapturedProcess,
        update: AdapterUpdate,
        id: RequestId,
    ) -> Result<()> {
        let AdapterUpdate { outbound, events } = update;
        let [event] = events.as_slice() else {
            anyhow::bail!("expected one question reply record")
        };
        let ProviderEventData::QuestionReplyPrepared {
            request_id,
            reply,
            frame,
        } = &event.data
        else {
            anyhow::bail!("adapter omitted question reply evidence")
        };
        ensure!(
            *request_id == id
                && matches!(outbound.as_slice(), [outbound] if frame == outbound.as_bytes()),
            "adapter question response does not match its request or recorded frame"
        );
        let action = reply.action;
        self.record(event.clone(), EventSource::Application)?;
        let write = bounded_write(process, outbound);
        let delivery = if write.is_ok() {
            Delivery::Submitted
        } else {
            Delivery::Unconfirmed
        };
        self.record(
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::QuestionReplySettled {
                    request_id: id,
                    action,
                    delivery,
                },
            },
            EventSource::Application,
        )?;
        write
    }
}

fn status(outcome: TurnOutcome) -> RunStatus {
    match outcome {
        TurnOutcome::Completed => RunStatus::Succeeded,
        TurnOutcome::Failed => RunStatus::Failed,
        TurnOutcome::Interrupted => RunStatus::Cancelled,
    }
}

fn prepare_attachment(
    staging: &mut StagedAttachments,
    runner: &dyn Runner,
    session: &dyn CliSession,
    checkout: &CheckoutLayout,
    capabilities: &CapabilityAssessment,
    attachment: &CapturedAttachment,
) -> Result<(AttachmentInput, Option<String>)> {
    let scope = &capabilities.scope;
    let (input, qualification_id) = match &attachment.payload {
        CapturedPayload::Native { content } => {
            let format = attachment.metadata.format;
            let encoding = session
                .attachment_encoding(format)
                .context("this session has no native route for the attachment format")?;
            let qualification = capabilities
                .entries
                .iter()
                .find(|entry| {
                    entry.descriptor.subject
                        == CapabilitySubject::NativeAttachment { format, encoding }
                })
                .and_then(|entry| entry.evidence.as_ref())
                .filter(|evidence| Verification::verified(evidence))
                .context("native delivery is unverified for this profile and checkout")?;
            let native = staging.native(runner, &attachment.metadata, content, encoding)?;
            (
                AttachmentInput::Native(native),
                Some(qualification.id.clone()),
            )
        }
        CapturedPayload::Excerpt { excerpt } => (AttachmentInput::Excerpt(excerpt.clone()), None),
        CapturedPayload::Reference { reference_path } => {
            if scope.provider == Provider::Claude
                && matches!(
                    attachment.metadata.source,
                    AttachmentSource::Checkout { .. }
                )
            {
                ensure!(
                    reference_path.starts_with(&checkout.cwd),
                    "the attachment is outside the Claude working directory"
                );
            }
            let reference = staging.reference(
                runner,
                &checkout.root,
                scope.provider,
                &attachment.metadata,
                reference_path,
            )?;
            (AttachmentInput::Reference(reference), None)
        }
    };
    input.block(scope.provider)?;
    Ok((input, qualification_id))
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
        attachments: Vec::new(),
        message_id: last.message_id.clone(),
        text,
    })
}

pub(super) fn bounded_write(process: &mut CapturedProcess, frames: Vec<WireMessage>) -> Result<()> {
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
