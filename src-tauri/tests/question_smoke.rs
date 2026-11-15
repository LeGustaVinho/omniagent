use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    conversations::SessionScope,
    dispatch::RecordedAgentEvent,
    process_io::{CapturedProcess, ProcessInput},
    providers::{
        ApprovalDecision, Capability, Provider, ProviderEventData, RequestId, SessionMode,
        TurnOutcome,
        capabilities::{CapabilityStatus, CapabilitySubject, EvidenceOrigin, ObservedResult},
        planning::{PlanSource, PlanningMode},
        profiles::{ConfiguredExecutable, probe_cli},
        questions::{
            QuestionAction, QuestionForm, QuestionInput, QuestionOrigin, QuestionReply,
            QuestionRoute,
        },
        runtime::start_managed_turn,
    },
    repository::{self, ProjectCheckout},
    runner::{DirectWindowsRunner, ExecutionPlatform, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingRunner(Arc<Mutex<Vec<Vec<u8>>>>);

impl Runner for RecordingRunner {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }
    fn execution_platform(&self) -> Option<ExecutionPlatform> {
        DirectWindowsRunner.execution_platform()
    }
    fn executable_revision(&self, path: &Path) -> Option<String> {
        DirectWindowsRunner.executable_revision(path)
    }
    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        let CapturedProcess {
            input,
            output,
            control,
        } = DirectWindowsRunner.launch(request)?;
        Ok(CapturedProcess {
            input: ProcessInput::new(RecordedPipe {
                input,
                frames: self.0.clone(),
            }),
            output,
            control,
        })
    }
}

struct RecordedPipe {
    input: ProcessInput,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl io::Write for RecordedPipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.input.send(bytes)?;
        self.frames.lock().unwrap().push(bytes.to_vec());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Exchange {
    mode: PlanningMode,
    proposal: (PlanSource, String),
    review_requested: bool,
}

async fn all_observations(
    store: &Store,
    conversation: &str,
    run: &str,
) -> Result<Vec<RecordedAgentEvent>> {
    let mut all = Vec::new();
    let mut cursor = 0;
    loop {
        let page = store
            .agent_observations(conversation.into(), run.into(), cursor)
            .await?;
        let Some(last) = page.last() else {
            return Ok(all);
        };
        cursor = last.seq;
        all.extend(page);
    }
}

async fn fixture(
    provider: Provider,
    model: &str,
    mode: SessionMode,
) -> Result<(tempfile::TempDir, Store, ProjectCheckout, SessionScope)> {
    let sandbox = tempfile::tempdir()?;
    let checkout_path = sandbox.path().join("checkout");
    fs::create_dir(&checkout_path)?;
    let checkout_path = dunce::canonicalize(checkout_path)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout_path)
            .status()?
            .success(),
        "fixture Git repository did not initialize"
    );
    let store = Store::open(sandbox.path().join("fixture.sqlite"))?;
    let project = repository::register(&store, checkout_path.clone()).await?;
    let probe = probe_cli(ConfiguredExecutable {
        provider,
        executable: None,
    })?;
    let scope = SessionScope {
        provider,
        profile_id: "planning-qualification".into(),
        account_binding: Some("authenticated-vendor-fixture".into()),
        checkout: checkout_path,
        executable: PathBuf::from(probe.executable.context("missing CLI executable")?),
        cli_version: probe.version.context("missing CLI version")?,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "read-only-planning-fixture".into(),
        mode,
        model: Some(model.into()),
        effort: None,
    };
    Ok((sandbox, store, project, scope))
}

