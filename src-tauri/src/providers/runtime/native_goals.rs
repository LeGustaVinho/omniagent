use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use crate::{
    dispatch::EventSource,
    jobs::{DispatchRejection, FailureCause, GoalStopAction},
    process_io::CapturedProcess,
    providers::{
        CliSession, ProviderEvent, ProviderEventData, RequestId, TurnOutcome,
        goal::{NativeGoalAction, NativeGoalCommand, NativeGoalControlOutcome, NativeGoalState},
    },
    runs::RunStatus,
};

use super::{
    TurnExecution, bounded_write, effective_qualification_matches, interrupt_to_completion,
};

pub(super) struct NativeGoalLoop {
    first_turn_deadline: Instant,
    pub(super) profile_changed: bool,
    session: String,
    pending: Option<(RequestId, NativeGoalAction)>,
    confirmation: Option<NativeGoalAction>,
    confirmed_state: Option<NativeGoalState>,
    start_confirmed: bool,
    terminal: Option<(NativeGoalState, Instant)>,
    turn: Option<String>,
    last_turn: Option<TurnOutcome>,
}

impl NativeGoalLoop {
    fn new(session: String) -> Self {
        Self {
            first_turn_deadline: Instant::now() + Duration::from_secs(30),
            profile_changed: false,
            session,
            pending: None,
            confirmation: None,
            confirmed_state: None,
            start_confirmed: false,
            terminal: None,
            turn: None,
            last_turn: None,
        }
    }

    pub(super) fn observe(&mut self, event: &ProviderEvent) {
        if event.location.session_id.as_deref() != Some(&self.session)
            || event.location.parent_item_id.is_some()
        {
            return;
        }
        match &event.data {
            ProviderEventData::TurnStarted => self.turn = event.location.turn_id.clone(),
            ProviderEventData::TurnFinished { outcome }
                if self.turn.as_ref() == event.location.turn_id.as_ref() =>
            {
                self.turn = None;
                self.last_turn = Some(*outcome);
            }
            ProviderEventData::GoalObserved { observation } => {
                if !matches!(
                    observation.state,
                    NativeGoalState::Active | NativeGoalState::Unknown
                ) && self
                    .terminal
                    .as_ref()
                    .is_none_or(|(previous, _)| *previous != observation.state)
                {
                    self.terminal =
                        Some((observation.state, Instant::now() + Duration::from_secs(20)));
                }
            }
            ProviderEventData::NativeGoalControlSettled {
                action,
                request_id,
                result,
            } if self
                .pending
                .as_ref()
                .is_some_and(|(id, expected)| id == request_id && expected == action) =>
            {
                self.pending = None;
                self.confirmation =
                    matches!(result, NativeGoalControlOutcome::Confirmed { .. }).then_some(*action);
                self.confirmed_state = match result {
                    NativeGoalControlOutcome::Confirmed { observation } => Some(observation.state),
                    _ => None,
                };
                if *action == NativeGoalAction::Start {
                    self.start_confirmed = self.confirmation.is_some();
                }
            }
            _ => {}
        }
    }

    pub(super) fn should_stop(&self) -> bool {
        let now = Instant::now();
        self.profile_changed
            || (!self.start_confirmed && self.pending.is_none())
            || (now >= self.first_turn_deadline
                && (!self.start_confirmed || (self.turn.is_none() && self.last_turn.is_none())))
            || self
                .terminal
                .is_some_and(|(_, deadline)| now >= deadline && self.result().is_none())
            || (self.terminal.is_none()
                && self.turn.is_none()
                && matches!(
                    self.last_turn,
                    Some(TurnOutcome::Failed | TurnOutcome::Interrupted)
                ))
    }

    pub(super) fn result(&self) -> Option<Option<&'static str>> {
        if !self.start_confirmed || self.turn.is_some() {
            return None;
        }
        match self.terminal?.0 {
            NativeGoalState::Completed if self.last_turn == Some(TurnOutcome::Completed) => {
                Some(Some("completed"))
            }
            NativeGoalState::Blocked if self.last_turn == Some(TurnOutcome::Completed) => {
                Some(Some("blocked"))
            }
            NativeGoalState::BudgetLimited | NativeGoalState::UsageLimited
                if self.last_turn == Some(TurnOutcome::Completed) =>
            {
                Some(Some("limit_reached"))
            }
            NativeGoalState::Paused | NativeGoalState::Cleared | NativeGoalState::Cancelled => {
                Some(None)
            }
            _ => None,
        }
    }
}

