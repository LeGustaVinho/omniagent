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
        SessionRequest, TurnInput, TurnOutcome, antigravity::AntigravityAdapter,
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};

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
) -> Result<String> {
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
    let mut native_id = None;
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
                ProviderEventData::SessionReady { model, effort } => {
                    ensure!(
                        model.as_deref() == Some("gemini-3.8-flash"),
                        "ACP selected another model"
                    );
                    ensure!(
                        effort.as_deref() == Some("low"),
                        "ACP selected another thinking level"
                    );
                    native_id = event.location.session_id;
                    send(
                        &mut process,
                        &session.send(TurnInput {
                            message_id: word.into(),
                            text: format!(
                                "Reply with exactly {word} and no punctuation. Do not use tools."
                            ),
                        })?,
                    )?;
                }
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
                    return native_id.context("ACP did not send a session ID");
                }
                _ => {}
            }
        }
    }
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