async fn exchange(
    provider: Provider,
    model: &str,
    prompt: &str,
    expected_rounds: usize,
    reply: impl Fn(usize, &QuestionForm) -> Result<(Value, Value)>,
) -> Result<Exchange> {
    let (_sandbox, store, project, scope) = fixture(provider, model, SessionMode::Plan).await?;
    let checkout_path = scope.checkout.clone();
    let source = "fixture_config.py";
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let source_code = format!("RELEASE_MARKER = \"{marker}\"\n");
    fs::write(checkout_path.join(source), &source_code)?;
    let git = |args: &[&str]| -> Result<Vec<u8>> {
        let output = Command::new("git.exe")
            .args(args)
            .current_dir(&checkout_path)
            .output()?;
        ensure!(
            output.status.success(),
            "fixture git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.stdout)
    };
    git(&["add", "--", source])?;
    git(&[
        "-c",
        "user.name=OmniAgent Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "core.hooksPath=.git/no-hooks",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "-m",
        "Add the planning fixture",
    ])?;
    let head = git(&["rev-parse", "HEAD"])?;
    let prompt = format!(
        "{prompt}\nAfter answering the question rounds and before proposing the plan, read {source} and include the exact RELEASE_MARKER value from the file in the plan. Keep the checkout unchanged."
    );
    let chat = store
        .create_conversation(project.checkout_id.clone(), "Planning qualification".into())
        .await?;
    let runner = Arc::new(RecordingRunner::default());
    let attempt = store
        .prepare_agent_attempt(chat.id.clone(), chat.context_id, scope, prompt)
        .await?;
    let run_id = attempt.run.id.clone();
    let worker = start_managed_turn(store.clone(), runner.clone(), attempt, None);
    let mut cursor = 0;
    let mut answered = Vec::<(RequestId, Value, Value, Value)>::new();
    let mut output = String::new();
    let mut mode = None;
    let mut proposal = None;
    let mut read_source = false;
    let mut review = None;
    let mut submitted = Vec::new();
    let mut prepared = Vec::new();
    tokio::time::timeout(Duration::from_secs(75), async {
        loop {
            let mut outcome = None;
            for entry in store
                .agent_observations(chat.id.clone(), run_id.clone(), cursor)
                .await?
            {
                cursor = entry.seq;
                match entry.event.data {
                    ProviderEventData::PlanningModeConfirmed { mode: observed } => {
                        mode = Some(observed)
                    }
                    ProviderEventData::PlanReviewRequested { request_id } => {
                        review = Some(request_id)
                    }
                    ProviderEventData::PlanProposed { text, source } => {
                        proposal = Some((source, text))
                    }
                    ProviderEventData::ContextRead { path, .. } => {
                        read_source |= Path::new(&path).file_name() == Some(source.as_ref());
                    }
                    ProviderEventData::QuestionReplySettled {
                        request_id,
                        action: omniagent::providers::questions::QuestionAction::Accept,
                        delivery: omniagent::dispatch::Delivery::Submitted,
                    } => submitted.push(request_id),
                    ProviderEventData::QuestionReplyPrepared {
                        request_id,
                        reply,
                        frame,
                    } => {
                        prepared.push((request_id, reply, frame));
                    }
                    ProviderEventData::Question {
                        request_id,
                        form,
                        normalized: Some(normalized),
                        ..
                    } => {
                        ensure!(
                            answered.len() < expected_rounds,
                            "fixture asked an unexpected extra question"
                        );
                        if let Some((prior, stale_reply, _, _)) = answered.last() {
                            assert_ne!(prior, &request_id);
                            assert!(
                                worker
                                    .answer(prior.clone(), stale_reply.clone())
                                    .await
                                    .is_err()
                            );
                        }
                        let (reply, expected) = reply(answered.len(), &normalized)?;
                        worker.answer(request_id.clone(), reply.clone()).await?;
                        answered.push((request_id, reply, form, expected));
                    }
                    ProviderEventData::AssistantMessage { text } => output.push_str(&text),
                    ProviderEventData::Approval { request_id, .. } => {
                        worker.approve(request_id, ApprovalDecision::Deny).await?
                    }
                    ProviderEventData::Error { message } => {
                        bail!("planning question fixture failed: {message}")
                    }
                    ProviderEventData::TurnFinished { outcome: observed } => {
                        outcome = Some(observed)
                    }
                    _ => {}
                }
            }
            if let Some(outcome) = outcome {
                ensure!(
                    outcome == TurnOutcome::Completed,
                    "planning question turn did not complete"
                );
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .context("planning question fixture timed out")??;
    assert_eq!(worker.wait().await?, RunStatus::Succeeded);
    ensure!(
        answered.len() == expected_rounds,
        "{provider:?} delivered {} of {expected_rounds} native question rounds. Output: {output}",
        answered.len()
    );
    assert_eq!(submitted.len(), expected_rounds);
    assert_eq!(prepared.len(), expected_rounds);
    let frames = runner
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|frame| serde_json::from_slice::<Value>(frame))
        .collect::<serde_json::Result<Vec<_>>>()?;
    for (((id, _, raw_form, expected), submitted), (prepared_id, typed, reply_frame)) in
        answered.into_iter().zip(submitted).zip(prepared)
    {
        assert_eq!(submitted, id);
        assert_eq!(prepared_id, id);
        assert_eq!(typed.action, QuestionAction::Accept);
        assert!(runner.0.lock().unwrap().contains(&reply_frame));
        verify_response(provider, &frames, &id, &raw_form, &expected)?;
    }
    if let Some(id) = &review {
        let denial = frames
            .iter()
            .find(|frame| claude_response_matches(frame, id))
            .context("Claude plan review response was not delivered")?;
        assert_eq!(denial["response"]["response"]["behavior"], "deny");
    }
    ensure!(
        git(&["status", "--porcelain", "--untracked-files=all"])?.is_empty()
            && git(&["rev-parse", "HEAD"])? == head
            && fs::read_to_string(checkout_path.join(source))? == source_code,
        "{provider:?} planning modified the disposable checkout"
    );
    let result = Exchange {
        mode: mode.context("the native planning mode was not confirmed")?,
        proposal: proposal.with_context(|| {
            format!("{provider:?} turn finished without a plan proposal. Output: {output}")
        })?,
        review_requested: review.is_some(),
    };
    ensure!(
        read_source && result.proposal.1.contains(&marker),
        "{provider:?} did not report a read of the source file and use its marker in the plan"
    );

    let evidence = store
        .agent_dispatch(chat.id, run_id)
        .await?
        .context("missing actual planning dispatch evidence")?;
    let assessment = evidence
        .snapshot
        .capabilities
        .context("missing planning capability assessment")?;
    let mut descriptors = Vec::new();
    for capability in [
        Capability::NativePlanMode,
        Capability::PlanningQuestions,
        Capability::PlanningAnswers,
        Capability::NativePlanOutput,
    ] {
        let descriptor = assessment
            .entries
            .iter()
            .find(|entry| entry.descriptor.subject == CapabilitySubject::Protocol { capability })
            .context("missing planning capability descriptor")?
            .descriptor
            .clone();
        store
            .record_capability_evidence(
                assessment.scope.clone(),
                descriptor.clone(),
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                evidence.id.clone(),
            )
            .await?;
        descriptors.push(descriptor);
    }
    let verified = store
        .resolve_capability_assessment(assessment.scope, descriptors)
        .await?;
    assert_eq!(verified.entries.len(), 4);
    assert!(
        verified
            .entries
            .iter()
            .all(|entry| entry.status() == CapabilityStatus::Verified)
    );
    Ok(result)
}

fn claude_response_matches(frame: &Value, id: &RequestId) -> bool {
    matches!(id, RequestId::Text(id) if frame["type"] == "control_response" && frame["response"]["request_id"] == *id)
}

fn verify_response(
    provider: Provider,
    frames: &[Value],
    id: &RequestId,
    form: &Value,
    expected: &Value,
) -> Result<()> {
    if provider == Provider::Claude {
        let frame = frames
            .iter()
            .find(|frame| claude_response_matches(frame, id))
            .context("missing Claude answer frame")?;
        assert_eq!(
            &frame["response"]["response"]["updatedInput"]["questions"],
            form
        );
        assert_eq!(
            &frame["response"]["response"]["updatedInput"]["answers"],
            expected
        );
    } else {
        let id = serde_json::to_value(id)?;
        let frame = frames
            .iter()
            .find(|frame| frame["id"] == id && frame.get("result").is_some())
            .context("missing JSON-RPC question response")?;
        assert_eq!(&frame["result"], expected);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code with Haiku 5.5"]
async fn claude_plan_transmits_the_verified_multiselect_encoding() -> Result<()> {
    let answer = exchange(
        Provider::Claude,
        "claude-haiku-5-5",
        "Do not guess my selection. Use the native AskUserQuestion tool now with exactly one question and multiSelect true to choose which nuts to include in a plan. Offer options named Cashew, Walnut, Almond, and Pecan. After I answer, call ExitPlanMode to offer the plan for review. Put the complete proposed plan in the reply, using the selected names. Do not write any files.",
        1, |_, form| {
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
    ).await?;
    assert_eq!(answer.mode, PlanningMode::ClaudePermissionMode);
    assert!(answer.review_requested);
    assert_eq!(answer.proposal.0, PlanSource::ClaudeExitPlanMode);
    ensure!(
        answer.proposal.1.contains("Walnut") && answer.proposal.1.contains("Pecan"),
        "Claude did not use both selected labels"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Antigravity with Gemini 3.8 Flash"]
async fn antigravity_plan_uses_an_opaque_acp_choice_value() -> Result<()> {
    let answer = exchange(
        Provider::Antigravity,
        "gemini-3.8-flash",
        "/plan Plan a one-file naming exercise. Use your native structured question tool first. Ask whether to name it Cashew or Walnut and wait for the selected name. Propose a short plan using that selection. Do not modify any files.",
        1, |_, form| {
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
    ).await?;
    assert_eq!(answer.mode, PlanningMode::AcpSessionMode);
    assert_eq!(answer.proposal.0, PlanSource::AntigravityPlanCommand);
    ensure!(
        answer.proposal.1.to_ascii_lowercase().contains("walnut"),
        "Antigravity did not use the selected option"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Antigravity with Gemini 3.8 Flash"]
async fn cancelling_acp_planning_closes_an_unanswered_native_request() -> Result<()> {
    let (_sandbox, store, project, scope) =
        fixture(Provider::Antigravity, "gemini-3.8-flash", SessionMode::Plan).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Cancel planning".into())
        .await?;
    let attempt = store.prepare_agent_attempt(
        chat.id.clone(), chat.context_id, scope,
        "/plan Ask a native structured question first to choose whether to write a report about Cashew or Walnut. Wait for the answer. Do not modify files.".into(),
    ).await?;
    let run_id = attempt.run.id.clone();
    let runner = Arc::new(RecordingRunner::default());
    let worker = start_managed_turn(store.clone(), runner.clone(), attempt, None);
    let question_id = tokio::time::timeout(Duration::from_secs(45), async {
        let mut cursor = 0;
        loop {
            for entry in store
                .agent_observations(chat.id.clone(), run_id.clone(), cursor)
                .await?
            {
                cursor = entry.seq;
                match entry.event.data {
                    ProviderEventData::Question {
                        request_id,
                        route: QuestionRoute::Native,
                        ..
                    } => return Ok::<_, anyhow::Error>(request_id),
                    ProviderEventData::Error { message } => bail!("ACP planning failed: {message}"),
                    ProviderEventData::TurnFinished { .. } => {
                        bail!("ACP turn finished without a pending question")
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .context("ACP question did not arrive")??;
    worker.interrupt()?;
    let stale = tokio::time::timeout(
        Duration::from_secs(15),
        worker.answer(question_id.clone(), json!({"action":"decline"})),
    )
    .await?;
    assert!(stale.is_err());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(15), worker.wait()).await??,
        RunStatus::Cancelled
    );
    let id = serde_json::to_value(question_id)?;
    let frames = runner
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|frame| serde_json::from_slice::<Value>(frame))
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert!(
        frames
            .iter()
            .any(|frame| frame["method"] == "session/cancel")
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame["id"] == id && frame["result"]["action"] == "cancel")
    );
    let events = store.agent_observations(chat.id, run_id, 0).await?;
    assert!(!events.iter().any(|entry| matches!(
        entry.event.data,
        ProviderEventData::QuestionReplyPrepared { .. }
    )));
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Codex with gpt-6-luna"]
async fn codex_plan_delivers_a_native_question_and_authoritative_plan_item() -> Result<()> {
    let answer = exchange(
        Provider::Codex,
        "gpt-6-luna",
        "Create a short plan for naming a file without editing the repository. Use the native request_user_input tool twice, with exactly one question per call. First ask which base name to use and offer Cashew, Walnut, and Almond. Wait for this answer before making the second call. Then ask for the file format and offer Markdown, Plain text, and JSON. Only after both answers, emit the plan with the resulting filename in a proposed_plan block. Do not guess or combine the questions.",
        2, |round, form| {
            let label = ["Walnut", "Markdown"][round];
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
                            .find(|option| option.label.contains(label))?,
                    ))
                })
                .context("Codex did not publish the requested named choices")?;
            Ok((
                json!({"action":"accept","values":{field.id.clone():choice.id}}),
                json!({"answers":{field.id.clone():{"answers":[choice.id]}}}),
            ))
        },
    ).await?;
    assert_eq!(answer.mode, PlanningMode::CodexCollaboration);
    assert_eq!(answer.proposal.0, PlanSource::CodexPlanItem);
    ensure!(
        answer.proposal.1.to_ascii_lowercase().contains("walnut.md"),
        "Codex did not combine the two selected options"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code with Haiku 5.5"]
async fn read_only_turn_records_a_validated_structured_question_after_completion() -> Result<()> {
    let (_sandbox, store, project, scope) =
        fixture(Provider::Claude, "claude-haiku-5-5", SessionMode::ReadOnly).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Structured planning fallback".into())
        .await?;
    let attempt = store.prepare_structured_planning_attempt(
        chat.id.clone(), chat.context_id, scope.clone(),
        "You are planning a report and cannot continue until the user decides whether to include a summary. Ask as a required Boolean field named include_summary. When answered, return the complete plan with the exact token SUMMARY_INCLUDED for true or SUMMARY_OMITTED for false. Do not assume an answer or edit any files.".into(),
        Vec::new(),
    ).await?;
    let run_id = attempt.run.id.clone();
    let turn_id = attempt.run.turn_id.clone();
    let finished = tokio::time::timeout(
        Duration::from_secs(60),
        start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), attempt, None).wait(),
    )
    .await??;
    let events = all_observations(&store, &chat.id, &run_id).await?;
    ensure!(
        finished == RunStatus::Succeeded,
        "structured planner failed: {:?}",
        events
            .iter()
            .map(|entry| &entry.event.data)
            .collect::<Vec<_>>()
    );
    let terminal = events
        .iter()
        .find(|event| {
            matches!(
                event.event.data,
                ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed
                }
            )
        })
        .context("missing provider turn completion")?;
    let question = events
        .iter()
        .find_map(|entry| match &entry.event.data {
            ProviderEventData::Question {
                request_id,
                normalized: Some(form),
                route,
                ..
            } => Some((entry, request_id, form, route)),
            _ => None,
        })
        .context("missing structured question")?;
    assert!(question.0.seq > terminal.seq);
    assert_eq!(
        question.1,
        &RequestId::Text(format!("structured:{turn_id}"))
    );
    assert_eq!(*question.3, QuestionRoute::StructuredTurn);
    assert!(
        question
            .2
            .fields
            .iter()
            .any(|field| field.id == "include_summary"
                && field.required
                && field.input == QuestionInput::Boolean)
    );
    let saved = store
        .agent_dispatch(chat.id.clone(), run_id.clone())
        .await?
        .context("missing structured planner dispatch")?;
    assert_eq!(saved.snapshot.question_route, QuestionRoute::StructuredTurn);
    let wire: Value = serde_json::from_slice(&saved.frames[0])?;
    assert!(
        wire["message"]["content"]
            .as_str()
            .unwrap()
            .contains("Read-only planning response format.")
    );
    let origin = QuestionOrigin {
        run_id: run_id.clone(),
        request_id: question.1.clone(),
    };
    let active = store.conversation(chat.id.clone()).await?;
    store.recover_agent_attempts_after_restart().await?;
    let reply = |value| QuestionReply {
        action: QuestionAction::Accept,
        values: [("include_summary".into(), value)].into_iter().collect(),
    };
    assert!(
        store
            .prepare_structured_planning_reply(
                chat.id.clone(),
                active.context_id.clone(),
                scope.clone(),
                origin.clone(),
                reply(json!("true")),
            )
            .await
            .is_err()
    );
    let mut other_profile = scope.clone();
    other_profile.profile_id = "different-planning-profile".into();
    assert!(
        store
            .prepare_structured_planning_reply(
                chat.id.clone(),
                active.context_id.clone(),
                other_profile,
                origin.clone(),
                reply(json!(true)),
            )
            .await
            .is_err()
    );
    let continuation = store
        .prepare_structured_planning_reply(
            chat.id.clone(),
            active.context_id,
            scope.clone(),
            origin.clone(),
            reply(json!(true)),
        )
        .await?;
    let linked_run = continuation.run.id.clone();
    assert_eq!(
        continuation.previous_run_id.as_deref(),
        Some(run_id.as_str())
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(60),
            start_managed_turn(
                store.clone(),
                Arc::new(DirectWindowsRunner),
                continuation,
                None
            )
            .wait(),
        )
        .await??,
        RunStatus::Succeeded
    );
    let saved = store
        .agent_dispatch(chat.id.clone(), linked_run.clone())
        .await?
        .unwrap();
    assert_eq!(saved.snapshot.answered_question.as_ref(), Some(&origin));
    assert!(
        saved
            .snapshot
            .submitted_messages
            .last()
            .unwrap()
            .text
            .contains(r#""include_summary":true"#)
    );
    let linked_events = all_observations(&store, &chat.id, &linked_run).await?;
    ensure!(linked_events.iter().any(|entry| matches!(
        &entry.event.data, ProviderEventData::PlanProposed { source: PlanSource::StructuredTurn, text }
            if text.contains("SUMMARY_INCLUDED")
    )), "linked planner did not use the accepted answer: {:?}", linked_events.iter().filter_map(|entry| match &entry.event.data {
        data @ (ProviderEventData::PlanProposed { .. } | ProviderEventData::Question { .. } | ProviderEventData::AssistantMessage { .. }) => Some(data),
        _ => None,
    }).collect::<Vec<_>>());
    let source_events = all_observations(&store, &chat.id, &run_id).await?;
    assert!(source_events.iter().any(|entry| matches!(
        &entry.event.data, ProviderEventData::StructuredQuestionReplyLinked { origin: saved, continuation_run_id, reply: answer }
            if saved == &origin && continuation_run_id == &linked_run && answer == &reply(json!(true))
    )));
    let active = store.conversation(chat.id.clone()).await?;
    assert!(
        store
            .prepare_structured_planning_reply(
                chat.id,
                active.context_id,
                scope,
                origin,
                reply(json!(true)),
            )
            .await
            .is_err()
    );
    Ok(())
}
