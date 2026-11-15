use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    process_io::CapturedProcess,
    providers::{
        AdapterUpdate, ApprovalDecision, CliAdapter, ProviderEventData, SessionIntent, SessionMode,
        SessionRequest, TurnInput, TurnOutcome, claude::ClaudeAdapter,
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};

mod support;

fn send(process: &mut CapturedProcess, update: &AdapterUpdate) -> Result<()> {
    for message in &update.outbound {
        process.input.send(message.as_bytes())?;
    }
    Ok(())
}

fn run_turn(
    executable: &Path,
    checkout: &Path,
    intent: SessionIntent,
    word: &str,
    prompt: Option<&str>,
    expected: bool,
) -> Result<String> {
    let mut session = ClaudeAdapter.create_session(SessionRequest {
        checkout: checkout.into(),
        intent,
        mode: SessionMode::ReadOnly,
        model: Some("claude-haiku-5-5".into()),
        effort: None,
    })?;
    let mut arguments = session.arguments();
    if !expected {
        // Isolate slash-command loading from the model finding the fixture with a file tool.
        let tools = arguments
            .iter()
            .position(|argument| argument == "--tools")
            .context("missing Claude tools flag")?;
        arguments[tools + 1] = "".into();
    }
    let arguments = arguments
        .iter()
        .map(|value| value.as_os_str())
        .collect::<Vec<_>>();
    let mut process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable,
        checkout,
        arguments: &arguments,
    })?;
    send(&mut process, &session.start()?)?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut session_id = None;
    let mut observed_model = false;
    let mut text = String::new();
    let mut deltas = String::new();
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Claude Code smoke response timed out or stopped")??;
        let update = session.receive(frame)?;
        send(&mut process, &update)?;
        for event in update.events {
            match event.data {
                ProviderEventData::Handshake { .. } => send(
                    &mut process,
                    &session.send(TurnInput {
                        attachments: Vec::new(),
                        message_id: word.into(),
                        text: prompt.map(str::to_owned).unwrap_or_else(|| {
                            format!(
                                "Reply with exactly {word} and no punctuation. Do not use tools."
                            )
                        }),
                    })?,
                )?,
                ProviderEventData::SessionReady { model, .. } => {
                    ensure!(
                        model.as_deref() == Some("claude-haiku-5-5"),
                        "Claude selected a different model"
                    );
                    session_id = event.location.session_id;
                }
                ProviderEventData::ModelInventory { models } => {
                    observed_model = models.iter().any(|model| {
                        model.id == "haiku"
                            && model.canonical_id.as_deref() == Some("claude-haiku-5-5")
                    });
                }
                ProviderEventData::AssistantMessage { text: message } => text.push_str(&message),
                ProviderEventData::AssistantDelta { text } => deltas.push_str(&text),
                ProviderEventData::Approval { request_id, .. } => send(
                    &mut process,
                    &session.respond_approval(&request_id, ApprovalDecision::Deny)?,
                )?,
                ProviderEventData::Error { message } => bail!("Claude smoke failed: {message}"),
                ProviderEventData::TurnFinished { outcome } => {
                    ensure!(outcome == TurnOutcome::Completed, "Claude turn failed");
                    ensure!(
                        observed_model,
                        "Claude did not report a resolved Haiku model"
                    );
                    if expected {
                        ensure!(text.trim() == word, "Claude returned unexpected final text");
                        ensure!(deltas.contains(word), "Claude did not stream text");
                    } else {
                        ensure!(
                            !text.contains(word) && !deltas.contains(word),
                            "Claude loaded the project skill in safe mode"
                        );
                    }
                    process.control.cancel()?;
                    process.control.wait()?;
                    return session_id.context("Claude did not publish a session ID");
                }
                _ => {}
            }
        }
    }
}

#[test]
#[ignore = "requires an installed, authenticated Claude Code CLI with access to Haiku 5.5"]
fn claude_starts_streams_and_resumes_through_the_runner() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(checkout.path())
            .status()?
            .success(),
        "could not initialize the disposable checkout"
    );
    let executable = ClaudeAdapter.discover(None)?;
    let started = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Start,
        "OMNIAGENT_CLAUDE",
        None,
        true,
    )?;
    let resumed = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Resume {
            provider_session_id: started.clone(),
        },
        "OMNIAGENT_CLAUDE_RESUME",
        None,
        true,
    )?;
    assert_eq!(started, resumed);
    Ok(())
}

#[test]
#[ignore = "requires authenticated Claude Code and Haiku 5.5"]
fn claude_safe_mode_excludes_a_project_skill_from_stream_json_invocation() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    let reply = support::write_skill(checkout.path(), ".claude/skills")?;
    let executable = ClaudeAdapter.discover(None)?;
    run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Start,
        &reply,
        Some(
            "/omniagent-skill-fixture\nIf that skill is unavailable, reply NONE. Do not use any tools.",
        ),
        false,
    )?;
    Ok(())
}
