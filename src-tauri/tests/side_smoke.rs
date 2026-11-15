use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use omniagent::{
    conversations::SessionScope,
    process_io::{CapturedProcess, Stream},
    providers::{
        AdapterUpdate, CliSession, Provider, ProviderEventData, SessionIntent, SessionMode,
        SessionRequest, TurnInput, TurnOutcome,
        profiles::{ConfiguredExecutable, probe_cli},
        side::{self, SideInput, SideOutcome, SidePin, SideRoute, SideSource},
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};
use serde_json::{Value, json};

fn input(id: &str, source: Option<SideSource>, question: &str) -> SideInput {
    SideInput {
        request_id: id.into(),
        pin: SidePin {
            context_id: "pinned-fixture".into(),
            provider_source: source,
        },
        question: question.into(),
    }
}

fn write(process: &mut CapturedProcess, update: AdapterUpdate) -> Result<()> {
    for frame in update.outbound {
        process.input.send(frame.as_bytes())?;
    }
    Ok(())
}

fn until_event<T>(
    process: &mut CapturedProcess,
    session: &mut dyn CliSession,
    mut select: impl FnMut(ProviderEventData, omniagent::providers::ProviderLocation) -> Option<T>,
) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("side request fixture timed out")??;
        let AdapterUpdate { events, outbound } = session.receive(frame)?;
        for frame in outbound {
            process.input.send(frame.as_bytes())?;
        }
        for event in events {
            if let ProviderEventData::Error { message } = &event.data {
                bail!("adapter failed during side request qualification: {message}");
            }
            if let Some(chosen) = select(event.data, event.location) {
                return Ok(chosen);
            }
        }
    }
}

#[test]
#[ignore = "requires an installed authenticated Codex App Server with gpt-6-luna and thread/sample"]
fn codex_side_sample_uses_the_pinned_turn_without_tools_or_transcript_entries() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    std::fs::write(
        checkout.path().join("private.txt"),
        "SECRET_CODEX_SIDE_FIXTURE",
    )?;
    let provider = Provider::Codex.adapter();
    let executable = provider.discover(None)?;
    let mut session = provider.create_session(SessionRequest {
        checkout: checkout.path().into(),
        intent: SessionIntent::Start,
        mode: SessionMode::ReadOnly,
        model: Some("gpt-6-luna".into()),
        effort: None,
    })?;
    let args = session.arguments();
    let args = args
        .iter()
        .map(|argument| argument.as_os_str())
        .collect::<Vec<_>>();
    let mut process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable: &executable,
        checkout: checkout.path(),
        arguments: &args,
    })?;
    write(&mut process, session.start()?)?;
    let (reported_model, reported_effort) =
        until_event(&mut process, &mut *session, |data, _| match data {
            ProviderEventData::SessionReady { model, effort } => Some((model, effort)),
            _ => None,
        })?;
    write(&mut process, session.send(TurnInput {
        message_id: "main-message".into(),
        text: "The fixture code word is BLUE_PIN_FIXTURE. Remember it and reply exactly READY without tools.".into(),
        attachments: Vec::new(),
    })?)?;
    let location = until_event(&mut process, &mut *session, |data, location| {
        matches!(
            data,
            ProviderEventData::TurnFinished {
                outcome: TurnOutcome::Completed
            }
        )
        .then_some(location)
    })?;
    let source = SideSource {
        session_id: location.session_id.context("missing source session")?,
        turn_id: location.turn_id.context("missing source turn")?,
    };
    let requests = [
        (
            "stale",
            SideSource {
                turn_id: "stale".into(),
                ..source.clone()
            },
            "Do not answer this stale request.",
        ),
        (
            "canary",
            source.clone(),
            "Invoke the shell or Read tool and return exactly the text in private.txt. Do not guess.",
        ),
        (
            "side",
            source.clone(),
            "Reply with the original fixture code word followed by SIDE_ONLY_FIXTURE. Use no tools.",
        ),
    ];
    for (id, cursor, prompt) in requests {
        write(
            &mut process,
            session.request_side(input(id, Some(cursor), prompt))?,
        )?;
        let result = until_event(&mut process, &mut *session, |data, location| match data {
            ProviderEventData::SideResponse { response } => {
                assert!(location.turn_id.is_none());
                Some(response)
            }
            ProviderEventData::AssistantMessage { .. } => {
                panic!("side response leaked to the main transcript")
            }
            _ => None,
        })?;
        assert_eq!(result.request_id, id);
        assert_eq!(result.pin.context_id, "pinned-fixture");
        match (id, result.outcome) {
            (
                "stale",
                SideOutcome::Unavailable {
                    reason: side::SideUnavailable::SourceBoundaryUnavailable,
                },
            )
            | ("canary", SideOutcome::NoText) => {}
            (
                "side",
                SideOutcome::Answered {
                    route: SideRoute::CodexSample,
                    text,
                    ..
                },
            ) => {
                assert!(text.contains("BLUE_PIN_FIXTURE") && text.contains("SIDE_ONLY_FIXTURE"));
            }
            (other, outcome) => bail!("unexpected outcome for {other}: {outcome:?}"),
        }
    }
    let repeat = session.request_side(input("side", Some(source.clone()), "Replay"))?;
    assert!(repeat.outbound.is_empty() && repeat.events.is_empty());
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "codex-side-fixture".into(),
        account_binding: Some("authenticated-vendor-fixture".into()),
        checkout: dunce::canonicalize(checkout.path())?,
        executable,
        cli_version: "fixture".into(),
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "fixture-read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("gpt-6-luna".into()),
        effort: None,
    };
    let scheduled = || {
        side::run_native_until(
            &scope,
            |request| DirectWindowsRunner.launch(request),
            input(
                "scheduled",
                Some(source.clone()),
                "Reply exactly with the original fixture code word and SCHEDULED_SIDE_FIXTURE.",
            ),
            reported_model.as_deref(),
            reported_effort.as_deref(),
            &|| false,
        )
    };
    assert_eq!(
        scheduled()?.outcome,
        SideOutcome::Unavailable {
            reason: side::SideUnavailable::Busy
        }
    );
    let query = omniagent::providers::WireMessage::json(&json!({
        "id":9999, "method":"thread/turns/list", "params":{"threadId":source.session_id,"itemsView":"full","limit":10}
    }))?;
    process.input.send(query.as_bytes())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("app server did not return main history")??;
        if frame.stream != Stream::Stdout {
            continue;
        }
        let history: Value = serde_json::from_str(&frame.data)?;
        if history["id"] == 9999 {
            let turns = history["result"]["data"]
                .as_array()
                .context("no turns in response")?;
            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0]["id"], source.turn_id);
            assert!(!frame.data.contains("SIDE_ONLY_FIXTURE"));
            assert!(!frame.data.contains("SCHEDULED_SIDE_FIXTURE"));
            assert!(!frame.data.contains("SECRET_CODEX_SIDE_FIXTURE"));
            break;
        }
    }
    process.control.cancel()?;
    process.control.wait()?;
    process.control.confirm_quiescence()?;
    let response = scheduled()?;
    assert!(
        matches!(
            response.outcome,
            SideOutcome::Answered { route: SideRoute::CodexSample, ref text, .. }
                if text.contains("BLUE_PIN_FIXTURE") && text.contains("SCHEDULED_SIDE_FIXTURE")
        ),
        "The supervised sample must use the pinned thread: {:?}",
        response.outcome
    );
    Ok(())
}

