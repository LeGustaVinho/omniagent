use std::{
    ffi::OsStr,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    process_io::CapturedProcess,
    providers::{
        AdapterUpdate, ApprovalDecision, CliAdapter, CliSession, ProviderEventData, SessionIntent,
        SessionMode, SessionRequest, TurnInput, TurnOutcome, codex::CodexAdapter,
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};

fn send(process: &mut CapturedProcess, update: AdapterUpdate) -> Result<Vec<ProviderEventData>> {
    for message in update.outbound {
        process.input.send(message.as_bytes())?;
    }
    Ok(update.events.into_iter().map(|event| event.data).collect())
}

fn run_turn(
    executable: &Path,
    checkout: &Path,
    intent: SessionIntent,
    expected: &str,
) -> Result<String> {
    let mut session: Box<dyn CliSession> = CodexAdapter.create_session(SessionRequest {
        checkout: checkout.into(),
        intent,
        mode: SessionMode::ReadOnly,
        model: Some("gpt-6-luna".into()),
        effort: None,
    })?;
    let arguments = session.arguments();
    let arguments = arguments
        .iter()
        .map(|value| value.as_os_str())
        .collect::<Vec<&OsStr>>();
    let mut process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable,
        checkout,
        arguments: &arguments,
    })?;
    send(&mut process, session.start()?)?;

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut thread = None;
    let mut final_text = String::new();
    let mut streamed = String::new();
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Codex smoke response timed out or stopped")??;
        let update = session.receive(frame)?;
        for request in update.outbound {
            process.input.send(request.as_bytes())?;
        }
        for event in update.events {
            match event.data {
                ProviderEventData::SessionReady { model, .. } => {
                    ensure!(
                        model.as_deref() == Some("gpt-6-luna"),
                        "Codex selected a different model"
                    );
                    thread = event.location.session_id;
                }
                ProviderEventData::ModelInventory { models } => {
                    ensure!(
                        models
                            .iter()
                            .any(|model| model.canonical_id.as_deref() == Some("gpt-6-luna")),
                        "Codex did not report the selected model in its inventory"
                    );
                    let probe = session
                        .query_effective_configuration()?
                        .context("Codex did not request effective configuration")?;
                    process.input.send(probe.as_bytes())?;
                }
                ProviderEventData::ConfigurationReported { revision } => {
                    ensure!(
                        revision.is_some_and(|revision| revision.len() == 64),
                        "Codex did not report a configuration for the actual checkout"
                    );
                    let message = session.send(TurnInput {
                        message_id: expected.to_owned(),
                        text: format!("Reply with exactly this word and no punctuation: {expected}. Do not use tools."),
                    })?;
                    send(&mut process, message)?;
                }
                ProviderEventData::AssistantDelta { text } => streamed.push_str(&text),
                ProviderEventData::AssistantMessage { text } => final_text = text,
                ProviderEventData::Approval { request_id, .. } => {
                    send(
                        &mut process,
                        session.respond_approval(&request_id, ApprovalDecision::Deny)?,
                    )?;
                }
                ProviderEventData::Error { message } => bail!("Codex smoke failed: {message}"),
                ProviderEventData::TurnFinished { outcome } => {
                    ensure!(
                        outcome == TurnOutcome::Completed,
                        "Codex turn did not complete"
                    );
                    ensure!(
                        final_text.trim() == expected,
                        "Codex returned unexpected final content"
                    );
                    ensure!(
                        streamed.contains(expected),
                        "Codex response was not streamed"
                    );
                    process.control.cancel()?;
                    process.control.wait()?;
                    return thread.context("Codex did not publish a session ID");
                }
                _ => {}
            }
        }
    }
}

#[test]
#[ignore = "requires an installed, authenticated Codex CLI with access to gpt-6-luna"]
fn codex_starts_streams_and_resumes_a_read_only_session() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(checkout.path())
            .status()?
            .success(),
        "could not initialize the disposable checkout"
    );
    let executable = CodexAdapter.discover(None)?;
    assert!(
        DirectWindowsRunner
            .executable_revision(&executable)
            .is_some()
    );
    let platform = DirectWindowsRunner
        .execution_platform()
        .context("Windows version unavailable")?;
    assert!(platform.host.build > 0);
    assert!(platform.guest.is_none());
    let session_id = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Start,
        "OMNIAGENT_START",
    )?;
    let resumed = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Resume {
            provider_session_id: session_id.clone(),
        },
        "OMNIAGENT_RESUME",
    )?;
    assert_eq!(session_id, resumed);
    Ok(())
}
