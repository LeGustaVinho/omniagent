use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::Path,
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    diagnostics,
    process::TerminalOutcome,
    process_io::{CapturedProcess, Stream},
    providers::configuration::open_locked_local_file,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};

use super::{
    CandidateGateAction, CandidateGateOutcome, CandidateRevision, PinnedGate, SelectedCandidate,
    materialize,
};

const OUTPUT_LIMIT: usize = 512 * 1024;

pub(super) fn pin_executable(path: &Path) -> io::Result<(File, String)> {
    let mut executable = open_locked_local_file(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut executable, &mut hash)?;
    Ok((executable, format!("{:x}", hash.finalize())))
}

pub(super) fn run(
    capture: &CandidateRevision,
    selected: &SelectedCandidate,
    gate: &PinnedGate,
) -> CandidateGateOutcome {
    let checkout = match materialize::materialize(capture, selected) {
        Ok(checkout) => checkout,
        Err(error) => {
            return unavailable(format!(
                "Could not materialize the exact selected tree: {error:#}"
            ));
        }
    };
    run_in_checkout(
        checkout.path(),
        &gate.gate.action,
        gate.executable_revision.as_deref(),
    )
}

pub(in crate::repository) fn run_in_checkout(
    checkout: &Path,
    action: &CandidateGateAction,
    expected_revision: Option<&str>,
) -> CandidateGateOutcome {
    let (executable, arguments, timeout_seconds) = match action {
        CandidateGateAction::Command {
            executable,
            arguments,
            timeout_seconds,
        } => (executable, arguments, timeout_seconds),
        CandidateGateAction::Unavailable { reason } => {
            return CandidateGateOutcome::CouldNotRun {
                reason: reason.clone(),
            };
        }
    };
    let Ok((executable_lock, actual_revision)) = pin_executable(executable) else {
        return unavailable(
            "The configured executable cannot be opened as a local locked file".into(),
        );
    };
    if Some(actual_revision.as_str()) != expected_revision {
        return unavailable(
            "The configured executable is missing or its revision has changed".into(),
        );
    }
    let args = arguments.iter().map(OsStr::new).collect::<Vec<_>>();
    let process = match DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable,
        checkout,
        arguments: &args,
    }) {
        Ok(process) => process,
        Err(error) => {
            return unavailable(format!(
                "Could not launch the verification command: {error}"
            ));
        }
    };
    drop(executable_lock);
    collect_output(process, timeout_seconds.map(Duration::from_secs))
}

fn unavailable(reason: String) -> CandidateGateOutcome {
    CandidateGateOutcome::CouldNotRun {
        reason: diagnostics::preview(&reason),
    }
}

fn collect_output(process: CapturedProcess, timeout: Option<Duration>) -> CandidateGateOutcome {
    let CapturedProcess {
        control,
        input,
        output,
    } = process;
    drop(input);
    let wait = control.clone();
    let worker = std::thread::spawn(move || wait.wait());
    let started = Instant::now();
    let mut cancelled = false;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut truncated = false;
    loop {
        if !cancelled && timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            cancelled = control.cancel().unwrap_or_default();
        }
        match output.recv_timeout(Duration::from_millis(40)) {
            Ok(Ok(frame)) => {
                let remaining = OUTPUT_LIMIT.saturating_sub(stdout.len() + stderr.len());
                let retain = frame
                    .data
                    .floor_char_boundary(remaining.min(frame.data.len()));
                truncated |= retain != frame.data.len();
                match frame.stream {
                    Stream::Stdout => stdout.push_str(&frame.data[..retain]),
                    Stream::Stderr => stderr.push_str(&frame.data[..retain]),
                }
            }
            Ok(Err(_)) => truncated = true,
            Err(RecvTimeoutError::Timeout)
                if worker.is_finished() && control.confirm_quiescence().is_err() =>
            {
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if worker.is_finished() => break,
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(40)),
        }
    }
    let outcome = worker
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("verification supervisor stopped")));
    let outcome = outcome.and_then(|outcome| control.confirm_quiescence().map(|()| outcome));
    match outcome {
        Ok(TerminalOutcome::Exited(0)) => CandidateGateOutcome::Passed {
            stdout,
            stderr,
            truncated,
        },
        Ok(TerminalOutcome::Exited(exit_code)) => CandidateGateOutcome::Failed {
            exit_code,
            stdout,
            stderr,
            truncated,
        },
        Ok(TerminalOutcome::Cancelled(_)) => CandidateGateOutcome::Interrupted {
            reason: "The verification command exceeded its configured timeout".into(),
            stdout,
            stderr,
            truncated,
        },
        Err(error) => CandidateGateOutcome::Interrupted {
            reason: diagnostics::preview(&format!(
                "The verification command did not settle: {error}"
            )),
            stdout,
            stderr,
            truncated,
        },
    }
}
