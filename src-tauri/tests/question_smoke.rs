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
    process_io::{CapturedProcess, ProcessInput},
    providers::{
        ApprovalDecision, Capability, Provider, ProviderEventData, RequestId, SessionMode,
        TurnOutcome,
        capabilities::{CapabilityStatus, CapabilitySubject, EvidenceOrigin, ObservedResult},
        planning::{PlanSource, PlanningMode},
        profiles::{ConfiguredExecutable, probe_cli},
        questions::{QuestionForm, QuestionInput, QuestionRoute},
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
    reply: impl Fn(&QuestionForm) -> Result<(Value, Value)>,
) -> Result<Exchange> {
    let (_sandbox, store, project, scope) = fixture(provider, model, SessionMode::Plan).await?;
    let checkout_path = scope.checkout.clone();
    let chat = store
        .create_conversation(project.checkout_id.clone(), "Planning qualification".into())
        .await?;
    let runner = Arc::new(RecordingRunner::default());
    let attempt = store
        .prepare_agent_attempt(chat.id.clone(), chat.context_id, scope, prompt.into())
        .await?;
    let run_id = attempt.run.id.clone();
    let worker = start_managed_turn(store.clone(), runner.clone(), attempt, None);
    let mut cursor = 0;
    let mut answered = None;
    let mut output = String::new();
    let mut mode = None;
    let mut proposal = None;
    let mut review = None;
    let mut submitted = None;
    let mut prepared = None;
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
                    ProviderEventData::QuestionReplySettled {
                        request_id,
                        action: omniagent::providers::questions::QuestionAction::Accept,
                        delivery: omniagent::dispatch::Delivery::Submitted,
                    } => submitted = Some(request_id),
                    ProviderEventData::QuestionReplyPrepared {
                        request_id,
                        reply,
                        frame,
                    } => {
                        prepared = Some((request_id, reply, frame));
                    }
                    ProviderEventData::Question {
                        request_id,
                        form,
                        normalized: Some(normalized),
                        ..
                    } => {
                        ensure!(
                            answered.is_none(),
                            "fixture asked an unexpected extra question"
                        );
                        let (reply, expected) = reply(&normalized)?;
                        worker.answer(request_id.clone(), reply).await?;
                        answered = Some((request_id, form, expected));
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
    let (id, raw_form, expected) = answered.with_context(|| {
        format!("{provider:?} did not deliver a native question. Output: {output}")
    })?;
    assert_eq!(submitted, Some(id.clone()));
    let (prepared_id, typed, reply_frame) =
        prepared.context("the question reply was not recorded before delivery")?;
    assert_eq!(prepared_id, id);
    assert_eq!(
        typed.action,
        omniagent::providers::questions::QuestionAction::Accept
    );
    assert!(runner.0.lock().unwrap().contains(&reply_frame));
    let frames = runner
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|frame| serde_json::from_slice::<Value>(frame))
        .collect::<serde_json::Result<Vec<_>>>()?;
    verify_response(provider, &frames, &id, &raw_form, &expected)?;
    if let Some(id) = &review {
        let denial = frames
            .iter()
            .find(|frame| claude_response_matches(frame, id))
            .context("Claude plan review response was not delivered")?;
        assert_eq!(denial["response"]["response"]["behavior"], "deny");
    }
    let status = Command::new("git.exe")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(&checkout_path)
        .output()?;
    ensure!(
        status.status.success() && status.stdout.is_empty(),
        "native planning modified the disposable checkout"
    );
    let result = Exchange {
        mode: mode.context("the native planning mode was not confirmed")?,
        proposal: proposal.with_context(|| {
            format!("{provider:?} turn finished without a plan proposal. Output: {output}")
        })?,
        review_requested: review.is_some(),
    };

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
#[ignore = "requires authenticated Codex with gpt-6-luna"]
async fn codex_plan_delivers_a_native_question_and_authoritative_plan_item() -> Result<()> {
    let answer = exchange(
        Provider::Codex,
        "gpt-6-luna",
        "Create a short plan for naming a file without editing the repository. Before suggesting a plan, use the native request_user_input tool with one question asking which name to use. Offer Cashew, Walnut, and Almond. Do not guess my answer. After I select a name, emit the final plan with the selected name using the proposed_plan block.",
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
        answer.proposal.1.to_ascii_lowercase().contains("walnut"),
        "Codex did not use the selected option"
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
        chat.id.clone(), chat.context_id, scope,
        "You are planning a report and cannot continue until the user decides whether to include a summary. Ask as a required Boolean field named include_summary. Do not assume an answer or edit any files.".into(),
        Vec::new(),
    ).await?;
    let run_id = attempt.run.id.clone();
    let turn_id = attempt.run.turn_id.clone();
    let finished = tokio::time::timeout(
        Duration::from_secs(60),
        start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), attempt, None).wait(),
    )
    .await??;
    let events = store
        .agent_observations(chat.id.clone(), run_id.clone(), 0)
        .await?;
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
        .agent_dispatch(chat.id, run_id)
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
    Ok(())
}
