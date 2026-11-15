use std::{
    ffi::OsStr,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};
use ts_rs::TS;

use crate::{
    process::TerminalOutcome,
    process_io::{CapturedProcess, Stream},
    protection,
    providers::SessionMode,
    repository,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

use super::{
    DispatchRejection, FailureCause, FailureObservation, FailurePhase, Job, JobOrigin, JobRequest,
    LaunchGate, ObservedProcessExit, PipelineDispatch, insert_job, load_job,
    pipeline::resolve_node_checkout, record_job_failure, record_launch_intent,
    release_settled_writer,
};

const OUTPUT_LIMIT: usize = 512 * 1024;

/// A local command bound by a trusted workflow executor. Direct execution can write to
/// the repository, so it runs under the same authorization as an approved agent writer.
pub struct PipelineCommand {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct CommandInvocation {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub checkout: PathBuf,
    pub runner: String,
    revision: String,
}

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct CommandJobOutput {
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub process: Option<ObservedProcessExit>,
}

pub(super) struct CommandAttempt {
    pub run: RunRecord,
    pub invocation: CommandInvocation,
}

pub(super) fn load_command(db: &Connection, job_id: &str) -> Result<Option<CommandInvocation>> {
    let encrypted: Option<Vec<u8>> = db
        .query_row(
            "SELECT invocation FROM job_commands WHERE job_id = ?1",
            [job_id],
            |row| row.get(0),
        )
        .optional()?;
    encrypted
        .map(|bytes| Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?))
        .transpose()
}

pub(super) fn load_command_output(db: &Connection, run: &str) -> Result<Option<CommandJobOutput>> {
    let bytes: Option<Vec<u8>> = db
        .query_row(
            "SELECT output FROM job_command_outputs WHERE run_id = ?1",
            [run],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?))
        .transpose()
}

impl Store {
    pub async fn command_job_run(&self, project: String, run: String) -> Result<Option<RunRecord>> {
        self.execute(move |db| {
            let allowed: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs r JOIN job_attempts a ON a.run_id = r.id
                 JOIN jobs j ON j.id = a.job_id WHERE r.kind = 'command' AND r.id = ?1 AND j.project_id = ?2)",
                params![run, project], |row| row.get(0),
            )?;
            allowed.then(|| runs::load_run(db, &run)).transpose()
        }).await
    }

    pub async fn submit_pipeline_command_job(
        &self,
        binding: PipelineDispatch,
        checkout: String,
        task: String,
        dependencies: Vec<String>,
        command: PipelineCommand,
    ) -> Result<Job> {
        ensure!(
            command.executable.is_absolute(),
            "A registered command needs an absolute executable path"
        );
        let (executable, revision) = tokio::task::spawn_blocking(move || {
            let executable = dunce::canonicalize(&command.executable)?;
            let revision = DirectWindowsRunner
                .executable_revision(&executable)
                .context("The command executable could not be identified")?;
            Ok::<_, anyhow::Error>((executable, revision))
        })
        .await??;
        let arguments = command.arguments;
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let (project, cwd) = resolve_node_checkout(&tx, &binding, &checkout)?;
                let mut request = JobRequest {
                    checkout_id: Some(checkout.clone()),
                    task_id: Some(task),
                    mode: SessionMode::ApprovedWrite,
                    instructions: serde_json::to_string_pretty(&serde_json::json!({
                        "executable": executable, "arguments": arguments, "revision": revision,
                    }))?,
                    dependencies,
                    requires_bridge: false,
                };
                request.normalize(&checkout)?;
                let job = insert_job(
                    &tx,
                    project.clone(),
                    None,
                    JobOrigin::Pipeline,
                    request,
                    0,
                    Some(&binding),
                )?;
                let invocation = CommandInvocation {
                    executable,
                    arguments,
                    checkout: cwd.into(),
                    runner: DirectWindowsRunner.identity().into(),
                    revision,
                };
                tx.execute(
                    "INSERT INTO job_commands (job_id, invocation) VALUES (?1, ?2)",
                    params![
                        job.id,
                        protection::protect(&serde_json::to_vec(&invocation)?)?,
                    ],
                )?;
                let job =
                    load_job(&tx, &project, &job.id)?.context("The command job disappeared")?;
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }

    pub async fn command_job_output(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<CommandJobOutput>> {
        self.execute(move |db| {
            let bytes: Option<Vec<u8>> = db.query_row(
                "SELECT o.output FROM job_command_outputs o JOIN job_attempts a ON a.run_id = o.run_id
                 JOIN jobs j ON j.id = a.job_id WHERE o.run_id = ?1 AND j.project_id = ?2",
                params![run, project], |row| row.get(0),
            ).optional()?;
            bytes.map(|bytes| Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?)).transpose()
        }).await
    }
}

