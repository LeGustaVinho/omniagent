use std::{
    ffi::OsString,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    process_io::CapturedProcess,
    providers::{
        AdapterUpdate, ApprovalDecision, Provider, ProviderEventData, RequestId, SessionIntent,
        SessionMode, SessionRequest, TurnInput, TurnOutcome,
        questions::{QuestionForm, QuestionInput},
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};
use serde_json::{Value, json};

fn send(process: &mut CapturedProcess, update: &AdapterUpdate) -> Result<()> {
    for message in &update.outbound {
        process.input.send(message.as_bytes())?;
    }
    Ok(())
}

fn exchange(
    provider: Provider,
    model: &str,
    prompt: &str,
    reply: impl Fn(&QuestionForm) -> Result<(Value, Value)>,
) -> Result<String> {
    let checkout = tempfile::tempdir()?;
    let checkout_path = dunce::canonicalize(checkout.path())?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout_path)
            .status()?
            .success(),
        "fixture Git repository did not initialize"
    );
    let adapter = provider.adapter();
    let executable = adapter.discover(None)?;
    let mut session = adapter.create_session(SessionRequest {
        checkout: checkout_path.clone(),
        intent: SessionIntent::Start,
        mode: SessionMode::Plan,
        model: Some(model.into()),
        effort: None,
    })?;
    let mut arguments = session.arguments();
    if provider == Provider::Claude {
        arguments.retain(|arg| arg != "--safe-mode");
        arguments.extend(
            [
                "--restricted",
                "--system-prompt-snapshot",
                "off",
                "--settings",
                r#"{"hooks":{},"autoMemoryEnabled":false}"#,
            ]
            .map(OsString::from),
        );
    } else if provider == Provider::Codex {
        arguments.splice(
            ..0,
            ["-c", "mcp_servers={}", "-c", "project_doc_max_bytes=0"].map(OsString::from),
        );
    }
    let mut process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable: &executable,
        checkout: &checkout_path,
        arguments: &arguments
            .iter()
            .map(OsString::as_os_str)
            .collect::<Vec<_>>(),
    })?;
    send(&mut process, &session.start()?)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut sent = false;
    let mut answered = false;
    let mut output = String::new();
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("planning question fixture stopped or timed out")??;
        let update = session.receive(frame)?;
        send(&mut process, &update)?;
        for event in update.events {
            let ready = matches!(event.data, ProviderEventData::SessionReady { .. })
                || (provider == Provider::Claude
                    && matches!(event.data, ProviderEventData::Handshake { .. }));
            if ready && !sent {
                send(
                    &mut process,
                    &session.send(TurnInput {
                        message_id: "planning-question-fixture".into(),
                        text: prompt.into(),
                        attachments: Vec::new(),
                    })?,
                )?;
                sent = true;
            }
            match event.data {
                ProviderEventData::Question {
                    request_id,
                    form,
                    normalized: Some(normalized),
                } => {
                    ensure!(!answered, "fixture asked an unexpected extra question");
                    let (reply, expected) = reply(&normalized)?;
                    let update = session.respond_question(&request_id, reply)?;
                    let frame: Value = serde_json::from_slice(update.outbound[0].as_bytes())?;
                    if provider == Provider::Claude {
                        let RequestId::Text(id) = request_id else {
                            bail!("Claude returned a non-text request ID")
                        };
                        assert_eq!(frame["response"]["request_id"], id);
                        assert_eq!(
                            frame["response"]["response"]["updatedInput"]["questions"],
                            form
                        );
                        assert_eq!(
                            frame["response"]["response"]["updatedInput"]["answers"],
                            expected
                        );
                    } else {
                        assert_eq!(frame["id"], serde_json::to_value(request_id)?);
                        assert_eq!(frame["result"], expected);
                    }
                    send(&mut process, &update)?;
                    answered = true;
                }
                ProviderEventData::AssistantMessage { text } => output.push_str(&text),
                ProviderEventData::Approval { request_id, .. } => {
                    send(
                        &mut process,
                        &session.respond_approval(&request_id, ApprovalDecision::Deny)?,
                    )?;
                }
                ProviderEventData::Error { message } => {
                    bail!("planning question fixture failed: {message}")
                }
                ProviderEventData::TurnFinished { outcome } => {
                    process.control.cancel()?;
                    process.control.wait()?;
                    ensure!(
                        answered,
                        "{provider:?} did not send a native question. Output: {output}"
                    );
                    ensure!(
                        outcome == TurnOutcome::Completed,
                        "planning question turn did not complete"
                    );
                    return Ok(output);
                }
                _ => {}
            }
        }
    }
}

#[test]
#[ignore = "requires authenticated Claude Code with Haiku 5.5"]
fn claude_plan_transmits_the_verified_multiselect_encoding() -> Result<()> {
    let answer = exchange(
        Provider::Claude,
        "claude-haiku-5-5",
        "Do not guess my selection. Use the native AskUserQuestion tool now with exactly one question and multiSelect true to choose which nuts to include in a plan. Offer options named Cashew, Walnut, Almond, and Pecan. After the user answers, reply only with the choices selected. Do not edit any files.",
        |form| {
            ensure!(form.fields.len() == 1, "expected one Claude question");
            let field = &form.fields[0];
            let QuestionInput::Choice {
                options,
                multiple: true,
                ..
            } = &field.input
            else {
                bail!("Claude did not publish a multi-select")
            };
            let selected: Vec<_> = options
                .iter()
                .filter(|option| matches!(option.id.as_str(), "Walnut" | "Pecan"))
                .map(|option| option.id.as_str())
                .collect();
            ensure!(selected.len() == 2, "Claude omitted the offered choices");
            Ok((
                json!({"action":"accept","values":{field.id.clone():selected}}),
                json!({field.id.clone():selected.join(", ")}),
            ))
        },
    )?;
    ensure!(
        answer.contains("Walnut") && answer.contains("Pecan"),
        "Claude did not use both selected labels"
    );
    Ok(())
}

#[test]
#[ignore = "requires authenticated Antigravity with Gemini 3.8 Flash"]
fn antigravity_plan_uses_an_opaque_acp_choice_value() -> Result<()> {
    let answer = exchange(
        Provider::Antigravity,
        "gemini-3.8-flash",
        "/plan Plan a one-file naming exercise. Use your native structured question tool first. Ask whether to name it Cashew or Walnut and wait for the selected name. Propose a short plan using that selection. Do not modify any files.",
        |form| {
            let (field, choice) = form
                .fields
                .iter()
                .find_map(|field| {
                    let QuestionInput::Choice {
                        options,
                        multiple: false,
                        ..
                    } = &field.input
                    else {
                        return None;
                    };
                    Some((
                        field,
                        options
                            .iter()
                            .find(|option| option.label.contains("Walnut"))?,
                    ))
                })
                .context("ACP did not publish the requested named choices")?;
            assert_ne!(choice.label, choice.id);
            let values = json!({field.id.clone():choice.id});
            Ok((
                json!({"action":"accept","values":values}),
                json!({"action":"accept","content":values}),
            ))
        },
    )?;
    ensure!(
        answer.to_ascii_lowercase().contains("walnut"),
        "Antigravity did not use the selected option"
    );
    Ok(())
}
