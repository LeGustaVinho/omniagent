use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::{AttemptPlan, SessionScope},
    mcp_bridge::{HELPER_FLAG, RunBridge},
    process_io::{CapturedProcess, Stream},
    providers::{Provider, SessionMode, WireMessage},
    repository,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};
use serde_json::{Value, json};

async fn attempt(store: &Store, checkout: PathBuf) -> Result<AttemptPlan> {
    fs::create_dir(&checkout)?;
    let checkout = dunce::canonicalize(checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    new_attempt(store, checkout).await
}

async fn new_attempt(store: &Store, checkout: PathBuf) -> Result<AttemptPlan> {
    let project = repository::register(store, checkout.clone()).await?;
    let conversation = store
        .create_conversation(project.checkout_id, "Bridge fixture".into())
        .await?;
    let plan = store
        .prepare_agent_attempt(
            conversation.id,
            conversation.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "bridge-fixture".into(),
                account_binding: None,
                executable: env!("CARGO_BIN_EXE_omniagent").into(),
                checkout,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: "read-only".into(),
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            },
            "inspect scope".into(),
        )
        .await?;
    store.mark_agent_running(plan.run.id.clone()).await?;
    Ok(plan)
}

fn receive(process: &CapturedProcess, id: usize) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
        ensure!(
            frame.stream == Stream::Stdout,
            "MCP helper stderr: {}",
            frame.data
        );
        let value: Value = serde_json::from_str(&frame.data)?;
        if value["id"] == id {
            return Ok(value);
        }
    }
}

fn send(process: &mut CapturedProcess, frame: Value) -> Result<()> {
    Ok(process.input.send(WireMessage::json(&frame)?.as_bytes())?)
}

async fn start_helper(ticket: PathBuf, checkout: PathBuf) -> Result<CapturedProcess> {
    tokio::task::spawn_blocking(move || {
        let mut process = DirectWindowsRunner.launch(LaunchRequest {
            version: CONTRACT_VERSION,
            executable: Path::new(env!("CARGO_BIN_EXE_omniagent")),
            checkout: &checkout,
            arguments: &[OsStr::new(HELPER_FLAG), ticket.as_os_str()],
        })?;
        send(&mut process, json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"omniagent-test","version":"1"}
        }}))?;
        ensure!(receive(&process, 1)?["result"]["capabilities"]["tools"].is_object());
        send(&mut process, json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))?;
        send(&mut process, json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}))?;
        let listing = receive(&process, 2)?;
        ensure!(listing["result"]["tools"][0]["name"] == "inspect_run_scope");
        Ok(process)
    }).await?
}

async fn invoke(process: CapturedProcess, arguments: Value) -> Result<(CapturedProcess, Value)> {
    invoke_tool(process, "inspect_run_scope", arguments).await
}

async fn invoke_tool(
    mut process: CapturedProcess,
    name: &'static str,
    arguments: Value,
) -> Result<(CapturedProcess, Value)> {
    tokio::task::spawn_blocking(move || {
        send(
            &mut process,
            json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{
                "name":name, "arguments":arguments
            }}),
        )?;
        let response = receive(&process, 3).context("scope tool response")?;
        Ok((process, response))
    })
    .await?
}

fn expect_scope(response: &Value, attempt: &AttemptPlan) {
    assert_eq!(
        response["result"]["structuredContent"],
        json!({
            "runId":attempt.run.id, "projectId":attempt.run.project_id,
            "checkoutId":attempt.run.checkout_id, "turnId":attempt.run.turn_id,
        })
    );
}

