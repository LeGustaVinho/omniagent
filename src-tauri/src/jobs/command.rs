use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::windows::fs::OpenOptionsExt,
    path::Path,
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
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::{
    process::TerminalOutcome,
    process_io::{CapturedProcess, Stream},
    protection,
    providers::SessionMode,
    repository,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
    runs::{self, RunRecord, RunStatus},
    store::Store,
    workflows::{
        CommandData, CommandOutputFormat, WorkflowJobInput, bind_job_input, publish_command_result,
        stage_job_input,
    },
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
pub(crate) struct PinnedCommandScript {
    pub path: PathBuf,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct CommandExchange {
    pub input: Vec<u8>,
    pub format: CommandOutputFormat,
}

pub(crate) struct PreparedCommand {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub revision: String,
    pub script: Option<PinnedCommandScript>,
    pub exchange: Option<CommandExchange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct CommandInvocation {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub checkout: PathBuf,
    pub runner: String,
    revision: String,
    #[serde(default)]
    script: Option<PinnedCommandScript>,
    #[serde(default)]
    exchange: Option<CommandExchange>,
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
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let prepared = PreparedCommand {
                    executable,
                    arguments: command.arguments,
                    revision,
                    script: None,
                    exchange: None,
                };
                let job = queue_pipeline_command(
                    &tx,
                    &binding,
                    checkout,
                    task,
                    dependencies,
                    prepared,
                    None,
                )?;
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

pub(crate) fn queue_pipeline_command(
    db: &Connection,
    binding: &PipelineDispatch,
    checkout: String,
    task: String,
    dependencies: Vec<String>,
    prepared: PreparedCommand,
    workflow_input: Option<&WorkflowJobInput>,
) -> Result<Job> {
    let (project, cwd) = resolve_node_checkout(db, binding, &checkout)?;
    if workflow_input.is_some() {
        let previous: Option<(String, String)> = db.query_row(
            "SELECT p.job_id, p.release_id FROM job_pipeline_origins p JOIN job_commands c ON c.job_id = p.job_id
             WHERE p.pipeline_run_id = ?1 AND p.node_instance_id = ?2 AND p.plan_revision IS ?3",
            params![binding.pipeline_run_id, binding.node_instance_id, binding.plan_revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        if let Some((id, release)) = previous {
            ensure!(
                binding.release_id.as_ref() == Some(&release),
                "The Command was already scheduled under a different release"
            );
            return load_job(db, &project, &id)?.context("The existing command job disappeared");
        }
    }
    let mut request = JobRequest {
        checkout_id: Some(checkout.clone()),
        task_id: Some(task),
        mode: SessionMode::ApprovedWrite,
        instructions: serde_json::to_string_pretty(&serde_json::json!({
            "executable": prepared.executable,
            "arguments": prepared.arguments,
            "revision": prepared.revision,
        }))?,
        dependencies,
        requires_bridge: false,
    };
    request.normalize(&checkout)?;
    if let Some(input) = workflow_input {
        stage_job_input(db, &project, binding, &request, input)?;
    }
    let job = insert_job(
        db,
        project.clone(),
        None,
        JobOrigin::Pipeline,
        request,
        0,
        Some(binding),
    )?;
    let invocation = CommandInvocation {
        executable: prepared.executable,
        arguments: prepared.arguments,
        checkout: cwd.into(),
        runner: DirectWindowsRunner.identity().into(),
        revision: prepared.revision,
        script: prepared.script,
        exchange: prepared.exchange,
    };
    db.execute(
        "INSERT INTO job_commands (job_id, invocation) VALUES (?1, ?2)",
        params![
            job.id,
            protection::protect(&serde_json::to_vec(&invocation)?)?
        ],
    )?;
    if let Some(input) = workflow_input {
        bind_job_input(db, &job.id, binding, input)?;
    }
    load_job(db, &project, &job.id)?.context("The command job disappeared")
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
        let mut result = None;
        let outcome = run_command(
            &app,
            &store,
            runner.as_ref(),
            &attempt,
            &admission,
            &stopping,
            &mut failure,
            &mut output,
            &mut result,
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
        let (status, result) = if output_error.is_some() && attempt.invocation.exchange.is_some() {
            failure.phase = FailurePhase::ResultValidation;
            failure.cause = Some(FailureCause::EvidenceUnavailable);
            (RunStatus::Failed, None)
        } else {
            (status, result)
        };
        app.block_on(store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            settle_command(&tx, &run_id, status, failure, output)?;
            if let Some(result) = result {
                publish_command_result(&tx, &run_id, result)?;
            }
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
    result: &mut Option<CommandData>,
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
    let pinned = if invocation.exchange.is_some() {
        Some(pinned_files(invocation))
    } else {
        None
    };
    if runner.identity() != invocation.runner
        || runner
            .executable_revision(&invocation.executable)
            .as_deref()
            != Some(&invocation.revision)
        || pinned.as_ref().is_some_and(Result::is_err)
        || invocation.script.as_ref().is_some_and(|script| {
            runner.executable_revision(&script.path).as_deref() != Some(&script.revision)
        })
    {
        app.block_on(
            store.record_dispatch_rejection(run.id.clone(), DispatchRejection::ProfileUnavailable),
        )?;
        anyhow::bail!("The registered executable or script changed or could not be verified");
    }
    let _pinned = pinned.transpose()?;
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
    let (ended, logs, input) = collect_output(
        process,
        invocation
            .exchange
            .as_ref()
            .map(|exchange| exchange.input.as_slice()),
        stopping,
        admission,
    );
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
    if matches!(ended, TerminalOutcome::Exited(0))
        && let Some(exchange) = &invocation.exchange
    {
        input.inspect_err(|_| failure.cause = Some(FailureCause::TransportClosed))?;
        failure.phase = FailurePhase::ResultValidation;
        let parsed = (|| {
            let logs = output.as_ref().context("Command stdout is unavailable")?;
            ensure!(
                !logs.truncated,
                "Command output exceeded its capture limit or was not valid UTF-8"
            );
            CommandData::parse(exchange.format, &logs.stdout)
        })()
        .inspect_err(|_| failure.cause = Some(FailureCause::InvalidResult))?;
        *result = Some(parsed);
    }
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

fn pinned_files(invocation: &CommandInvocation) -> Result<Vec<File>> {
    let open = |path: &Path| -> Result<File> {
        ensure!(
            dunce::canonicalize(path)? == path,
            "The registered command path changed"
        );
        // Keep the executable and script read-only for the duration of the supervised tree.
        Ok(OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)?)
    };
    let mut files = vec![open(&invocation.executable)?];
    if let Some(script) = &invocation.script {
        files.push(open(&script.path)?);
    }
    Ok(files)
}

fn collect_output(
    process: CapturedProcess,
    stdin: Option<&[u8]>,
    stopping: &watch::Receiver<bool>,
    admission: &LaunchGate,
) -> (
    io::Result<TerminalOutcome>,
    CommandJobOutput,
    io::Result<()>,
) {
    let CapturedProcess {
        control,
        mut input,
        output,
    } = process;
    let mut logs = CommandJobOutput::default();
    let (send, receive) = mpsc::channel();
    thread::scope(|scope| {
        let writer = stdin.map(|data| scope.spawn(move || input.send(data)));
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
        let input_result = writer.map_or(Ok(()), |writer| {
            writer
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("The command stdin worker stopped")))
        });
        (
            ended.expect("command supervisor reported an outcome"),
            logs,
            input_result,
        )
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
    super::settle_owned_goal_cancellations(db)?;
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