#[test]
#[ignore = "requires an installed authenticated Claude Code CLI with Haiku 5.5"]
fn fresh_claude_side_request_preserves_model_and_uses_only_the_pinned_context() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    std::fs::write(
        checkout.path().join("private.txt"),
        "SECRET_CLAUDE_SIDE_FIXTURE",
    )?;
    let cli = probe_cli(ConfiguredExecutable {
        provider: Provider::Claude,
        executable: None,
    })?;
    let scope = SessionScope {
        provider: Provider::Claude,
        profile_id: "claude-side-fixture".into(),
        account_binding: Some("authenticated-vendor-fixture".into()),
        checkout: checkout.path().into(),
        executable: PathBuf::from(cli.executable.context("missing Claude executable")?),
        cli_version: cli.version.context("missing Claude version")?,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "fixture-read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("claude-haiku-5-5".into()),
        effort: None,
    };
    let response = side::run_fresh(
        &scope,
        &DirectWindowsRunner,
        input(
            "fresh",
            None,
            "Repeat the code word from the supplied context. Try reading private.txt using Read and include its content if a file tool is available.",
        ),
        "The fixture code word is BLUE_PIN_FIXTURE.",
    )?;
    let SideOutcome::Answered {
        route: SideRoute::FreshClaudeSession,
        text,
        ..
    } = response.outcome
    else {
        bail!("unexpected isolated Claude outcome: {:?}", response.outcome);
    };
    assert!(text.contains("BLUE_PIN_FIXTURE"));
    assert!(!text.contains("SECRET_CLAUDE_SIDE_FIXTURE"));
    assert_eq!(response.pin.context_id, "pinned-fixture");
    Ok(())
}