impl TurnExecution {
    pub(super) fn start_native_goal(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
    ) -> Result<bool> {
        let seed = self
            .plan
            .native_goal_seed
            .as_ref()
            .context("Missing native goal scope")?;
        let (scope, _) = self
            .capability_scope
            .as_ref()
            .context("Missing effective native goal profile")?;
        let assessment = self.app.block_on(
            self.store
                .resolve_capability_assessment(scope.clone(), self.reported.descriptors(scope)),
        )?;
        if !effective_qualification_matches(&seed.proof, &assessment, seed.token_budget.is_some())?
        {
            self.app.block_on(
                self.store
                    .invalidate_native_goal_qualification(seed.proof.evidence_id.clone()),
            )?;
            self.reject_dispatch(DispatchRejection::NativeGoalUnverified)?;
            return Ok(false);
        }
        let update = session.control_native_goal(NativeGoalCommand::Start {
            objective: seed.objective.clone(),
            token_budget: seed.token_budget,
        })?;
        let thread = update
            .events
            .first()
            .and_then(|event| event.location.session_id.clone())
            .context("Missing native goal session")?;
        if !self.app.block_on(self.store.activate_native_goal_executor(
            self.plan.run.id.clone(),
            seed.proof.evidence_id.clone(),
        ))? {
            return Ok(false);
        }
        self.native_goal = Some(NativeGoalLoop::new(thread));
        self.write_native_goal_control(process, update)?;
        Ok(true)
    }

    fn write_native_goal_control(
        &mut self,
        process: &mut CapturedProcess,
        update: crate::providers::AdapterUpdate,
    ) -> Result<()> {
        let [event] = update.events.as_slice() else {
            anyhow::bail!("Missing prepared native goal control");
        };
        let ProviderEventData::NativeGoalControlPrepared {
            request_id,
            action,
            frame,
        } = &event.data
        else {
            anyhow::bail!("Missing prepared native goal control");
        };
        let native = self
            .native_goal
            .as_mut()
            .context("Native goal executor has not started")?;
        ensure!(
            event.location.session_id.as_deref() == Some(&native.session)
                && native.pending.is_none()
                && matches!(update.outbound.as_slice(), [wire] if wire.as_bytes() == frame),
            "Native goal control changed before dispatch"
        );
        native.pending = Some((request_id.clone(), *action));
        native.confirmation = None;
        native.confirmed_state = None;
        self.record(event.clone(), EventSource::Application)?;
        bounded_write(process, update.outbound)
    }

    pub(super) fn stop_native_goal(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
    ) -> Result<RunStatus> {
        let mut action = self
            .app
            .block_on(self.store.native_goal_stop_action(self.plan.run.id.clone()))?;
        self.wait_native_control(process, session)?;
        for _ in 0..2 {
            if self
                .native_goal
                .as_ref()
                .is_some_and(|goal| goal.pending.is_some())
            {
                break;
            }
            let command = match action {
                GoalStopAction::Pause => NativeGoalCommand::Pause,
                GoalStopAction::Cancel => NativeGoalCommand::Cancel,
            };
            self.write_native_goal_control(process, session.control_native_goal(command)?)?;
            self.wait_native_control(process, session)?;
            let latest = self
                .app
                .block_on(self.store.native_goal_stop_action(self.plan.run.id.clone()))?;
            if latest == action {
                break;
            }
            action = latest;
        }
        let (confirmed, has_turn) = self
            .native_goal
            .as_ref()
            .map(|goal| {
                (
                    goal.confirmation
                        == Some(match action {
                            GoalStopAction::Pause => NativeGoalAction::Pause,
                            GoalStopAction::Cancel => NativeGoalAction::Cancel,
                        }),
                    goal.turn.is_some(),
                )
            })
            .context("Native goal executor has not started")?;
        if !confirmed {
            self.failure.cause = Some(FailureCause::StopUnconfirmed);
        } else if has_turn {
            interrupt_to_completion(process, session, |event, source| self.record(event, source))?;
        }
        Ok(if action == GoalStopAction::Cancel && confirmed {
            RunStatus::Cancelled
        } else {
            RunStatus::Interrupted
        })
    }

    pub(super) fn confirm_native_terminal(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
    ) -> Result<bool> {
        self.write_native_goal_control(
            process,
            session.control_native_goal(NativeGoalCommand::Status)?,
        )?;
        self.wait_native_control(process, session)?;
        let goal = self
            .native_goal
            .as_mut()
            .context("Native goal executor has not started")?;
        let Some(state) = goal.confirmed_state else {
            return Ok(false);
        };
        goal.terminal = (state != NativeGoalState::Active)
            .then(|| (state, Instant::now() + Duration::from_secs(20)));
        Ok(true)
    }

    fn wait_native_control(
        &mut self,
        process: &mut CapturedProcess,
        session: &mut dyn CliSession,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .native_goal
            .as_ref()
            .is_some_and(|goal| goal.pending.is_some())
        {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let Ok(frame) = process.output.recv_timeout(remaining) else {
                break;
            };
            self.record_update(process, session.receive(frame?)?, EventSource::Provider)?;
        }
        Ok(())
    }
}