#[tokio::test]
async fn stdio_helpers_report_only_the_identity_of_their_active_attempt() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("bridge.sqlite"))?;
    let first = attempt(&store, directory.path().join("alpha")).await?;
    let second = attempt(&store, directory.path().join("beta")).await?;
    assert_ne!(first.run.project_id, second.run.project_id);
    let first_bridge = RunBridge::start(&store, first.run.id.clone()).await?;
    let second_bridge = RunBridge::start(&store, second.run.id.clone()).await?;
    let first_helper = start_helper(
        first_bridge.ticket_path().into(),
        first.scope.checkout.clone(),
    )
    .await?;
    let second_helper = start_helper(
        second_bridge.ticket_path().into(),
        second.scope.checkout.clone(),
    )
    .await?;
    let (first_helper, response) = invoke(first_helper, json!({})).await?;
    expect_scope(&response, &first);
    let (first_helper, response) = invoke(first_helper, json!({"runId":second.run.id})).await?;
    assert!(response["error"].is_object());
    let (second_helper, response) = invoke(second_helper, json!({})).await?;
    expect_scope(&response, &second);

    store
        .finish_agent_attempt(first.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    let (_first_helper, response) = invoke(first_helper, json!({})).await?;
    assert!(response["error"].is_object());
    let (_second_helper, response) = invoke(second_helper, json!({})).await?;
    expect_scope(&response, &second);

    let first_ticket = first_bridge.ticket_path().to_path_buf();
    drop(first_bridge);
    assert!(!first_ticket.exists());
    assert!(
        RunBridge::start(&store, first.run.id.clone())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn run_bound_mcp_submits_jobs_idempotently_and_only_discloses_its_own_children() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("bridge.sqlite"))?;
    let parent = attempt(&store, directory.path().join("parent")).await?;
    let stranger = new_attempt(&store, parent.scope.checkout.clone()).await?;
    let foreign = attempt(&store, directory.path().join("foreign")).await?;
    let task = store
        .create_project_task(parent.run.project_id.clone(), "Parent project task".into())
        .await?;
    let foreign_task = store
        .create_project_task(foreign.run.project_id, "Other project task".into())
        .await?;
    let bridge = RunBridge::start(&store, parent.run.id.clone()).await?;
    let stranger_bridge = RunBridge::start(&store, stranger.run.id.clone()).await?;
    let stranger_helper = start_helper(
        stranger_bridge.ticket_path().into(),
        stranger.scope.checkout,
    )
    .await?;
    let helper = start_helper(bridge.ticket_path().into(), parent.scope.checkout).await?;
    let request = json!({"requestId":"child-once", "request":{
        "mode":"plan", "instructions":"Plan a bounded test change", "taskId":task.id,
    }});
    let (helper, submitted) = invoke_tool(helper, "submit_job", request.clone()).await?;
    let id = submitted["result"]["structuredContent"]["jobId"]
        .as_str()
        .with_context(|| format!("submit response: {submitted}"))?;
    let (helper, replay) = invoke_tool(helper, "submit_job", request.clone()).await?;
    assert_eq!(replay["result"]["structuredContent"]["jobId"], id);

    let mut changed = request.clone();
    changed["request"]["instructions"] = json!("Reused ID with different input");
    let (helper, conflict) = invoke_tool(helper, "submit_job", changed).await?;
    assert!(
        conflict["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already used")
    );
    let (helper, status) = invoke_tool(helper, "get_job_status", json!({"jobId":id})).await?;
    let current = &status["result"]["structuredContent"];
    assert_eq!(current["id"], id);
    assert_eq!(current["projectId"], parent.run.project_id);
    assert_eq!(current["taskId"], task.id);
    assert_eq!(current["origin"]["runId"], parent.run.id);
    assert_eq!(current["context"]["contextId"], parent.context_id);
    assert_eq!(current["queueState"], "queued");
    assert!(current.get("instructions").is_none());

    let (_stranger_helper, denied) =
        invoke_tool(stranger_helper, "get_job_status", json!({"jobId":id})).await?;
    assert!(denied["error"].is_object());
    let (helper, spoofed) = invoke_tool(
        helper,
        "get_job_status",
        json!({"jobId":id,"runId":stranger.run.id}),
    )
    .await?;
    assert!(spoofed["error"].is_object());
    let (helper, foreign_task) = invoke_tool(
        helper,
        "submit_job",
        json!({"requestId":"foreign-task", "request":{
            "mode":"plan", "instructions":"Try another project", "taskId":foreign_task.id,
        }}),
    )
    .await?;
    assert!(foreign_task["error"].is_object());
    let (helper, foreign_checkout) = invoke_tool(helper, "submit_job", json!({"requestId":"foreign-checkout", "request":{
        "mode":"plan", "instructions":"Try another checkout", "checkoutId":foreign.run.checkout_id,
    }})).await?;
    assert!(foreign_checkout["error"].is_object());

    store
        .finish_agent_attempt(parent.run.id, RunStatus::Cancelled, None)
        .await?;
    let (_helper, expired) = invoke_tool(helper, "get_job_status", json!({"jobId":id})).await?;
    assert!(expired["error"].is_object());
    Ok(())
}
