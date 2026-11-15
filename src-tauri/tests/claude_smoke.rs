use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    process_io::{CapturedProcess, Stream},
    providers::{
        AdapterUpdate, ApprovalDecision, CliAdapter, ProviderEventData, SessionIntent, SessionMode,
        SessionRequest, TurnInput, TurnOutcome, claude::ClaudeAdapter,
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};
use serde_json::{Value, json};

mod support;

fn write_frame(process: &mut CapturedProcess, body: Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(&body)?;
    bytes.push(b'\n');
    process.input.send(&bytes)?;
    Ok(())
}

fn goal_prompt(process: &mut CapturedProcess, text: &str) -> Result<()> {
    write_frame(
        process,
        json!({
            "type":"user",
            "message":{"role":"user","content":text},
            "parent_tool_use_id":null,
        }),
    )
}

fn goal_frames(
    process: &mut CapturedProcess,
    mut observe: impl FnMut(&mut CapturedProcess, Value) -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Claude Goal Mode probe timed out")??;
        if frame.stream == Stream::Stdout && observe(process, serde_json::from_str(&frame.data)?)? {
            return Ok(());
        }
    }
}

#[test]
#[ignore = "requires an installed authenticated Claude Code with Haiku 5.5 and native Goal Mode"]
fn claude_stream_goal_evaluates_continues_and_survives_interrupt() -> Result<()> {
    let root = tempfile::tempdir()?;
    let checkout = root.path().join("checkout");
    fs::create_dir(&checkout)?;
    let checkout = dunce::canonicalize(checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    let settings = root.path().join("settings.json");
    fs::write(
        &settings,
        serde_json::to_vec(&json!({
            "hooks": {"Stop":[{"hooks":[{"type":"command","command":"cmd.exe /d /c exit 0"}]}]},
            "autoMemoryEnabled":false,
        }))?,
    )?;
    let mcp = root.path().join("mcp.json");
    fs::write(&mcp, r#"{"mcpServers":{}}"#)?;
    let executable = ClaudeAdapter.discover(None)?;
    let launch = |intent| -> Result<CapturedProcess> {
        let adapter = ClaudeAdapter.create_session(SessionRequest {
            checkout: checkout.clone(),
            intent,
            mode: SessionMode::ReadOnly,
            model: Some("claude-haiku-5-5".into()),
            effort: None,
        })?;
        let mut arguments = adapter.arguments();
        arguments.retain(|argument| argument != "--safe-mode");
        let mcp_flag = arguments
            .iter()
            .position(|argument| argument == "--mcp-config")
            .context("missing MCP flag")?;
        arguments[mcp_flag + 1] = mcp.clone().into_os_string();
        arguments.extend([
            "--restricted".into(),
            "--system-prompt-snapshot".into(),
            "off".into(),
            "--settings".into(),
            settings.clone().into_os_string(),
        ]);
        let arguments = arguments
            .iter()
            .map(|argument| argument.as_os_str())
            .collect::<Vec<_>>();
        let mut process = DirectWindowsRunner.launch(LaunchRequest {
            version: CONTRACT_VERSION,
            executable: &executable,
            checkout: &checkout,
            arguments: &arguments,
        })?;
        write_frame(
            &mut process,
            json!({
                "type":"control_request","request_id":"fixture-init","request":{"subtype":"initialize","hooks":{}}
            }),
        )?;
        goal_frames(&mut process, |_, frame| {
            if frame["response"]["request_id"] != "fixture-init" {
                return Ok(false);
            }
            assert_eq!(frame["response"]["subtype"], "success");
            assert!(
                frame["response"]["response"]["commands"]
                    .as_array()
                    .is_some_and(|commands| {
                        commands
                            .iter()
                            .any(|command| command["name"] == "goal" && command["builtin"] == true)
                    })
            );
            Ok(true)
        })?;
        Ok(process)
    };
    let mut completed = launch(SessionIntent::Start)?;
    goal_prompt(
        &mut completed,
        "/goal Check the conversation for at least two distinct main model replies. The first reply must start with FIRST_FIXTURE_STEP and the next with FINAL_FIXTURE_STEP. Use no tools. Stop after the second marker has been emitted in its own reply.",
    )?;
    let mut property = None;
    goal_frames(&mut completed, |_, frame| {
        if frame["type"] != "result" {
            return Ok(false);
        }
        assert_eq!(frame["subtype"], "success");
        assert!(frame["num_turns"].as_u64().is_some_and(|turns| turns >= 2));
        property = frame["result"].as_str().map(str::to_owned);
        Ok(true)
    })?;
    assert!(property.is_some_and(|text| text.contains("FINAL_FIXTURE_STEP")));
    completed.control.cancel()?;
    completed.control.wait()?;

    let mut active = launch(SessionIntent::Start)?;
    goal_prompt(
        &mut active,
        "/goal Produce two separate replies listing 40 tree species each, starting replies with GOAL_START and GOAL_END respectively. Use no tools. Stop after the second list.",
    )?;
    let (mut interrupted, mut acknowledged, mut result) = (false, false, false);
    let mut requests = 0;
    let mut session_id = None;
    goal_frames(&mut active, |process, frame| {
        if frame["type"] == "system" && frame["status"] == "requesting" {
            requests += 1;
        }
        if !interrupted && requests >= 2 {
            interrupted = true;
            write_frame(
                process,
                json!({
                    "type":"control_request","request_id":"fixture-stop","request":{"subtype":"interrupt"}
                }),
            )?;
        }
        if frame["response"]["request_id"] == "fixture-stop" {
            assert_eq!(frame["response"]["subtype"], "success");
            acknowledged = true;
        }
        if frame["type"] == "result" {
            assert!(
                interrupted,
                "Claude finished before the fixture could interrupt"
            );
            session_id = frame["session_id"].as_str().map(str::to_owned);
            result = true;
        }
        Ok(acknowledged && result)
    })?;
    goal_prompt(&mut active, "/goal")?;
    goal_frames(&mut active, |_, frame| {
        if frame["type"] != "result" {
            return Ok(false);
        }
        assert!(frame["result"].as_str().is_some_and(|text| {
            text.starts_with("Goal active:") && text.contains("Last check:")
        }));
        Ok(true)
    })?;
    active.control.cancel()?;
    active.control.wait()?;

    let provider_session_id = session_id.context("missing interrupted Claude session ID")?;
    let mut restored = launch(SessionIntent::Resume {
        provider_session_id: provider_session_id.clone(),
    })?;
    goal_prompt(&mut restored, "/goal")?;
    goal_frames(&mut restored, |_, frame| {
        if frame["type"] != "result" {
            return Ok(false);
        }
        assert_eq!(frame["session_id"], provider_session_id);
        assert!(frame["result"].as_str().is_some_and(
            |text| text.starts_with("Goal active:") && text.contains("not yet evaluated")
        ));
        Ok(true)
    })?;
    goal_prompt(&mut restored, "/goal clear")?;
    goal_frames(&mut restored, |_, frame| {
        if frame["type"] != "result" {
            return Ok(false);
        }
        assert!(
            frame["result"]
                .as_str()
                .is_some_and(|text| text.starts_with("Goal cleared:"))
        );
        Ok(true)
    })?;
    restored.control.cancel()?;
    restored.control.wait()?;
    Ok(())
}

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
