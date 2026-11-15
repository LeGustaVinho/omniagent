use std::{
    ffi::OsStr,
    path::Path,
    process::Command,
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    process_io::{CapturedProcess, OutputFrame, Stream},
    providers::{
        CliAdapter, CliSession, ProviderEventData, SessionIntent, SessionMode, SessionRequest,
        WireMessage,
        codex::CodexAdapter,
        goal::{
            NativeGoalCommand, NativeGoalControlOutcome, NativeGoalObservation, NativeGoalState,
        },
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};
use serde_json::{Value, json};

struct AppServer {
    process: CapturedProcess,
    request_id: u64,
    notifications: Vec<Value>,
}

impl AppServer {
    fn launch(executable: &Path, checkout: &Path, goals: bool) -> Result<Self> {
        let arguments = [
            "-c",
            if goals {
                "features.goals=true"
            } else {
                "features.goals=false"
            },
            "app-server",
            "--listen",
            "stdio://",
        ]
        .map(OsStr::new);
        let mut server = Self {
            process: DirectWindowsRunner.launch(LaunchRequest {
                version: CONTRACT_VERSION,
                executable,
                checkout,
                arguments: &arguments,
            })?,
            request_id: 10_000,
            notifications: Vec::new(),
        };
        server.request(
            "initialize",
            json!({
                "clientInfo":{"name":"omniagent-goal-fixture","version":"1"},
                "capabilities":{"experimentalApi":true,"explicitGatewayOauth":true},
            }),
        )?;
        server.send(json!({"method":"initialized"}))?;
        Ok(server)
    }

    fn send(&mut self, frame: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        self.process.input.send(&bytes)?;
        Ok(())
    }

    fn read_until(
        &mut self,
        timeout: Duration,
        test: impl Fn(&Value) -> bool,
    ) -> Result<Option<Value>> {
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.process.output.recv_timeout(remaining) {
                Ok(frame) => {
                    let frame = frame?;
                    if frame.stream != Stream::Stdout {
                        continue;
                    }
                    let frame: Value = serde_json::from_str(&frame.data)?;
                    if test(&frame) {
                        return Ok(Some(frame));
                    }
                    if frame["method"].is_string() {
                        self.notifications.push(frame);
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id += 1;
        let id = self.request_id;
        self.send(json!({"id":id, "method":method, "params":params}))?;
        let frame = self
            .read_until(Duration::from_secs(20), |frame| frame["id"] == id)?
            .with_context(|| format!("no response to {method}"))?;
        ensure!(
            frame["error"].is_null(),
            "{method} failed: {}",
            frame["error"]
        );
        Ok(frame["result"].clone())
    }

    fn exchange(
        &mut self,
        adapter: &mut dyn CliSession,
        request: WireMessage,
    ) -> Result<omniagent::providers::AdapterUpdate> {
        let frame: Value = serde_json::from_slice(request.as_bytes())?;
        let id = &frame["id"];
        self.process.input.send(request.as_bytes())?;
        let response = self
            .read_until(Duration::from_secs(20), |frame| frame["id"] == *id)?
            .with_context(|| format!("no response to {}", frame["method"]))?;
        adapter.receive(OutputFrame {
            stream: Stream::Stdout,
            data: response.to_string(),
        })
    }

    fn control(
        &mut self,
        adapter: &mut dyn CliSession,
        command: NativeGoalCommand,
    ) -> Result<NativeGoalObservation> {
        let mut prepared = adapter.control_native_goal(command)?;
        ensure!(prepared.outbound.len() == 1);
        let response = self.exchange(adapter, prepared.outbound.remove(0))?;
        let [event] = response.events.as_slice() else {
            anyhow::bail!("expected one goal control receipt");
        };
        let ProviderEventData::NativeGoalControlSettled {
            result: NativeGoalControlOutcome::Confirmed { observation },
            ..
        } = &event.data
        else {
            anyhow::bail!("goal control was not confirmed: {:?}", event.data);
        };
        Ok(observation.clone())
    }

    fn notification(&mut self, test: impl Fn(&Value) -> bool) -> Result<Value> {
        if let Some(index) = self.notifications.iter().position(&test) {
            return Ok(self.notifications.remove(index));
        }
        self.read_until(Duration::from_secs(30), test)?
            .context("expected notification did not arrive")
    }

    fn finish(self) -> Result<()> {
        self.process.control.cancel()?;
        self.process.control.wait()?;
        Ok(())
    }
}

#[test]
#[ignore = "requires an installed authenticated Codex App Server with native goals and gpt-6-luna"]
fn codex_injects_goal_context_without_spending_the_goal_budget_on_a_setup_turn() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    let checkout = dunce::canonicalize(checkout.path())?;
    let executable = CodexAdapter.discover(None)?;
    let mut server = AppServer::launch(&executable, &checkout, true)?;
    let opened = server.request(
        "thread/start",
        json!({
            "cwd":checkout, "sandbox":"read-only", "approvalPolicy":"never", "model":"gpt-6-luna",
        }),
    )?;
    let thread = opened["thread"]["id"]
        .as_str()
        .context("missing fresh thread ID")?;
    server.request("thread/inject_items", json!({
        "threadId":thread,"items":[{
            "type":"message", "role":"user",
            "content":[{"type":"input_text","text":"The fixture's requested output is the word PINE. Use no tools."}]
        }]
    }))?;
    let turn_started =
        |frame: &Value| frame["method"] == "turn/started" && frame["params"]["threadId"] == thread;
    assert!(!server.notifications.iter().any(turn_started));
    let started = server.request("thread/goal/set", json!({
        "threadId":thread, "origin":"user", "status":"active","tokenBudget":512,
        "objective":"Respond with the requested fixture output from the user's context. Use no tools."
    }))?;
    assert_eq!(started["goal"]["tokensUsed"], 0);
    let autonomous = server.notification(turn_started)?;
    let turn = &autonomous["params"]["turn"]["id"];
    let result = server.notification(|frame| {
        frame["method"] == "turn/completed" && frame["params"]["turn"]["id"] == *turn
    })?;
    assert_eq!(result["params"]["turn"]["status"], "completed");
    assert!(server.notifications.iter().any(|frame| {
        frame["method"] == "item/completed"
            && frame["params"]["turnId"] == *turn
            && frame["params"]["item"]["type"] == "agentMessage"
            && frame["params"]["item"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("PINE"))
    }));
    server.finish()?;
    Ok(())
}

#[test]
#[ignore = "requires an installed authenticated Codex App Server with native goals and gpt-6-luna"]
fn codex_native_goal_budget_control_and_restore() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    let checkout = dunce::canonicalize(checkout.path())?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    let executable = CodexAdapter.discover(None)?;
    let snapshot = || {
        json!({
            "cwd": checkout, "sandbox": "read-only", "approvalPolicy": "never", "model": "gpt-6-luna",
        })
    };
    let mut server = AppServer::launch(&executable, &checkout, true)?;
    let start = server.request("thread/start", snapshot())?;
    let thread = start["thread"]["id"]
        .as_str()
        .context("missing fresh thread ID")?;
    let status = |status| json!({"threadId":thread,"origin":"user","status":status});
    let mut adapter = CodexAdapter.create_session(SessionRequest {
        checkout: checkout.clone(),
        intent: SessionIntent::Start,
        mode: SessionMode::ReadOnly,
        model: Some("gpt-6-luna".into()),
        effort: None,
    })?;
    let frame = |message: Value| OutputFrame {
        stream: Stream::Stdout,
        data: message.to_string(),
    };
    adapter.start()?;
    adapter.receive(frame(json!({"id":1,"result":{"userAgent":"goal-fixture"}})))?;
    adapter.receive(frame(json!({"id":2,"result":start})))?;
    let configuration = adapter
        .query_effective_configuration()?
        .context("missing Codex config request")?;
    server.exchange(adapter.as_mut(), configuration)?;

    let absent = server.control(adapter.as_mut(), NativeGoalCommand::Status)?;
    assert_eq!(absent.state, NativeGoalState::Cleared);
    let started = server.control(
        adapter.as_mut(),
        NativeGoalCommand::Start {
            objective: "Read-only fixture. Follow the next user request without calling tools."
                .into(),
            token_budget: Some(256),
        },
    )?;
    assert_eq!(started.state, NativeGoalState::Active);
    assert_eq!(started.token_budget, Some(256));
    assert_eq!(started.tokens_used, Some(0));
    let paused = server.control(adapter.as_mut(), NativeGoalCommand::Pause)?;
    assert_eq!(paused.state, NativeGoalState::Paused);
    let resumed = server.control(adapter.as_mut(), NativeGoalCommand::Resume)?;
    assert_eq!(resumed.state, NativeGoalState::Active);
    let turn = server.request("turn/start", json!({
        "threadId":thread,
        "input":[{"type":"text","text":"Print numbers one to five thousand in words. Use no tools."}],
    }))?;
    let turn = &turn["turn"]["id"];
    let budget = server.notification(|frame| {
        frame["method"] == "thread/goal/updated"
            && frame["params"]["threadId"] == thread
            && frame["params"]["turnId"] == *turn
            && frame["params"]["goal"]["status"] == "budgetLimited"
    })?;
    let used = budget["params"]["goal"]["tokensUsed"]
        .as_u64()
        .context("missing usage")?;
    assert!(used >= 256);
    let decoded = adapter.receive(frame(budget))?;
    assert!(decoded.events.iter().any(|event| matches!(&event.data,
        ProviderEventData::GoalObserved { observation }
            if observation.state == NativeGoalState::BudgetLimited && observation.tokens_used == Some(used)
                && observation.token_budget == Some(256)
                && observation.provider_turn_id.as_deref() == turn.as_str()
    )));
    server.notification(|frame| {
        frame["method"] == "turn/completed" && frame["params"]["turn"]["id"] == *turn
    })?;
    let first_goal = server.control(adapter.as_mut(), NativeGoalCommand::Status)?;
    assert_eq!(first_goal.tokens_used, Some(used));
    assert_eq!(first_goal.token_budget, Some(256));
    let cleared = server.control(adapter.as_mut(), NativeGoalCommand::Cancel)?;
    assert_eq!(cleared.state, NativeGoalState::Cleared);
    let cleared = server.notification(|frame| {
        frame["method"] == "thread/goal/cleared" && frame["params"]["threadId"] == thread
    })?;
    let decoded = adapter.receive(frame(cleared))?;
    assert!(decoded.events.iter().any(|event| matches!(&event.data,
        ProviderEventData::GoalObserved { observation }
            if observation.state == NativeGoalState::Cleared && event.location.turn_id.is_none()
    )));

    let second = server.control(
        adapter.as_mut(),
        NativeGoalCommand::Start {
            objective:
                "Read-only lifecycle check. Wait for host action. Do not use tools or write files."
                    .into(),
            token_budget: Some(40_000),
        },
    )?;
    assert_eq!(second.state, NativeGoalState::Active);
    let automatic = |frame: &Value| {
        frame["method"] == "turn/started"
            && frame["params"]["threadId"] == thread
            && frame["params"]["turn"]["id"] != *turn
    };
    let autonomous = server.notification(automatic)?;
    let decoded = adapter.receive(frame(autonomous.clone()))?;
    assert!(decoded.events.iter().any(|event| {
        matches!(event.data, ProviderEventData::TurnStarted)
            && event.location.turn_id.as_deref() == autonomous["params"]["turn"]["id"].as_str()
    }));
    server.finish()?;

    let mut resume_params = snapshot();
    resume_params["threadId"] = json!(thread);
    let mut managed = AppServer::launch(&executable, &checkout, false)?;
    assert_eq!(
        managed.request("thread/resume", resume_params.clone())?["thread"]["id"],
        thread
    );
    let explicit_or_automatic_turn =
        |frame: &Value| frame["method"] == "turn/started" && frame["params"]["threadId"] == thread;
    assert!(!managed.notifications.iter().any(explicit_or_automatic_turn));
    assert!(
        managed
            .read_until(Duration::from_secs(3), explicit_or_automatic_turn)?
            .is_none()
    );
    managed.finish()?;

    let mut native = AppServer::launch(&executable, &checkout, true)?;
    native.request("thread/resume", resume_params)?;
    let turn = native.notification(explicit_or_automatic_turn)?;
    let turn = &turn["params"]["turn"]["id"];
    let paused = native.request("thread/goal/set", status("paused"))?;
    assert_eq!(paused["goal"]["status"], "paused");
    native.request("turn/interrupt", json!({"threadId":thread,"turnId":turn}))?;
    let completed = native.notification(|frame| {
        frame["method"] == "turn/completed" && frame["params"]["turn"]["id"] == *turn
    })?;
    assert_eq!(completed["params"]["turn"]["status"], "interrupted");
    assert_eq!(
        native.request("thread/goal/get", json!({"threadId":thread}))?["goal"]["status"],
        "paused"
    );
    assert_eq!(
        native.request(
            "thread/goal/clear",
            json!({"threadId":thread,"origin":"user"})
        )?["cleared"],
        true
    );
    assert!(native.request("thread/goal/get", json!({"threadId":thread}))?["goal"].is_null());
    native.finish()?;
    Ok(())
}
