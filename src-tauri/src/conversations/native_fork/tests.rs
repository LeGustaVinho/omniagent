use super::*;
use crate::{
    conversations::{
        AttemptPlan, ContinuationChoice, ContinuationInspection,
        tests::{prepare, setup},
    },
    dispatch::{Delivery, DispatchEvidence},
    jobs::{AdmissionEnvironment, ProjectSubagentPolicy, SubagentLimitMode},
    process::TerminalOutcome,
    process_io::{CapturedProcess, OutputFrame, ProcessInput, Stream},
    providers::{CliAdapter, ProviderEventData, codex::CodexAdapter, runtime::start_managed_turn},
    runner::{DirectWindowsRunner, LaunchRequest, RunControl, Runner},
    runs::RunStatus,
};
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Default)]
struct ForkRunner {
    launches: AtomicU32,
    reject_fork_boundary: bool,
}

impl Runner for ForkRunner {
    fn identity(&self) -> &str {
        DirectWindowsRunner::IDENTITY
    }

    fn executable_revision(&self, _: &Path) -> Option<String> {
        Some("codex-fork-fixture".into())
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        let (send, output) = mpsc::channel();
        let input = if request.arguments == ["plugin", "list", "--json"].map(std::ffi::OsStr::new) {
            send_frame(&send, json!({"installed": []}))?;
            ProcessInput::new(io::sink())
        } else {
            ProcessInput::new(ForkWire {
                launch: self.launches.fetch_add(1, Ordering::Relaxed),
                checkout: request.checkout.to_string_lossy().into_owned(),
                session: String::new(),
                model: String::new(),
                fork_turn: None,
                reject_fork_boundary: self.reject_fork_boundary,
                send,
            })
        };
        Ok(CapturedProcess {
            input,
            output,
            control: Arc::new(CompletedProcess),
        })
    }
}

struct CompletedProcess;

impl RunControl for CompletedProcess {
    fn cancel(&self) -> io::Result<bool> {
        Ok(false)
    }
    fn wait(&self) -> io::Result<TerminalOutcome> {
        Ok(TerminalOutcome::Exited(0))
    }
    fn confirm_quiescence(&self) -> io::Result<()> {
        Ok(())
    }
}

struct ForkWire {
    launch: u32,
    checkout: String,
    session: String,
    model: String,
    fork_turn: Option<String>,
    reject_fork_boundary: bool,
    send: mpsc::Sender<io::Result<OutputFrame>>,
}

fn send_frame(send: &mpsc::Sender<io::Result<OutputFrame>>, value: Value) -> io::Result<()> {
    send.send(Ok(OutputFrame {
        stream: Stream::Stdout,
        data: value.to_string(),
    }))
    .map_err(io::Error::other)
}

impl Write for ForkWire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let frame: Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        let id = &frame["id"];
        let params = &frame["params"];
        let respond = |result| send_frame(&self.send, json!({"id": id, "result": result}));
        match frame["method"].as_str().unwrap() {
            "initialize" => respond(json!({"userAgent": "codex-fork-fixture"}))?,
            "initialized" => (),
            method @ ("thread/start" | "thread/resume" | "thread/fork") => {
                self.model = params["model"].as_str().unwrap().into();
                self.session = if method == "thread/resume" {
                    params["threadId"].as_str().unwrap().into()
                } else if method == "thread/fork" {
                    format!("fork-{}", self.launch)
                } else {
                    format!("session-{}", self.launch)
                };
                if method == "thread/fork" {
                    assert_ne!(params["threadId"].as_str().unwrap(), self.session);
                    assert_eq!(params["excludeTurns"], true);
                    self.fork_turn = Some(params["lastTurnId"].as_str().unwrap().into());
                }
                respond(json!({"thread": {"id": self.session}, "model": self.model,
                    "cwd": self.checkout, "sandbox": {"type": "readOnly"}}))?;
            }
            "thread/turns/list" => {
                assert_eq!(params["threadId"], self.session);
                let expected = self.fork_turn.take().unwrap();
                let turn = if self.reject_fork_boundary {
                    "a-different-turn"
                } else {
                    &expected
                };
                respond(json!({"data": [{"id": turn, "status": "completed"}]}))?;
            }
            "model/list" => respond(json!({"data": []}))?,
            "config/read" => respond(json!({"config": {
                "approval_policy": "never",
                "sandbox_mode": "read-only",
                "agents": {"enabled": false},
                "features": {"multi_agent_v2": {"max_concurrent_threads_per_session": 1}}
            }}))?,
            "skills/list" => respond(json!({"data": []}))?,
            "turn/start" => {
                assert!(
                    self.fork_turn.is_none(),
                    "The prompt reached the unverified fork"
                );
                assert_eq!(params["threadId"], self.session);
                let turn = format!("turn-{}", self.launch);
                respond(json!({"turn": {"id": turn}}))?;
                for frame in [
                    json!({"method":"item/completed","params":{"threadId":self.session,"turnId":turn,
                      "item":{"type":"agentMessage","id":"response","text":"Fixture response"}}}),
                    json!({"method":"turn/completed","params":{"threadId":self.session,
                      "turn":{"id":turn,"status":"completed"}}}),
                ] {
                    send_frame(&self.send, frame)?;
                }
            }
            method => panic!("Unexpected fixture method {method}"),
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn environment(scope: &SessionScope) -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: scope.runner.clone(),
        bridge_available: false,
    }
}

