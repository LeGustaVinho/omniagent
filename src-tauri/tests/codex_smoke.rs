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
        AdapterUpdate, ApprovalDecision, CliAdapter, CliSession, ForkBoundary, ProviderEventData,
        SessionIntent, SessionMode, SessionRequest, TurnInput, TurnOutcome, codex::CodexAdapter,
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};

mod support;

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
    compact_after: bool,
) -> Result<(String, String)> {
    let mut fork_boundary = match &intent {
        SessionIntent::Fork {
            provider_session_id,
            boundary: ForkBoundary::CompletedTurn { turn_id },
        } => Some((provider_session_id.clone(), turn_id.clone())),
        _ => None,
    };
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
    let mut finished_turn = None;
    let mut compacted = false;
    let mut compaction_finished = false;
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
                ProviderEventData::NativeForkVerified {
                    source_session_id,
                    last_turn_id,
                } => {
                    assert_eq!(
                        fork_boundary.take(),
                        Some((source_session_id, last_turn_id))
                    );
                }
                ProviderEventData::SessionReady { model, .. } => {
                    assert!(
                        fork_boundary.is_none(),
                        "fork was not verified before readiness"
                    );
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
                ProviderEventData::ConfigurationReported { revision, .. } => {
                    ensure!(
                        revision.is_some_and(|revision| revision.len() == 64),
                        "Codex did not report a configuration for the actual checkout"
                    );
                    let probe = session
                        .query_skill_inventory()?
                        .context("Codex did not request the skill catalog")?;
                    process.input.send(probe.as_bytes())?;
                }
                ProviderEventData::SkillsReported { catalog } => {
                    ensure!(
                        catalog.reported,
                        "Codex skills/list did not return the scoped catalog"
                    );
                    ensure!(
                        catalog
                            .entries
                            .iter()
                            .any(|skill| skill.name == "omniagent-skill-fixture"
                                && skill.enabled == Some(true)),
                        "Codex did not report the fixture skill"
                    );
                    let message = session.send(TurnInput {
                        attachments: Vec::new(),
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
                    finished_turn = event.location.turn_id;
                    if compact_after {
                        send(&mut process, session.compact_native()?)?;
                    }
                }
                ProviderEventData::Compacted if finished_turn.is_some() => {
                    assert_eq!(event.location.session_id, thread);
                    assert!(event.location.turn_id.is_some());
                    compacted = true;
                }
                ProviderEventData::NativeCompactionFinished { outcome } => {
                    assert!(compacted);
                    assert_eq!(outcome, TurnOutcome::Completed);
                    compaction_finished = true;
                }
                _ => {}
            }
        }
        if let Some(turn) = &finished_turn
            && (!compact_after || compaction_finished)
        {
            process.control.cancel()?;
            process.control.wait()?;
            return Ok((
                thread.context("Codex did not publish a session ID")?,
                turn.clone(),
            ));
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
    support::write_skill(checkout.path(), ".agents/skills")?;
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
    let (session_id, _) = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Start,
        "OMNIAGENT_START",
        false,
    )?;
    let (resumed, _) = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Resume {
            provider_session_id: session_id.clone(),
        },
        "OMNIAGENT_RESUME",
        false,
    )?;
    assert_eq!(session_id, resumed);
    Ok(())
}

#[test]
#[ignore = "requires an installed, authenticated Codex CLI with exact forks and native compaction"]
fn codex_forks_at_a_completed_turn_and_compacts_the_verified_fork() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    let output = Command::new("git.exe")
        .args(["init", "--quiet"])
        .current_dir(checkout.path())
        .output()?;
    ensure!(
        output.status.success(),
        "could not initialize the disposable checkout"
    );
    support::write_skill(checkout.path(), ".agents/skills")?;
    let executable = CodexAdapter.discover(None)?;
    let (source, first) = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Start,
        "WALNUT",
        false,
    )?;
    let (_, later) = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Resume {
            provider_session_id: source.clone(),
        },
        "ALMOND",
        false,
    )?;
    assert_ne!(first, later);
    let (fork, _) = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Fork {
            provider_session_id: source.clone(),
            boundary: ForkBoundary::CompletedTurn { turn_id: first },
        },
        "PECAN",
        true,
    )?;
    assert_ne!(source, fork);
    Ok(())
}
