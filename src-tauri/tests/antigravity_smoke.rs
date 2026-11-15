use std::{
    ffi::OsStr,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    process::TerminalOutcome,
    process_io::{CapturedProcess, OutputFrame, Stream},
    providers::{
        AdapterUpdate, ApprovalDecision, CliAdapter, CliSession, ProviderEventData, SessionIntent,
        SessionMode, SessionRequest, TurnInput, TurnOutcome,
        antigravity::AntigravityAdapter,
        runtime::{Interruption, interrupt_to_completion},
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};

fn send(process: &mut CapturedProcess, update: &AdapterUpdate) -> Result<()> {
    for message in &update.outbound {
        process.input.send(message.as_bytes())?;
    }
    Ok(())
}

fn launch_ready(
    executable: &Path,
    checkout: &Path,
    intent: SessionIntent,
) -> Result<(Box<dyn CliSession>, CapturedProcess, String)> {
    let mut session = AntigravityAdapter.create_session(SessionRequest {
        checkout: checkout.into(),
        intent,
        mode: SessionMode::Plan,
        model: Some("gemini-3.8-flash".into()),
        effort: Some("low".into()),
    })?;
    let arguments = session.arguments();
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
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut observed_config = false;
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Antigravity smoke response timed out or stopped")??;
        let update = session.receive(frame)?;
        send(&mut process, &update)?;
        for event in update.events {
            match event.data {
                ProviderEventData::SessionOptions { options } => {
                    observed_config = options.iter().any(|option| {
                        option.id == "thought_level"
                            && option.current.as_deref() == Some("low")
                            && option.choices.iter().any(|choice| choice.value == "low")
                    });
                }
                ProviderEventData::SessionReady { model, effort } => {
                    ensure!(
                        observed_config,
                        "ACP did not publish the resolved thinking options"
                    );
                    ensure!(
                        model.as_deref() == Some("gemini-3.8-flash"),
                        "ACP selected another model"
                    );
                    ensure!(
                        effort.as_deref() == Some("low"),
                        "ACP selected another thinking level"
                    );
                    let id = event
                        .location
                        .session_id
                        .context("ACP did not send a session ID")?;
                    return Ok((session, process, id));
                }
                ProviderEventData::Error { message } => bail!("ACP smoke failed: {message}"),
                _ => {}
            }
        }
    }
}

fn run_turn(
    executable: &Path,
    checkout: &Path,
    intent: SessionIntent,
    word: &str,
) -> Result<String> {
    let (mut session, mut process, native_id) = launch_ready(executable, checkout, intent)?;
    send(
        &mut process,
        &session.send(TurnInput {
            message_id: word.into(),
            text: format!("Reply with exactly {word} and no punctuation. Do not use tools."),
        })?,
    )?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut answer = String::new();
    let mut delta = String::new();
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Antigravity smoke response timed out or stopped")??;
        let update = session.receive(frame)?;
        send(&mut process, &update)?;
        for event in update.events {
            match event.data {
                ProviderEventData::AssistantDelta { text } => delta.push_str(&text),
                ProviderEventData::AssistantMessage { text } => answer = text,
                ProviderEventData::Approval { request_id, .. } => send(
                    &mut process,
                    &session.respond_approval(&request_id, ApprovalDecision::Deny)?,
                )?,
                ProviderEventData::Error { message } => bail!("ACP smoke failed: {message}"),
                ProviderEventData::TurnFinished { outcome } => {
                    ensure!(
                        outcome == TurnOutcome::Completed,
                        "ACP prompt did not finish"
                    );
                    ensure!(
                        answer.trim() == word,
                        "ACP returned unexpected final content"
                    );
                    ensure!(delta.contains(word), "ACP did not stream text");
                    process.control.cancel()?;
                    process.control.wait()?;
                    return Ok(native_id);
                }
                _ => {}
            }
        }
    }
}

#[test]
#[ignore = "requires an authenticated Antigravity CLI with Gemini 3.8 Flash"]
fn antigravity_confirms_cancellation_on_the_outstanding_prompt() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    let executable = AntigravityAdapter.discover(None)?;
    let (mut session, mut process, _) =
        launch_ready(&executable, checkout.path(), SessionIntent::Start)?;
    send(
        &mut process,
        &session.send(TurnInput {
            message_id: "cancel-smoke".into(),
            text: "Write the numbers from 1 to 2000, one per line. Do not use tools.".into(),
        })?,
    )?;
    // Cancel after the provider has begun the prompt. A notification delivered
    // before the server starts processing session/prompt can race its setup.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("ACP did not start streaming before cancellation")??;
        let update = session.receive(frame)?;
        send(&mut process, &update)?;
        if update
            .events
            .iter()
            .any(|event| matches!(event.data, ProviderEventData::AssistantDelta { .. }))
        {
            break;
        }
        if update
            .events
            .iter()
            .any(|event| matches!(event.data, ProviderEventData::TurnFinished { .. }))
        {
            bail!("ACP completed without streaming the cancellation fixture");
        }
    }
    let outcome = interrupt_to_completion(&mut process, &mut *session, |_, _| Ok(()))?;
    assert_eq!(
        outcome,
        Interruption::PromptReturned(TurnOutcome::Interrupted)
    );
    process.control.cancel()?;
    process.control.wait()?;
    Ok(())
}

#[test]
fn unresponsive_acp_prompt_is_stopped_by_the_job_supervisor() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    let mut session = AntigravityAdapter.create_session(SessionRequest {
        checkout: checkout.path().into(),
        intent: SessionIntent::Start,
        mode: SessionMode::Plan,
        model: None,
        effort: None,
    })?;
    session.start()?;
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"silent","modes":{"currentModeId":"plan"}}}"#,
    ] {
        session.receive(OutputFrame {
            stream: Stream::Stdout,
            data: line.into(),
        })?;
    }
    session.send(TurnInput {
        message_id: "cancel".into(),
        text: "fixture".into(),
    })?;
    let mut quiet = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable: Path::new(env!("CARGO_BIN_EXE_omniagent")),
        checkout: checkout.path(),
        arguments: &[OsStr::new("--fixture-grandchild")],
    })?;
    let mut observed = Vec::new();
    let outcome = interrupt_to_completion(&mut quiet, &mut *session, |event, _| {
        observed.push(event.data);
        Ok(())
    })?;
    assert!(matches!(
        outcome,
        Interruption::ProcessStopped(TerminalOutcome::Cancelled(_))
    ));
    assert_eq!(
        observed,
        vec![ProviderEventData::TurnFinished {
            outcome: TurnOutcome::Interrupted
        }]
    );
    Ok(())
}

#[test]
#[ignore = "requires an authenticated Antigravity CLI with Gemini 3.8 Flash"]
fn antigravity_negotiates_streams_and_resumes_a_plan_session() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(checkout.path())
            .status()?
            .success(),
        "could not initialize the disposable checkout"
    );
    let executable = AntigravityAdapter.discover(None)?;
    let id = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Start,
        "OMNIAGENT_ACP",
    )?;
    let resumed = run_turn(
        &executable,
        checkout.path(),
        SessionIntent::Resume {
            provider_session_id: id.clone(),
        },
        "OMNIAGENT_ACP_RESUME",
    )?;
    assert_eq!(id, resumed);
    Ok(())
}