pub(super) fn start_command(
    store: Store,
    runner: Arc<dyn Runner>,
    attempt: CommandAttempt,
    admission: LaunchGate,
) -> (watch::Sender<bool>, JoinHandle<Result<()>>) {
    let (interrupt, stopping) = watch::channel(false);
    let app = Handle::current();
    let worker = tokio::task::spawn_blocking(move || {
        let mut failure = FailureObservation {
            phase: FailurePhase::Preparation,
            ..Default::default()
        };
        let mut output = None;
        let outcome = run_command(
            &app,
            &store,
            runner.as_ref(),
            &attempt,
            &admission,
            &stopping,
            &mut failure,
            &mut output,
        );
        let status = match &outcome {
            Ok(status) => *status,
            Err(_) => RunStatus::Failed,
        };
        let run_id = attempt.run.id;
        let output = output
            .map(|details| -> Result<_> {
                Ok(protection::protect(&serde_json::to_vec(&details)?)?)
            })
            .transpose();
        // A failure to persist optional output must not erase a confirmed outcome.
        let (output, output_error) = match output {
            Ok(output) => (output, None),
            Err(error) => (None, Some(error)),
        };
        app.block_on(store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            settle_command(&tx, &run_id, status, failure, output)?;
            tx.commit()?;
            Ok(())
        }))?;
        store.job_signal.send_replace(());
        if let Some(error) = output_error {
            return Err(error)
                .context("Command outcome was recorded but its output could not be saved");
        }
        outcome.map(|_| ())
    });
    (interrupt, worker)
}

fn run_command(
    app: &Handle,
    store: &Store,
    runner: &dyn Runner,
    attempt: &CommandAttempt,
    admission: &LaunchGate,
    stopping: &watch::Receiver<bool>,
    failure: &mut FailureObservation,
    output: &mut Option<CommandJobOutput>,
) -> Result<RunStatus> {
    let run = &attempt.run;
    let invocation = &attempt.invocation;
    if admission.is_closed() {
        return Ok(RunStatus::Interrupted);
    }
    let requirements = app.block_on(store.job_dispatch_requirements(run.id.clone()))?;
    if !requirements.capabilities.is_empty() || !requirements.customizations.is_empty() {
        app.block_on(store.record_dispatch_rejection(
            run.id.clone(),
            DispatchRejection::Requirements {
                capabilities: requirements.capabilities,
                customizations: requirements.customizations,
            },
        ))?;
        anyhow::bail!("The command cannot verify the rule's CLI requirements");
    }
    if runner.identity() != invocation.runner
        || runner
            .executable_revision(&invocation.executable)
            .as_deref()
            != Some(&invocation.revision)
    {
        app.block_on(
            store.record_dispatch_rejection(run.id.clone(), DispatchRejection::ProfileUnavailable),
        )?;
        anyhow::bail!("The registered command executable changed or could not be verified");
    }
    app.block_on(repository::verify_checkout(
        store,
        run.checkout_id.clone(),
        invocation.checkout.clone(),
    ))
    .inspect_err(|_| failure.cause = Some(FailureCause::WorktreeUnavailable))?;
    failure.phase = FailurePhase::EvidenceCapture;
    ensure!(
        app.block_on(repository::capture_worktree_start(store, run.id.clone()))?,
        "The command needs a managed worktree reservation"
    );
    failure.phase = FailurePhase::Launch;
    let args = invocation
        .arguments
        .iter()
        .map(OsStr::new)
        .collect::<Vec<_>>();
    let process = {
        let Some(_boundary) = app.block_on(admission.enter()) else {
            return Ok(RunStatus::Interrupted);
        };
        let id = run.id.clone();
        let allowed = app
            .block_on(store.execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let allowed = record_launch_intent(&tx, &id)?;
                tx.commit()?;
                Ok(allowed)
            }))?
            .context("Job authorization changed before command launch")?;
        if !allowed {
            return Ok(RunStatus::Cancelled);
        }
        runner
            .launch(LaunchRequest {
                version: CONTRACT_VERSION,
                executable: &invocation.executable,
                checkout: &invocation.checkout,
                arguments: &args,
            })
            .inspect_err(|_| failure.cause = Some(FailureCause::LaunchRejected))?
    };
    let id = run.id.clone();
    app.block_on(store.execute(move |db| {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = runs::load_run(&tx, &id)?;
        runs::advance_run(&tx, &run, RunStatus::Running)?;
        tx.commit()?;
        Ok(())
    }))?;

    failure.phase = FailurePhase::Execution;
    let control = process.control.clone();
    let (ended, logs) = collect_output(process, stopping, admission);
    *output = Some(logs);
    failure.phase = FailurePhase::ProcessStop;
    let ended = ended.inspect_err(|_| failure.cause = Some(FailureCause::StopUnconfirmed))?;
    failure.process = Some(ended.into());
    output.as_mut().expect("command output is present").process = failure.process;
    control
        .confirm_quiescence()
        .inspect_err(|_| failure.cause = Some(FailureCause::StopUnconfirmed))?;
    failure.phase = FailurePhase::EvidenceCapture;
    app.block_on(repository::save_worktree_candidate(store, run.id.clone()))
        .inspect_err(|_| failure.cause = Some(FailureCause::EvidenceUnavailable))?;
    failure.phase = FailurePhase::Execution;
    Ok(match ended {
        TerminalOutcome::Exited(0) => RunStatus::Succeeded,
        TerminalOutcome::Exited(_) => {
            failure.cause = Some(FailureCause::ProcessExit);
            RunStatus::Failed
        }
        TerminalOutcome::Cancelled(_) if admission.is_closed() => {
            failure.cause = Some(FailureCause::Shutdown);
            RunStatus::Interrupted
        }
        TerminalOutcome::Cancelled(_) => RunStatus::Cancelled,
    })
}

