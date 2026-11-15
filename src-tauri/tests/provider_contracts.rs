use std::{
    ffi::OsStr,
    fs,
    io::{self, BufRead, Write},
    path::Path,
    process::Command,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use omniagent::{
    conversations::{Conversation, SessionScope},
    dispatch::{Delivery, EventSource, NativeHistory},
    jobs::{AdmissionBlocker, JobContext, JobScheduler, QueueSettings, RecoveryAction},
    process::TerminalOutcome,
    process_io::{CapturedProcess, OutputFrame, ProcessInput, Stream},
    providers::{
        AdapterUpdate, ApprovalDecision, Capability, CliSession, Provider, ProviderEvent,
        ProviderEventData as Event, RequestId, SessionIntent, SessionMode, SessionRequest,
        TurnInput, TurnOutcome,
        capabilities::{
            Availability, CapabilityDescriptor, CapabilityStatus, CapabilitySubject,
            DeclaredSource, EvidenceBoundary, EvidenceOrigin, ObservedResult, ResolvedCapability,
            Verification,
        },
        runtime::start_managed_turn,
    },
    repository,
    runner::{DirectWindowsRunner, LaunchRequest, RunControl, Runner},
    runs::{RunRecord, RunStatus},
    store::Store,
};
use serde_json::{Value, json};

const APPROVAL: &str = "fixture-approval";
const SESSION: &str = "fixture-session";
const ANSWER: &str = "Answer ☕";
const PROMPT: &str = "Read the fixture ☕\r\nDo not modify files. Exact wire canary.";
const WORKER_MARKER: &str = "omniagent-wire-worker";

fn is_prompt(provider: Provider, frame: &Value) -> bool {
    match provider {
        Provider::Codex => frame["method"] == "turn/start",
        Provider::Claude => frame["type"] == "user",
        Provider::Antigravity => frame["method"] == "session/prompt",
    }
}

fn verify_denial(provider: Provider, reply: &Value) {
    match provider {
        Provider::Codex => {
            assert_eq!(reply["id"], APPROVAL);
            assert_eq!(reply["result"]["decision"], "decline");
        }
        Provider::Claude => {
            assert_eq!(reply["response"]["request_id"], APPROVAL);
            assert_eq!(reply["response"]["response"]["behavior"], "deny");
        }
        Provider::Antigravity => {
            assert_eq!(reply["id"], APPROVAL);
            assert_eq!(reply["result"]["outcome"]["optionId"], "reject");
        }
    }
}

// The test binary doubles as the child CLI. The parent creates the marker in a
// disposable checkout and launches only this ignored test, with real stdio pipes.
#[test]
#[ignore = "child entry point invoked by the managed wire test"]
fn protocol_fixture_worker() -> Result<()> {
    let checkout = std::env::current_dir()?;
    let provider: Provider = serde_json::from_slice(&fs::read(checkout.join(WORKER_MARKER))?)?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut bytes = Vec::new();
    let mut turn_id = "wire-turn";
    let prompt = loop {
        assert!(stdin.read_until(b'\n', &mut bytes)? > 0);
        let frame: Value = serde_json::from_slice(&bytes)?;
        if is_prompt(provider, &frame) {
            fs::write(checkout.join("received-prompt.ndjson"), &bytes)?;
            break frame;
        }
        if frame["method"] == "thread/resume" {
            fs::write(checkout.join("received-resume.ndjson"), &bytes)?;
            turn_id = "recovered-turn";
        }
        if let Some(reply) = handshake(provider, &frame, &checkout)? {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
        bytes.clear();
    };

    writeln!(stdout, "{{malformed-fixture")?;
    for line in transcript(provider, &prompt, turn_id) {
        if let Some(index) = line.find('☕') {
            // Deliberately split the UTF-8 codepoint and the enclosing frame.
            let split = index + 1;
            stdout.write_all(&line.as_bytes()[..split])?;
            stdout.flush()?;
            thread::sleep(Duration::from_millis(30));
            stdout.write_all(&line.as_bytes()[split..])?;
            stdout.write_all(b"\n")?;
        } else {
            writeln!(stdout, "{line}")?;
        }
        stdout.flush()?;
        if line.contains(APPROVAL) {
            bytes.clear();
            assert!(stdin.read_until(b'\n', &mut bytes)? > 0);
            let reply: Value = serde_json::from_slice(&bytes)?;
            if provider == Provider::Codex && reply["method"] == "turn/interrupt" {
                assert_eq!(reply["params"]["turnId"], turn_id);
                fs::write(checkout.join("received-interrupt.ndjson"), &bytes)?;
                let deadline = std::time::Instant::now() + Duration::from_secs(4);
                while !checkout.join("settle-interrupt").exists() {
                    anyhow::ensure!(
                        std::time::Instant::now() < deadline,
                        "timed out waiting to settle interrupt"
                    );
                    thread::sleep(Duration::from_millis(25));
                }
                let terminal = json!({"method":"turn/completed", "params":{
                    "threadId":SESSION,"turn":{"id":turn_id,"status":"interrupted"}
                }});
                writeln!(stdout, "{terminal}")?;
                stdout.flush()?;
                break;
            }
            verify_denial(provider, &reply);
            fs::write(checkout.join("received-decision.ndjson"), &bytes)?;
        }
    }
    // Let the supervisor reap the child after it receives the protocol terminal.
    let mut trailing = String::new();
    stdin.read_line(&mut trailing)?;
    Ok(())
}

struct FixtureRunner(Provider);

struct FinishedProbe;

impl RunControl for FinishedProbe {
    fn cancel(&self) -> io::Result<bool> {
        Ok(false)
    }

    fn wait(&self) -> io::Result<TerminalOutcome> {
        Ok(TerminalOutcome::Exited(0))
    }
}

impl Runner for FixtureRunner {
    fn identity(&self) -> &str {
        "windows-provider-wire-fixture"
    }

    fn executable_revision(&self, path: &Path) -> Option<String> {
        DirectWindowsRunner.executable_revision(path)
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.arguments == ["plugin", "list", "--json"].map(OsStr::new) {
            let (sender, output) = mpsc::channel();
            let data = if self.0 == Provider::Codex {
                r#"{"installed":[]}"#
            } else {
                "[]"
            };
            sender
                .send(Ok(OutputFrame {
                    stream: Stream::Stdout,
                    data: data.into(),
                }))
                .unwrap();
            return Ok(CapturedProcess {
                input: ProcessInput::new(io::sink()),
                output,
                control: Arc::new(FinishedProbe),
            });
        }
        let expected_arg = match self.0 {
            Provider::Codex => "app-server",
            Provider::Claude => "-p",
            Provider::Antigravity => "acp",
        };
        assert!(request.arguments.contains(&OsStr::new(expected_arg)));
        DirectWindowsRunner.launch(LaunchRequest {
            arguments: &[
                "--quiet",
                "--ignored",
                "--exact",
                "protocol_fixture_worker",
                "--nocapture",
            ]
            .map(OsStr::new),
            ..request
        })
    }
}

async fn open_wire_fixture(
    provider: Provider,
    sandbox: &Path,
) -> Result<(Store, Conversation, Arc<FixtureRunner>, SessionScope)> {
    let checkout = dunce::canonicalize(sandbox)?;
    assert!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    fs::write(checkout.join(WORKER_MARKER), serde_json::to_vec(&provider)?)?;
    let store = Store::open(checkout.join("fixture.sqlite"))?;
    let project = repository::register(&store, checkout.clone()).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Protected wire fixture".into())
        .await?;
    let runner = Arc::new(FixtureRunner(provider));
    let scope = SessionScope {
        provider,
        profile_id: "wire-fixture".into(),
        account_binding: None,
        checkout: checkout.clone(),
        executable: std::env::current_exe()?,
        cli_version: match provider {
            Provider::Codex => "0.169.0-alpha.11.1",
            Provider::Claude => "2.1.296",
            Provider::Antigravity => "1.5.9",
        }
        .into(),
        runner: runner.identity().into(),
        permission_revision: "fixture-read-only".into(),
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    };
    Ok((store, chat, runner, scope))
}

async fn verify_managed_wire(provider: Provider) -> Result<()> {
    let sandbox = tempfile::tempdir()?;
    let (store, chat, runner, scope) = open_wire_fixture(provider, sandbox.path()).await?;
    let checkout = scope.checkout.clone();
    let database = checkout.join("fixture.sqlite");
    let attempt = store
        .prepare_agent_attempt(chat.id.clone(), chat.context_id, scope, PROMPT.into())
        .await?;
    let run = attempt.run.id.clone();
    let managed = start_managed_turn(store.clone(), runner, attempt, None);
    let mut cursor = 0;
    let mut events = Vec::new();
    let collected = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            for entry in store
                .agent_observations(chat.id.clone(), run.clone(), cursor)
                .await?
            {
                cursor = entry.seq;
                if let Event::Approval { request_id, .. } = &entry.event.data {
                    assert!(entry.source == EventSource::Provider);
                    managed
                        .approve(request_id.clone(), ApprovalDecision::Deny)
                        .await?;
                }
                let finished = matches!(entry.event.data, Event::TurnFinished { .. });
                events.push(entry.event);
                if finished {
                    return Ok::<_, anyhow::Error>(());
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if !matches!(collected, Ok(Ok(()))) {
        let _ = managed.interrupt();
        let outcome = managed.wait().await;
        bail!("{provider:?} wire fixture did not finish: {collected:?}, {outcome:?}\n{events:#?}");
    }
    assert_eq!(managed.wait().await?, RunStatus::Succeeded);
    verify_stream(provider, &events);
    let submitted = events
        .iter()
        .position(|event| matches!(event.data, Event::DispatchSubmitted { .. }))
        .context("dispatch was not submitted")?;
    let malformed = match provider {
        Provider::Codex => "Malformed Codex protocol frame",
        Provider::Claude => "Malformed Claude protocol frame",
        Provider::Antigravity => "Malformed ACP protocol frame",
    };
    assert!(
        events[submitted + 1..].iter().any(
            |event| matches!(&event.data, Event::Diagnostic { message } if message == malformed)
        )
    );

    let dispatch = store
        .agent_dispatch(chat.id, run)
        .await?
        .context("protected dispatch missing")?;
    assert_eq!(dispatch.delivery, Delivery::Submitted);
    let received = fs::read(checkout.join("received-prompt.ndjson"))?;
    assert_eq!(dispatch.frames, [received.clone()]);
    let prompt: Value = serde_json::from_slice(&received)?;
    let text = match provider {
        Provider::Codex => &prompt["params"]["input"][0]["text"],
        Provider::Claude => &prompt["message"]["content"],
        Provider::Antigravity => &prompt["params"]["prompt"][0]["text"],
    };
    assert_eq!(text, PROMPT);
    let reply = fs::read(checkout.join("received-decision.ndjson"))?;
    verify_denial(provider, &serde_json::from_slice(&reply)?);
    let connection = rusqlite::Connection::open(database)?;
    let protected: Vec<u8> = connection.query_row(
        "SELECT frames FROM dispatches WHERE id = ?",
        [&dispatch.id],
        |row| row.get(0),
    )?;
    assert!(
        !protected
            .windows(b"Exact wire canary".len())
            .any(|window| window == b"Exact wire canary")
    );
    Ok(())
}

#[tokio::test]
async fn all_managed_adapters_deliver_protected_dispatch_over_native_pipes() -> Result<()> {
    for provider in Provider::ALL {
        verify_managed_wire(provider)
            .await
            .with_context(|| format!("{provider:?} managed wire fixture"))?;
    }
    Ok(())
}

async fn fixture_approval(store: &Store, conversation: &str) -> Result<RunRecord> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(run) = store.latest_conversation_run(conversation.into()).await?
                && !store
                    .pending_conversation_inputs(conversation.into(), run.id.clone())
                    .await?
                    .is_empty()
            {
                return Ok(run);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("scheduler did not reach the provider approval")?
}

#[tokio::test]
async fn detached_scheduler_preserves_a_quiet_approval_until_the_provider_confirms_stop()
-> Result<()> {
    let sandbox = tempfile::tempdir()?;
    let (store, chat, runner, mut scope) =
        open_wire_fixture(Provider::Codex, sandbox.path()).await?;
    scope.account_binding = Some("fixture-account".into());
    let checkout = scope.checkout.clone();
    store
        .set_job_queue_settings(QueueSettings {
            paused: true,
            capacity: 1,
        })
        .await?;
    store
        .save_conversation_draft(chat.id.clone(), PROMPT.into())
        .await?;
    let job = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: chat.id.clone(),
                context_id: chat.context_id,
            },
            scope,
            PROMPT.into(),
        )
        .await?;
    let scheduler = JobScheduler::start(store.clone(), runner, None);
    let inspector = store
        .inspect_job(
            job.project_id.clone(),
            job.id.clone(),
            scheduler.environment(),
        )
        .await?
        .context("queued job missing")?;
    assert_eq!(inspector.blockers, [AdmissionBlocker::QueuePaused]);
    assert!(inspector.job.latest_attempt.is_none());
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;

    let run = fixture_approval(&store, &chat.id).await?;
    assert_eq!(run.status, RunStatus::Running);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let quiet = store
        .job(job.project_id.clone(), job.id.clone())
        .await?
        .unwrap()
        .latest_attempt
        .unwrap();
    assert_eq!(quiet.status, RunStatus::Running);
    assert!(!quiet.cancel_requested);
    assert!(
        !store
            .pending_conversation_inputs(chat.id.clone(), run.id.clone())
            .await?
            .is_empty()
    );

    store
        .request_job_interrupt(chat.id.clone(), run.id.clone())
        .await?;
    tokio::time::timeout(Duration::from_secs(4), async {
        while !checkout.join("received-interrupt.ndjson").is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("provider did not receive the interrupt")?;
    let stopping = store
        .job(job.project_id.clone(), job.id.clone())
        .await?
        .unwrap()
        .latest_attempt
        .unwrap();
    assert_eq!(stopping.status, RunStatus::Running);
    assert!(stopping.cancel_requested);
    fs::write(checkout.join("settle-interrupt"), b"")?;

    let terminal = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let result = store
                .latest_conversation_run(chat.id.clone())
                .await?
                .context("attempt disappeared")?;
            if result.status.is_terminal() {
                return Ok::<_, anyhow::Error>(result);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("provider did not settle the interrupted attempt")??;
    assert_eq!(terminal.id, run.id);
    assert_eq!(terminal.status, RunStatus::Cancelled);
    assert!(
        store
            .pending_conversation_inputs(chat.id.clone(), run.id.clone())
            .await?
            .is_empty()
    );
    assert!(
        store
            .request_job_interrupt(chat.id.clone(), run.id.clone())
            .await
            .is_err()
    );
    let connection = rusqlite::Connection::open(checkout.join("fixture.sqlite"))?;
    let delivery: String = connection.query_row(
        "SELECT delivery FROM agent_permissions WHERE run_id = ?1",
        [&run.id],
        |row| row.get(0),
    )?;
    assert_eq!(delivery, "closed");
    drop(connection);
    let original = store
        .agent_dispatch(chat.id.clone(), run.id.clone())
        .await?
        .context("missing original wire evidence")?;
    let queued = store
        .queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            run.id.clone(),
            RecoveryAction::Resume,
        )
        .await?;
    assert_eq!(queued.queued_recovery, Some(RecoveryAction::Resume));
    let resumed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(current) = store.latest_conversation_run(chat.id.clone()).await?
                && current.id != run.id
            {
                assert!(
                    !current.status.is_terminal(),
                    "resumed run ended before approval: {current:?}"
                );
                if !store
                    .pending_conversation_inputs(chat.id.clone(), current.id.clone())
                    .await?
                    .is_empty()
                {
                    scheduler
                        .active_turn(&chat.id, &current.id)?
                        .approve(RequestId::Text(APPROVAL.into()), ApprovalDecision::Deny)
                        .await?;
                    return Ok::<_, anyhow::Error>(current);
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("resumed provider did not reach approval")??;
    let complete = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let latest = store
                .job(job.project_id.clone(), job.id.clone())
                .await?
                .context("job disappeared")?
                .latest_attempt
                .context("attempt disappeared")?;
            if latest.run_id == resumed.id && latest.status.is_terminal() {
                return Ok::<_, anyhow::Error>(latest);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("resumed provider did not finish")??;
    assert_eq!(complete.status, RunStatus::Succeeded);
    assert_eq!(complete.ordinal, 2);
    assert_eq!(complete.previous_run_id.as_deref(), Some(run.id.as_str()));
    assert_eq!(complete.recovery, Some(RecoveryAction::Resume));
    let resume_wire: Value =
        serde_json::from_slice(&fs::read(checkout.join("received-resume.ndjson"))?)?;
    assert_eq!(resume_wire["params"]["threadId"], SESSION);
    let resumed_wire = store
        .agent_dispatch(chat.id.clone(), resumed.id)
        .await?
        .context("missing recovered dispatch")?;
    assert_eq!(
        resumed_wire.snapshot.native_history,
        NativeHistory::ProviderRecovery {
            session_id: SESSION.into(),
            source_run_id: run.id.clone(),
            source_turn_id: Some("wire-turn".into()),
            context_id: original.snapshot.context_id,
        }
    );
    assert_eq!(resumed_wire.snapshot.submitted_messages.len(), 1);
    assert_eq!(
        original.frames,
        store.agent_dispatch(chat.id, run.id).await?.unwrap().frames
    );
    scheduler.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_stops_admission_before_reaping_a_live_cli_and_keeps_unclaimed_jobs() -> Result<()>
{
    let sandbox = tempfile::tempdir()?;
    let (store, first, runner, scope) = open_wire_fixture(Provider::Codex, sandbox.path()).await?;
    let checkout = scope.checkout.clone();
    let database = checkout.join("fixture.sqlite");
    let second = store
        .create_conversation(first.checkout_id.clone(), "Pending at exit".into())
        .await?;
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    for chat in [&first, &second] {
        store
            .save_conversation_draft(chat.id.clone(), PROMPT.into())
            .await?;
        store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id: chat.id.clone(),
                    context_id: chat.context_id.clone(),
                },
                scope.clone(),
                PROMPT.into(),
            )
            .await?;
    }
    let scheduler = JobScheduler::start(store.clone(), runner.clone(), None);
    let live = fixture_approval(&store, &first.id).await?;
    let old_dispatch = store
        .agent_dispatch(first.id.clone(), live.id.clone())
        .await?
        .context("missing live dispatch")?;
    let pending = store
        .latest_conversation_job(second.id.clone(), scheduler.environment())
        .await?
        .context("pending job missing")?;
    assert_eq!(
        pending.blockers,
        [AdmissionBlocker::Capacity {
            active: 1,
            limit: 1
        }]
    );
    let stopping = tokio::spawn(async move { scheduler.shutdown().await });
    tokio::time::timeout(Duration::from_secs(4), async {
        while !checkout.join("received-interrupt.ndjson").is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("exit did not send a provider interrupt")?;
    assert!(
        store
            .latest_conversation_run(second.id.clone())
            .await?
            .is_none()
    );
    fs::write(checkout.join("settle-interrupt"), b"")?;
    tokio::time::timeout(Duration::from_secs(10), stopping)
        .await
        .context("scheduler failed to settle on exit")???;
    assert_eq!(
        store
            .latest_conversation_run(first.id.clone())
            .await?
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
    assert!(
        store
            .latest_conversation_run(second.id.clone())
            .await?
            .is_none()
    );
    store
        .set_job_queue_settings(QueueSettings {
            paused: true,
            capacity: 1,
        })
        .await?;
    drop(store);

    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let restored = JobScheduler::start(store.clone(), runner, None);
    let pending_after_restart = store
        .latest_conversation_job(second.id.clone(), restored.environment())
        .await?
        .unwrap();
    assert_eq!(pending_after_restart.job.id, pending.job.id);
    assert_eq!(
        pending_after_restart.blockers,
        [AdmissionBlocker::QueuePaused]
    );
    assert!(
        store
            .latest_conversation_run(second.id.clone())
            .await?
            .is_none()
    );
    let restored_dispatch = store
        .agent_dispatch(first.id.clone(), live.id.clone())
        .await?
        .context("original dispatch was lost during restart")?;
    assert_eq!(restored_dispatch.frames, old_dispatch.frames);
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let next = fixture_approval(&store, &second.id).await?;
    let resumed = store
        .latest_conversation_job(second.id.clone(), restored.environment())
        .await?
        .unwrap()
        .job;
    assert_eq!(resumed.latest_attempt.unwrap().run_id, next.id);
    assert_eq!(
        store.latest_conversation_run(first.id).await?.unwrap().id,
        live.id
    );
    restored.shutdown().await?;
    Ok(())
}

fn handshake(provider: Provider, request: &Value, checkout: &Path) -> Result<Option<Value>> {
    if provider == Provider::Claude {
        assert_eq!(request["request"]["subtype"], "initialize");
        return Ok(Some(json!({
            "type":"control_response", "response":{"subtype":"success",
                "request_id":request["request_id"],"response":{"current_permission_mode":"plan"}}
        })));
    }
    let result = match (provider, request["method"].as_str()) {
        (Provider::Codex, Some("initialize")) => json!({"userAgent":"fixture"}),
        (Provider::Codex, Some("initialized")) => return Ok(None),
        (Provider::Codex, Some("thread/start" | "thread/resume")) => json!({
            "thread":{"id":SESSION},"cwd":checkout,"model":"fixture-model",
            "sandbox":{"type":"readOnly"}
        }),
        (Provider::Codex, Some("model/list" | "skills/list")) => json!({"data":[]}),
        (Provider::Codex, Some("config/read")) => json!({"config":{}}),
        (Provider::Antigravity, Some("initialize")) => {
            assert_eq!(request["params"]["protocolVersion"], 1);
            json!({"protocolVersion":1,"agentCapabilities":{}})
        }
        (Provider::Antigravity, Some("session/new")) => {
            json!({"sessionId":SESSION,"modes":{"currentModeId":"plan"}})
        }
        _ => bail!("Unexpected {provider:?} fixture request: {request}"),
    };
    Ok(Some(if provider == Provider::Antigravity {
        json!({"jsonrpc":"2.0","id":request["id"],"result":result})
    } else {
        json!({"id":request["id"],"result":result})
    }))
}

fn transcript(provider: Provider, request: &Value, turn: &str) -> impl Iterator<Item = String> {
    let template = match provider {
        Provider::Codex => include_str!("fixtures/providers/codex-app-server.jsonl"),
        Provider::Claude => include_str!("fixtures/providers/claude-stream-json.jsonl"),
        Provider::Antigravity => include_str!("fixtures/providers/antigravity-acp-v1.jsonl"),
    };
    let id = request["id"].to_string();
    template.lines().map(move |line| {
        line.replace("\"$prompt_id\"", &id)
            .replace("fixture-turn", turn)
            .replace("fixture-result", turn)
    })
}

fn receive(session: &mut dyn CliSession, data: String) -> Result<AdapterUpdate> {
    session.receive(OutputFrame {
        stream: Stream::Stdout,
        data,
    })
}

fn start_session(provider: Provider, checkout: &Path) -> Result<Box<dyn CliSession>> {
    let mut session = provider.adapter().create_session(SessionRequest {
        checkout: checkout.into(),
        intent: SessionIntent::Start,
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    })?;
    let mut pending = std::collections::VecDeque::from(session.start()?.outbound);
    while let Some(frame) = pending.pop_front() {
        let request = serde_json::from_slice(frame.as_bytes())?;
        if let Some(response) = handshake(provider, &request, checkout)? {
            pending.extend(receive(&mut *session, response.to_string())?.outbound);
        }
    }
    Ok(session)
}

fn verify_stream(provider: Provider, events: &[ProviderEvent]) {
    let find = |predicate: fn(&Event) -> bool| {
        let matches = events
            .iter()
            .enumerate()
            .filter(|(_, event)| predicate(&event.data))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{provider:?} {events:#?}");
        matches[0].0
    };
    let reasoning = find(|event| {
        matches!(event,
        Event::Reasoning { text, .. } if text == "Inspect the fixture")
    });
    let approval = find(|event| {
        matches!(event,
        Event::Approval { request_id: RequestId::Text(id), .. } if id == APPROVAL)
    });
    let delta = find(|event| matches!(event, Event::AssistantDelta { text } if text == ANSWER));
    let message = find(|event| matches!(event, Event::AssistantMessage { text } if text == ANSWER));
    let terminal = find(|event| matches!(event, Event::TurnFinished { .. }));
    assert_eq!(
        events[terminal].data,
        Event::TurnFinished {
            outcome: TurnOutcome::Completed
        }
    );
    assert!(reasoning < approval && approval < delta && delta < message && message < terminal);
    assert_eq!(
        events[terminal].location.session_id.as_deref(),
        Some(SESSION)
    );
    match provider {
        Provider::Codex | Provider::Claude => {
            let child = find(|event| matches!(event, Event::ChildActivity { child_id: None, .. }));
            assert!(reasoning < child && child < approval);
            let usage = find(|event| {
                matches!(
                    event,
                    Event::Usage {
                        input: Some(12),
                        output: Some(4),
                        cached_input: Some(2)
                    }
                )
            });
            assert!(delta < usage && usage < terminal);
        }
        Provider::Antigravity => {
            assert!(!events.iter().any(|event| matches!(
                event.data,
                Event::ChildActivity { .. } | Event::Usage { .. }
            )));
            find(|event| matches!(event, Event::Tool { name, .. } if name == "Delegate review"));
        }
    }
}

#[test]
fn successive_native_turns_keep_reasoning_approval_and_completion_separate() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    for provider in Provider::ALL {
        let mut session = start_session(provider, checkout.path())?;
        for turn in ["turn-1", "turn-2"] {
            let input = session.send(TurnInput {
                message_id: turn.into(),
                text: "Read the fixture".into(),
                attachments: Vec::new(),
            })?;
            let request = serde_json::from_slice(input.outbound[0].as_bytes())?;
            let mut events = input.events;
            let unknown = match provider {
                Provider::Codex => json!({"method":"future/event","params":{"threadId":SESSION}}),
                Provider::Claude => json!({"type":"future_event","session_id":SESSION}),
                Provider::Antigravity => json!({"jsonrpc":"2.0","method":"session/update",
                    "params":{"sessionId":SESSION,"update":{"sessionUpdate":"future_event"}}}),
            };
            let unknown = receive(&mut *session, unknown.to_string())?;
            assert!(matches!(
                unknown.events.as_slice(),
                [ProviderEvent {
                    data: Event::Diagnostic { .. },
                    ..
                }]
            ));
            for line in transcript(provider, &request, turn) {
                let update = receive(&mut *session, line)?;
                for event in &update.events {
                    if let Event::Approval { request_id, .. } = &event.data {
                        let response =
                            session.respond_approval(request_id, ApprovalDecision::Deny)?;
                        assert_eq!(response.outbound.len(), 1);
                    }
                }
                events.extend(update.events);
            }
            verify_stream(provider, &events);
            let location = &events.last().context("missing terminal event")?.location;
            match provider {
                Provider::Codex => assert_eq!(location.turn_id.as_deref(), Some(turn)),
                Provider::Claude => assert_eq!(location.item_id.as_deref(), Some(turn)),
                Provider::Antigravity => assert_eq!(location.turn_id, None),
            }
            assert!(
                session
                    .respond_approval(&RequestId::Text(APPROVAL.into()), ApprovalDecision::Deny)
                    .is_err()
            );
        }
    }
    Ok(())
}

#[test]
fn capability_sources_and_successful_fixture_evidence_have_distinct_statuses() {
    let mut capability = ResolvedCapability {
        descriptor: CapabilityDescriptor {
            subject: CapabilitySubject::Protocol {
                capability: Capability::Reasoning,
            },
            source: DeclaredSource::Adapter,
            availability: Availability::Available,
            observed: None,
            blocker: None,
            boundary: EvidenceBoundary::EquivalentConfiguration,
        },
        evidence: Some(Verification {
            id: "fixture".into(),
            checked_at: "2026-11-15".into(),
            origin: EvidenceOrigin::ProtocolFixture,
            result: ObservedResult::Succeeded,
            artifact_id: "provider-stream".into(),
            attachment_limits: None,
        }),
    };
    assert_eq!(capability.status(), CapabilityStatus::Unverified);
    capability.descriptor.source = DeclaredSource::Provider;
    assert_eq!(capability.status(), CapabilityStatus::Reported);
    capability.descriptor.observed = Some(ObservedResult::Succeeded);
    assert_eq!(capability.status(), CapabilityStatus::Observed);
    capability.descriptor.availability = Availability::Unavailable;
    assert_eq!(capability.status(), CapabilityStatus::Unavailable);
}
