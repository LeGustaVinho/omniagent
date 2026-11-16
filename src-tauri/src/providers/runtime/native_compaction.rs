use std::{
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use tokio::runtime::Handle;

use crate::{
    conversations::compaction::NativeCompactionPlan,
    dispatch::EventSource,
    process_io::CapturedProcess,
    providers::{CliSession, ProviderEventData, SessionIntent, SessionRequest, TurnOutcome},
    repository,
    runner::{CONTRACT_VERSION, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};

use super::{LaunchGate, ScopedLaunch, bounded_write, requirements::verify_desktop_profile};

const STARTUP_LIMIT: Duration = Duration::from_secs(20);
const COMPACTION_LIMIT: Duration = Duration::from_secs(90);
// The App Server can publish usage just after the terminal notification.
const TELEMETRY_GRACE: Duration = Duration::from_millis(400);

pub(crate) fn execute(
    app: &Handle,
    store: &Store,
    runner: &dyn Runner,
    gate: &LaunchGate,
    plan: NativeCompactionPlan,
) -> Result<()> {
    let launch = start(app, store, runner, gate, &plan);
    let (result, stopped) = match launch {
        Ok((mut process, mut session, _launch)) => {
            let result = app
                .block_on(store.mark_agent_running(plan.run.id.clone()))
                .and_then(|()| drive(app, store, gate, &plan, &mut process, session.as_mut()));
            let stopped = process
                .control
                .cancel()
                .and_then(|_| process.control.confirm_quiescence())
                .is_ok();
            (result, stopped)
        }
        Err(error) => (Err(error), true),
    };
    let (status, detail) = if !stopped {
        (RunStatus::Interrupted, Some("The provider process could not be confirmed stopped. Restart OmniAgent before using this conversation again.".into()))
    } else {
        match result {
            Ok(RunStatus::Succeeded) => (RunStatus::Succeeded, None),
            Ok(RunStatus::Interrupted) => (
                RunStatus::Interrupted,
                Some("The provider compaction was interrupted.".into()),
            ),
            Ok(_) => (
                RunStatus::Failed,
                Some("The provider did not confirm completion of this compaction.".into()),
            ),
            Err(_) if gate.is_closed() => (
                RunStatus::Interrupted,
                Some("The app stopped during this provider operation.".into()),
            ),
            Err(error) => (RunStatus::Failed, Some(error.to_string())),
        }
    };
    app.block_on(store.finish_native_compaction(plan.run.id, status, detail, stopped))
}

fn start(
    app: &Handle,
    store: &Store,
    runner: &dyn Runner,
    gate: &LaunchGate,
    plan: &NativeCompactionPlan,
) -> Result<(CapturedProcess, Box<dyn CliSession>, ScopedLaunch)> {
    let scope = &plan.scope;
    ensure!(
        runner.identity() == scope.runner,
        "The selected runner changed"
    );
    verify_desktop_profile(app, store, scope, runner, gate)?;
    ensure!(
        runner.executable_revision(&scope.executable) == plan.evidence.executable_revision
            && runner.execution_platform() == plan.evidence.platform,
        "The CLI executable or runner changed since the conversation dispatch"
    );
    let checkout = app.block_on(repository::verify_checkout(
        store,
        plan.run.checkout_id.clone(),
        scope.checkout.clone(),
    ))?;
    let mut session = scope.provider.adapter().create_session(SessionRequest {
        checkout: checkout.cwd.clone(),
        intent: SessionIntent::Resume {
            provider_session_id: plan.provider_session_id.clone(),
        },
        mode: scope.mode,
        model: scope.model.clone(),
        effort: scope.effort.clone(),
    })?;
    session.set_checkout_root(&checkout.root);
    session.pin_native_compaction_turn(plan.provider_turn_id.clone())?;
    let mut review = app.block_on(store.resolve_launch_review(
        plan.run.checkout_id.clone(),
        &checkout,
        scope.provider,
    ))?;
    // Maintenance has no tool or bridge authority. Apply the same scoped
    // launch defense while omitting all executable customization approvals.
    review.approved.clear();
    let mut launch = ScopedLaunch::prepare(
        scope.provider,
        &scope.cli_version,
        &checkout,
        &review,
        session.arguments(),
        None,
    )?;
    launch.apply_codex_subagent_limit(0)?;
    let args = launch
        .arguments
        .iter()
        .map(|arg| arg.as_os_str())
        .collect::<Vec<_>>();
    let _boundary = app
        .block_on(gate.enter())
        .context("The scheduler is exiting")?;
    let process = runner
        .launch(LaunchRequest {
            version: CONTRACT_VERSION,
            executable: &scope.executable,
            checkout: &checkout.cwd,
            arguments: &args,
        })
        .context("Could not start the provider for native compaction")?;
    Ok((process, session, launch))
}

fn drive(
    app: &Handle,
    store: &Store,
    gate: &LaunchGate,
    plan: &NativeCompactionPlan,
    process: &mut CapturedProcess,
    session: &mut dyn CliSession,
) -> Result<RunStatus> {
    bounded_write(process, session.start()?.outbound)?;
    let mut deadline = Instant::now() + STARTUP_LIMIT;
    let mut sent = false;
    let mut observed = None;
    loop {
        if gate.is_closed() {
            return Ok(RunStatus::Interrupted);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return observed.context(if sent {
                "The provider did not report a compaction result"
            } else {
                "The exact provider session did not become ready"
            });
        }
        let frame = match process
            .output
            .recv_timeout(remaining.min(Duration::from_millis(50)))
        {
            Ok(frame) => frame?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return observed.context("The provider stopped before confirming compaction");
            }
        };
        let update = session.receive(frame)?;
        bounded_write(process, update.outbound)?;
        for event in update.events {
            match &event.data {
                ProviderEventData::SessionReady { model, effort } if !sent => {
                    ensure!(
                        event.location.session_id.as_ref() == Some(&plan.provider_session_id)
                            && model.as_ref() == plan.scope.model.as_ref()
                            && plan
                                .scope
                                .effort
                                .as_ref()
                                .is_none_or(|expected| effort.as_ref() == Some(expected)),
                        "The provider returned a different session, model, or thinking level"
                    );
                    let request = session.compact_native()?;
                    let _boundary = app
                        .block_on(gate.enter())
                        .context("The scheduler is exiting")?;
                    app.block_on(store.start_native_compaction_submission(plan.run.id.clone()))?;
                    bounded_write(process, request.outbound)?;
                    sent = true;
                    deadline = Instant::now() + COMPACTION_LIMIT;
                }
                ProviderEventData::NativeCompactionFinished { outcome } if sent => {
                    ensure!(
                        event.location.session_id.as_ref() == Some(&plan.provider_session_id),
                        "Compaction finished on a different provider session"
                    );
                    observed = Some(super::status(*outcome));
                    deadline = Instant::now() + TELEMETRY_GRACE;
                }
                ProviderEventData::Compacted
                | ProviderEventData::Usage { .. }
                | ProviderEventData::ReasoningUsage { .. }
                    if sent =>
                {
                    if event.location.session_id.as_ref() != Some(&plan.provider_session_id)
                        || event.location.turn_id.is_none()
                        || event.location.parent_item_id.is_some()
                    {
                        continue;
                    }
                }
                ProviderEventData::Approval { .. } | ProviderEventData::Question { .. } => {
                    anyhow::bail!("The provider requested input during maintenance");
                }
                ProviderEventData::Error { .. } if !sent => {
                    anyhow::bail!("The provider could not resume the selected session");
                }
                _ => continue,
            }
            if matches!(
                event.data,
                ProviderEventData::NativeCompactionFinished {
                    outcome: TurnOutcome::Completed
                }
            ) {
                ensure!(
                    event.location.turn_id.is_some(),
                    "The provider did not identify the compacted turn"
                );
            }
            app.block_on(store.record_agent_observation(
                plan.run.id.clone(),
                EventSource::Provider,
                event,
            ))?;
        }
    }
}