fn collect_output(
    process: CapturedProcess,
    stopping: &watch::Receiver<bool>,
    admission: &LaunchGate,
) -> (std::io::Result<TerminalOutcome>, CommandJobOutput) {
    let CapturedProcess {
        control,
        input,
        output,
    } = process;
    drop(input);
    let mut logs = CommandJobOutput::default();
    let (send, receive) = mpsc::channel();
    thread::scope(|scope| {
        let wait = control.clone();
        scope.spawn(move || {
            let _ = send.send(wait.wait());
        });
        let mut requested = false;
        let mut ended = None;
        loop {
            if !requested && (admission.is_closed() || *stopping.borrow()) {
                requested = control.cancel().is_ok();
            }
            if ended.is_none() {
                ended = receive.try_recv().ok();
                if ended.as_ref().is_some_and(Result::is_err) {
                    let _ = control.cancel();
                    break;
                }
            }
            match output.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(frame)) => {
                    let remaining =
                        OUTPUT_LIMIT.saturating_sub(logs.stdout.len() + logs.stderr.len());
                    let limit = frame
                        .data
                        .floor_char_boundary(remaining.min(frame.data.len()));
                    logs.truncated |= limit < frame.data.len();
                    match frame.stream {
                        Stream::Stdout => logs.stdout.push_str(&frame.data[..limit]),
                        Stream::Stderr => logs.stderr.push_str(&frame.data[..limit]),
                    }
                }
                Ok(Err(_)) => logs.truncated = true,
                Err(mpsc::RecvTimeoutError::Disconnected) if ended.is_some() => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        (ended.expect("command supervisor reported an outcome"), logs)
    })
}

fn settle_command(
    db: &Connection,
    id: &str,
    status: RunStatus,
    failure: FailureObservation,
    output: Option<Vec<u8>>,
) -> Result<()> {
    let run = runs::load_run(db, id)?;
    runs::advance_run(db, &run, status)?;
    if let Some(output) = output {
        db.execute(
            "INSERT INTO job_command_outputs (run_id, output) VALUES (?1, ?2)",
            params![id, output],
        )?;
    }
    record_job_failure(db, id, status, Some(failure))?;
    release_settled_writer(db, id)?;
    if status == RunStatus::Interrupted {
        super::requeue_unlaunched_job(db, id)?;
    }
    super::observe_child_run(db, id)?;
    Ok(())
}

pub(crate) fn recover_commands(db: &Connection) -> Result<()> {
    let active = db
        .prepare(
            "SELECT id FROM runs WHERE kind = 'command' AND status IN ('starting', 'running')",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in active {
        settle_command(
            db,
            &id,
            RunStatus::Interrupted,
            FailureObservation {
                phase: FailurePhase::Restart,
                cause: Some(FailureCause::RuntimeRestart),
                process: None,
            },
            None,
        )?;
    }
    Ok(())
}