async fn dispatch(
    store: &Store,
    runner: &Arc<dyn Runner>,
    plan: AttemptPlan,
) -> Result<DispatchEvidence> {
    let conversation = plan.conversation_id.clone();
    let run = plan.run.id.clone();
    let status = tokio::time::timeout(
        Duration::from_secs(50),
        start_managed_turn(store.clone(), runner.clone(), plan, None).wait(),
    )
    .await??;
    if status != RunStatus::Succeeded {
        let events = store
            .agent_observations(conversation.clone(), run.clone(), 0)
            .await?;
        let observations = events
            .into_iter()
            .map(|event| event.event.data)
            .collect::<Vec<_>>();
        anyhow::bail!("Managed turn {status:?}: {observations:?}");
    }
    let dispatch = store
        .agent_dispatch(conversation, run)
        .await?
        .context("Missing dispatch")?;
    assert_eq!(dispatch.delivery, Delivery::Submitted);
    Ok(dispatch)
}

async fn exercise_forks(live: bool) -> Result<()> {
    let (dir, store, chat, mut scope) = setup().await?;
    scope.runner = DirectWindowsRunner::IDENTITY.into();
    let runner: Arc<dyn Runner> = if live {
        scope.executable = CodexAdapter.discover(None)?;
        scope.model = Some("gpt-6-luna".into());
        scope.effort = None;
        Arc::new(DirectWindowsRunner)
    } else {
        std::fs::write(&scope.executable, b"fixture")?;
        Arc::new(ForkRunner::default())
    };
    let checkout = chat.checkout_id.clone();
    let project = store
        .execute(move |db| {
            Ok(db.query_row(
                "SELECT project_id FROM checkouts WHERE id = ?1",
                [checkout],
                |row| row.get(0),
            )?)
        })
        .await?;
    store
        .set_project_subagent_policy(
            project,
            ProjectSubagentPolicy {
                mode: SubagentLimitMode::Strict,
                maximum: 0,
            },
        )
        .await?;
    let first = prepare(&store, &chat.id, scope.clone(), "Reply only with MAPLE").await?;
    let first_run = first.run.id.clone();
    let first_dispatch = dispatch(&store, &runner, first).await?;
    let local_source_session = first_dispatch
        .snapshot
        .session_id
        .context("No local source session")?;
    let source_session = store
        .conversation_sessions(chat.id.clone())
        .await?
        .into_iter()
        .find(|session| session.id == local_source_session)
        .and_then(|session| session.provider_session_id)
        .context("No provider source session")?;
    let first_boundary = store.conversation_history(chat.id.clone()).await?;
    let earlier = first_boundary
        .messages
        .last()
        .context("No source answer")?
        .revision_id
        .clone();
    let second = prepare(&store, &chat.id, scope.clone(), "Reply only with FIR").await?;
    assert!(
        matches!(&second.intent, SessionIntent::Resume { provider_session_id } if provider_session_id == &source_session)
    );
    let second_run = second.run.id.clone();
    dispatch(&store, &runner, second).await?;
    let head = store.conversation_history(chat.id.clone()).await?;
    let mut provider_sessions = std::collections::HashSet::from([source_session.clone()]);
    let mut saved_forks = Vec::new();

    for (boundary, source_run, source_context) in [
        (None, second_run.as_str(), head.context_id.as_str()),
        (
            Some(earlier.clone()),
            first_run.as_str(),
            first_boundary.context_id.as_str(),
        ),
    ] {
        let fork = store
            .fork_conversation(
                chat.id.clone(),
                head.context_id.clone(),
                boundary,
                "Native branch".into(),
            )
            .await?;
        store
            .save_conversation_draft(fork.id.clone(), "Reply only with BIRCH".into())
            .await?;
        let sent = store
            .submit_conversation_continuation(
                fork.id.clone(),
                fork.context_id,
                scope.clone(),
                "Reply only with BIRCH".into(),
            )
            .await?;
        assert!(matches!(sent, ContinuationInspection::Queued { .. }));
        let plan = store
            .claim_next_job(environment(&scope))
            .await?
            .context("Fork was not admitted")?;
        assert_eq!(plan.input.len(), 1);
        assert_eq!(plan.input[0].text, "Reply only with BIRCH");
        assert!(matches!(plan.intent, SessionIntent::Fork { .. }));
        let fork_run = plan.run.id.clone();
        let result = dispatch(&store, &runner, plan).await?;
        assert_ne!(
            result.snapshot.session_id.as_deref(),
            Some(local_source_session.as_str())
        );
        let sessions = store.conversation_sessions(fork.id.clone()).await?;
        assert_eq!(sessions.len(), 1);
        assert!(
            provider_sessions.insert(
                sessions[0]
                    .provider_session_id
                    .clone()
                    .context("No provider fork session")?
            )
        );
        assert_eq!(result.snapshot.submitted_messages.len(), 1);
        let NativeHistory::ProviderFork {
            session_id,
            source_run_id,
            source_turn_id,
            context_id,
        } = result.snapshot.native_history
        else {
            anyhow::bail!("Missing protected native fork boundary");
        };
        assert_eq!(session_id, source_session);
        assert_eq!(source_run_id, source_run);
        assert_eq!(context_id, source_context);
        let events = store
            .agent_observations(fork.id.clone(), fork_run.clone(), 0)
            .await?;
        assert!(events.iter().any(|event| matches!(
            &event.event.data, ProviderEventData::NativeForkVerified { source_session_id, last_turn_id }
                if source_session_id == &session_id && last_turn_id == &source_turn_id
        )));
        let inventory = store
            .dispatch_inventory(fork.id.clone(), fork_run.clone())
            .await?
            .context("Missing inventory")?;
        assert_eq!(
            inventory
                .inherited_checkout
                .context("Missing source checkout")?
                .run_id,
            source_run
        );
        saved_forks.push((
            fork.id,
            fork_run,
            source_run.to_owned(),
            source_context.to_owned(),
        ));
    }

    if !live {
        let rejected = store
            .fork_conversation(
                chat.id.clone(),
                head.context_id.clone(),
                Some(earlier.clone()),
                "Different boundary".into(),
            )
            .await?;
        let plan = prepare(&store, &rejected.id, scope.clone(), "Reply only with ASH").await?;
        assert!(matches!(plan.intent, SessionIntent::Fork { .. }));
        let run_id = plan.run.id.clone();
        let runner: Arc<dyn Runner> = Arc::new(ForkRunner {
            launches: AtomicU32::new(100),
            reject_fork_boundary: true,
        });
        let result = dispatch(&store, &runner, plan).await?;
        assert_eq!(result.snapshot.native_history, NativeHistory::Fresh);
        let sessions = store.conversation_sessions(rejected.id.clone()).await?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].provider_session_id.as_deref(),
            Some("session-100")
        );
        assert_eq!(result.snapshot.submitted_messages.len(), 3);
        assert_eq!(
            result.snapshot.submitted_messages[0].text,
            "Reply only with MAPLE"
        );
        assert_eq!(
            result.snapshot.submitted_messages[2].text,
            "Reply only with ASH"
        );
        let events = store.agent_observations(rejected.id, run_id, 0).await?;
        assert!(events.iter().any(|event| matches!(
            event.event.data,
            ProviderEventData::NativeForkUnavailable { .. }
        )));
        assert!(!events.iter().any(|event| matches!(
            event.event.data,
            ProviderEventData::NativeForkVerified { .. }
        ) || event.event.location.session_id.as_deref()
            == Some("fork-100")));
    }

    let fresh = store
        .fork_conversation(
            chat.id.clone(),
            head.context_id.clone(),
            Some(earlier.clone()),
            "Edited branch".into(),
        )
        .await?;
    let copied = store.conversation_history(fresh.id.clone()).await?;
    store
        .replace_conversation_message(
            fresh.id.clone(),
            fresh.context_id,
            copied.messages[0].message_id.clone(),
            "The revised independent seed".into(),
        )
        .await?;
    let edited = store.conversation_history(fresh.id.clone()).await?;
    let draft = "Reply only with OAK";
    store
        .save_conversation_draft(fresh.id.clone(), draft.into())
        .await?;
    let ContinuationInspection::ReviewRequired { preview_id, .. } = store
        .submit_conversation_continuation(
            fresh.id.clone(),
            edited.context_id,
            scope.clone(),
            draft.into(),
        )
        .await?
    else {
        anyhow::bail!("The edited fork should require a fresh-session review");
    };
    store
        .confirm_conversation_continuation(
            fresh.id.clone(),
            preview_id,
            ContinuationChoice::SameTab,
            scope.clone(),
            draft.into(),
        )
        .await?;
    let plan = store
        .claim_next_job(environment(&scope))
        .await?
        .context("Edited fork was not admitted")?;
    assert!(matches!(plan.intent, SessionIntent::Start));
    let result = dispatch(&store, &runner, plan).await?;
    assert_eq!(result.snapshot.native_history, NativeHistory::Fresh);
    assert_eq!(
        result.snapshot.submitted_messages[0].text,
        "The revised independent seed"
    );
    assert_eq!(
        result.snapshot.submitted_messages.last().unwrap().text,
        draft
    );

    let rewind = store
        .rewind_conversation_history(chat.id.clone(), head.context_id.clone(), Some(earlier))
        .await?;
    assert_eq!(
        store.conversation_messages(chat.id.clone()).await?.len(),
        first_boundary.messages.len()
    );
    store
        .rewind_conversation_history(chat.id.clone(), rewind, None)
        .await?;
    let cleared = prepare(&store, &chat.id, scope.clone(), "Reply only with ELM").await?;
    assert!(matches!(cleared.intent, SessionIntent::Start));
    let result = dispatch(&store, &runner, cleared).await?;
    assert_eq!(result.snapshot.submitted_messages.len(), 1);
    assert_eq!(
        result.snapshot.submitted_messages[0].text,
        "Reply only with ELM"
    );
    drop(store);
    let reopened = Store::open(dir.path().join("store.sqlite"))?;
    let saved = reopened
        .conversation_history_revision(chat.id, head.context_id)
        .await?;
    assert_eq!(saved.messages[0].text, "Reply only with MAPLE");
    assert_eq!(saved.messages[2].text, "Reply only with FIR");
    for (conversation, run, source_run, source_context) in saved_forks {
        let evidence = reopened
            .agent_dispatch(conversation, run)
            .await?
            .context("Lost fork across restart")?;
        assert!(matches!(
            evidence.snapshot.native_history,
            NativeHistory::ProviderFork { source_run_id, context_id, .. }
                if source_run_id == source_run && context_id == source_context
        ));
    }
    Ok(())
}

#[tokio::test]
async fn managed_runtime_forks_exact_turns_and_replays_an_edited_seed() -> Result<()> {
    exercise_forks(false).await
}

#[tokio::test]
#[ignore = "requires an authenticated Codex CLI"]
async fn authenticated_codex_managed_fork() -> Result<()> {
    ensure!(cfg!(windows), "This fixture uses the Windows runner");
    exercise_forks(true).await
}
