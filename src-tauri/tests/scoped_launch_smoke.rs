use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::{Conversation, SessionScope},
    customizations::{CustomizationInventory, CustomizationKind},
    dispatch::NativeHistory,
    mcp_bridge::Registration,
    providers::{
        ApprovalDecision, Provider, ProviderEventData, SessionMode,
        profiles::{ConfiguredExecutable, probe_cli},
        runtime::start_managed_turn,
    },
    repository::{self, ProjectCheckout},
    runner::DirectWindowsRunner,
    runs::RunStatus,
    store::Store,
};

struct Fixture {
    _temporary: tempfile::TempDir,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temporary = tempfile::tempdir()?;
        let path = dunce::canonicalize(temporary.path())?.join("checkout");
        fs::create_dir(&path)?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "-q"])
                .current_dir(&path)
                .status()?
                .success()
        );
        for (kind, marker) in [("approved", "approved-mcp"), ("pending", "pending-mcp")] {
            fs::write(
                path.join(format!("{kind}.js")),
                format!("require('node:fs').writeFileSync('{marker}','started');process.exit(0);"),
            )?;
        }
        Ok(Self {
            _temporary: temporary,
            path,
        })
    }

    fn doc(&self, name: &str, marker: &str) -> Result<()> {
        fs::write(
            self.path.join(name),
            format!(
                "For the phrase 'instruction audit', reply exactly {marker}. Do not use tools."
            ),
        )?;
        Ok(())
    }

    async fn register(&self) -> Result<(Store, ProjectCheckout)> {
        let store = Store::open(
            self.path
                .parent()
                .expect("fixture parent")
                .join("data.sqlite"),
        )?;
        let checkout = repository::register(&store, self.path.clone()).await?;
        Ok((store, checkout))
    }
}

async fn approve(
    store: &Store,
    checkout: &ProjectCheckout,
    inventory: CustomizationInventory,
    label: &str,
    kind: CustomizationKind,
) -> Result<CustomizationInventory> {
    let entry = inventory
        .entries
        .iter()
        .find(|entry| entry.label == label && entry.kind == kind)
        .context("fixture declaration missing from the review")?;
    store
        .approve_checkout_customization(
            checkout.checkout_id.clone(),
            entry.id.clone(),
            entry.fingerprint.clone(),
        )
        .await
}

fn scope(provider: Provider, model: &str, path: &Path) -> Result<SessionScope> {
    let observed = probe_cli(ConfiguredExecutable {
        provider,
        executable: None,
    })?;
    Ok(SessionScope {
        provider,
        executable: observed.executable.context("fixture CLI missing")?.into(),
        cli_version: observed.version.context("fixture CLI version missing")?,
        checkout: path.into(),
        runner: "windows-direct".into(),
        profile_id: "scoped-launch-fixture".into(),
        account_binding: Some("vendor-test-account".into()),
        mode: SessionMode::ReadOnly,
        permission_revision: "read-only".into(),
        model: Some(model.into()),
        effort: None,
    })
}

async fn turn(
    store: &Store,
    conversation: &Conversation,
    scope: SessionScope,
) -> Result<(String, RunStatus)> {
    turn_with_prompt(store, conversation, scope,
        "Instruction audit. Follow the repository instruction context. Do not read files or use tools.").await
}

async fn turn_with_prompt(
    store: &Store,
    conversation: &Conversation,
    scope: SessionScope,
    prompt: &str,
) -> Result<(String, RunStatus)> {
    turn_with_approval(store, conversation, scope, prompt, None).await
}

async fn turn_with_approval(
    store: &Store,
    conversation: &Conversation,
    scope: SessionScope,
    prompt: &str,
    approval: Option<(&str, ApprovalDecision)>,
) -> Result<(String, RunStatus)> {
    let current = store.conversation(conversation.id.clone()).await?;
    let attempt = store
        .prepare_agent_attempt(current.id, current.context_id, scope, prompt.into())
        .await?;
    let run = attempt.run.id.clone();
    let running = start_managed_turn(
        store.clone(),
        Arc::new(DirectWindowsRunner),
        attempt,
        Some(PathBuf::from(env!("CARGO_BIN_EXE_omniagent"))),
    );
    let outcome = tokio::time::timeout(Duration::from_secs(55), async {
        if let Some((expected_tool, decision)) = approval {
            let mut cursor = 0;
            'active: loop {
                for event in store
                    .agent_observations(conversation.id.clone(), run.clone(), cursor)
                    .await?
                {
                    cursor = event.seq;
                    if matches!(
                        event.event.data,
                        ProviderEventData::TurnFinished { .. } | ProviderEventData::Error { .. }
                    ) {
                        break 'active;
                    }
                    if let ProviderEventData::Approval {
                        request_id,
                        operation,
                        description,
                        ..
                    } = event.event.data
                    {
                        ensure!(
                            operation == expected_tool
                                || description
                                    .as_ref()
                                    .is_some_and(|value| value.contains(expected_tool)),
                            "unexpected fixture request {operation:?} {description:?}"
                        );
                        running.approve(request_id, decision).await?;
                    }
                }
                tokio::time::sleep(Duration::from_millis(75)).await;
            }
        }
        running.wait().await
    })
    .await;
    let status = match outcome {
        Ok(status) => status?,
        Err(error) => {
            let events = store
                .agent_observations(conversation.id.clone(), run.clone(), 0)
                .await?;
            let relevant: Vec<_> = events
                .iter()
                .filter_map(|event| match &event.event.data {
                    ProviderEventData::Approval { .. }
                    | ProviderEventData::Error { .. }
                    | ProviderEventData::BridgeRegistration { .. } => Some(&event.event.data),
                    _ => None,
                })
                .collect();
            return Err(anyhow::anyhow!("{error}: {relevant:?}"));
        }
    };
    Ok((run, status))
}

async fn verify_bridge(provider: Provider, model: &str) -> Result<()> {
    let fixture = Fixture::new()?;
    let (store, checkout) = fixture.register().await?;
    let mut scope = scope(provider, model, &fixture.path)?;
    let approval = if provider == Provider::Antigravity {
        scope.mode = SessionMode::ApprovedWrite;
        Some(("inspect_run_scope", ApprovalDecision::AllowOnce))
    } else {
        None
    };
    let conversation = store
        .create_conversation(checkout.checkout_id, "MCP run binding".into())
        .await?;
    for resumed in [false, true] {
        let (run, status) = turn_with_approval(&store, &conversation, scope.clone(),
            "Use the inspect_run_scope tool from the omniagent_runtime MCP server (mcp__omniagent_runtime__inspect_run_scope) with no arguments. Return only the exact runId from the result. Call it afresh on this turn because the runId changes on resume.", approval).await?;
        let events = store
            .agent_observations(conversation.id.clone(), run.clone(), 0)
            .await?;
        let registrations = events
            .iter()
            .filter_map(|event| match &event.event.data {
                ProviderEventData::BridgeRegistration { registration } => Some(registration),
                _ => None,
            })
            .collect::<Vec<_>>();
        if status != RunStatus::Succeeded || !registrations.contains(&&Registration::ScopeVerified)
        {
            let evidence = events
                .iter()
                .filter_map(|event| match &event.event.data {
                    ProviderEventData::AssistantMessage { text }
                    | ProviderEventData::Diagnostic { message: text }
                    | ProviderEventData::Error { message: text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            anyhow::bail!(
                "{provider:?} run {run} (resume={resumed}): {status:?}, {registrations:?}, {evidence:?}"
            );
        }
        let messages = store.conversation_messages(conversation.id.clone()).await?;
        let response = &messages.last().context("missing bridge response")?.text;
        assert!(
            response.contains(&run),
            "provider returned {response:?} for {run}"
        );
        let dispatch = store
            .agent_dispatch(conversation.id.clone(), run)
            .await?
            .context("missing dispatch")?;
        assert_eq!(
            matches!(
                dispatch.snapshot.native_history,
                NativeHistory::ProviderCursor { .. }
            ),
            resumed
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Codex App Server with gpt-6-luna"]
async fn codex_calls_its_run_bridge_on_start_and_resume() -> Result<()> {
    verify_bridge(Provider::Codex, "gpt-6-luna").await
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code with Haiku 5.5"]
async fn claude_calls_its_run_bridge_on_start_and_resume() -> Result<()> {
    verify_bridge(Provider::Claude, "claude-haiku-5-5").await
}

#[tokio::test]
#[ignore = "requires authenticated Antigravity with Gemini 3.8 Flash"]
async fn antigravity_calls_its_run_bridge_on_start_and_resume() -> Result<()> {
    verify_bridge(Provider::Antigravity, "gemini-3.8-flash").await
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code with Haiku 5.5"]
async fn claude_routes_write_permission_through_the_mcp_prompt_tool() -> Result<()> {
    let fixture = Fixture::new()?;
    let (store, checkout) = fixture.register().await?;
    let mut scope = scope(Provider::Claude, "claude-haiku-5-5", &fixture.path)?;
    scope.mode = SessionMode::ApprovedWrite;
    let conversation = store
        .create_conversation(checkout.checkout_id, "Claude permission prompt".into())
        .await?;
    for (file, decision) in [
        ("denied.txt", ApprovalDecision::Deny),
        ("allowed.txt", ApprovalDecision::AllowOnce),
    ] {
        let (run, status) = turn_with_approval(&store, &conversation, scope.clone(),
            &format!("Use only the Write tool exactly once to create {file} in the current directory with text permission-fixture. Stop if denied."),
            Some(("Write", decision))).await?;
        let events = store
            .agent_observations(conversation.id.clone(), run, 0)
            .await?;
        assert!(events.iter().any(|event| matches!(&event.event.data, ProviderEventData::Approval { operation, prompt: Some(_), .. } if operation == "Write")), "Claude did not call the MCP permission prompt");
        assert_eq!(status, RunStatus::Succeeded);
        assert_eq!(
            fixture.path.join(file).is_file(),
            decision == ApprovalDecision::AllowOnce
        );
    }
    Ok(())
}

async fn verify_answer(store: &Store, conversation: &Conversation, marker: &str) -> Result<()> {
    let messages = store.conversation_messages(conversation.id.clone()).await?;
    let text = &messages.last().context("empty fixture conversation")?.text;
    ensure!(
        text.trim() == marker,
        "provider did not receive the projected instructions"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code with the restricted print profile"]
async fn claude_loads_only_the_approved_launch_entries_and_projects_instructions() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::create_dir(fixture.path.join(".claude"))?;
    fixture.doc("CLAUDE.md", "OMNI_CLAUDE_PROJECTED")?;
    fs::write(
        fixture.path.join(".claude/settings.json"),
        r#"{"hooks":{
        "SessionStart":[
            {"matcher":"startup","hooks":[{"type":"command","command":"powershell.exe -NoProfile -File ./hook-approved.ps1"}]},
            {"matcher":"startup","hooks":[{"type":"command","command":"powershell.exe -NoProfile -File ./hook-pending.ps1"}]}
        ]
    }}"#,
    )?;
    fs::write(
        fixture.path.join("hook-approved.ps1"),
        "Set-Content approved-hook started",
    )?;
    fs::write(
        fixture.path.join("hook-pending.ps1"),
        "Set-Content pending-hook started",
    )?;
    fs::write(
        fixture.path.join(".mcp.json"),
        r#"{"mcpServers":{
        "approved":{"command":"node","args":["./approved.js"]},
        "pending":{"command":"node","args":["./pending.js"]}
    }}"#,
    )?;
    let (store, checkout) = fixture.register().await?;
    let inventory = store
        .saved_checkout_customizations(checkout.checkout_id.clone())
        .await?;
    let first_hook = inventory
        .entries
        .iter()
        .find(|entry| {
            entry.kind == CustomizationKind::Hook
                && entry
                    .arguments
                    .iter()
                    .any(|arg| arg.contains("hook-approved"))
        })
        .context("fixture hook missing")?;
    let inventory = store
        .approve_checkout_customization(
            checkout.checkout_id.clone(),
            first_hook.id.clone(),
            first_hook.fingerprint.clone(),
        )
        .await?;
    approve(
        &store,
        &checkout,
        inventory,
        "approved",
        CustomizationKind::McpServer,
    )
    .await?;
    let session = scope(Provider::Claude, "claude-haiku-5-5", &fixture.path)?;
    let conversation = store
        .create_conversation(checkout.checkout_id, "Scoped Claude".into())
        .await?;
    let (run, status) = turn(&store, &conversation, session.clone()).await?;
    assert_eq!(status, RunStatus::Succeeded);
    verify_answer(&store, &conversation, "OMNI_CLAUDE_PROJECTED").await?;
    assert!(fixture.path.join("approved-hook").is_file());
    assert!(fixture.path.join("approved-mcp").is_file());
    assert!(!fixture.path.join("pending-hook").exists());
    assert!(!fixture.path.join("pending-mcp").exists());
    let observed = store
        .agent_observations(conversation.id.clone(), run.clone(), 0)
        .await?;
    assert!(observed.iter().any(
        |entry| matches!(&entry.event.data, ProviderEventData::HookObserved { trigger, status, .. }
        if trigger == "SessionStart" && status == "success")
    ));
    let dispatch = store
        .agent_dispatch(conversation.id.clone(), run)
        .await?
        .unwrap();
    let evidence = dispatch.snapshot.scoped_launch.unwrap();
    assert_eq!(evidence.approved.len(), 2);
    assert!(
        evidence
            .inputs
            .iter()
            .any(|input| input.name == "instructions.md"
                && input.contents.contains("OMNI_CLAUDE_PROJECTED"))
    );

    fs::write(
        fixture.path.join("approved.js"),
        "require('node:fs').writeFileSync('stale-mcp','started');process.exit(0);",
    )?;
    fixture.doc("CLAUDE.md", "OMNI_CLAUDE_PROJECTED_AFTER_RESUME")?;
    let (run, status) = turn(&store, &conversation, session).await?;
    assert_eq!(status, RunStatus::Succeeded);
    verify_answer(&store, &conversation, "OMNI_CLAUDE_PROJECTED_AFTER_RESUME").await?;
    assert!(!fixture.path.join("stale-mcp").exists());
    let after = store
        .agent_dispatch(conversation.id.clone(), run)
        .await?
        .unwrap();
    assert!(matches!(
        after.snapshot.native_history,
        NativeHistory::ProviderCursor { .. }
    ));
    assert_eq!(after.snapshot.scoped_launch.unwrap().approved.len(), 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Codex App Server with gpt-6-luna"]
async fn codex_ignores_implicit_project_servers_and_uses_only_projected_entries() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::create_dir(fixture.path.join(".codex"))?;
    fixture.doc("CODEX.md", "OMNI_CODEX_PROJECTED")?;
    fs::write(
        fixture.path.join(".codex/config.toml"),
        r#"
project_doc_fallback_filenames = ["CODEX.md"]
[mcp_servers.approved]
command = "node"
args = ["./approved.js"]
[mcp_servers.pending]
command = "node"
args = ["./pending.js"]
"#,
    )?;
    let (store, checkout) = fixture.register().await?;
    let inventory = store
        .saved_checkout_customizations(checkout.checkout_id.clone())
        .await?;
    approve(
        &store,
        &checkout,
        inventory,
        "approved",
        CustomizationKind::McpServer,
    )
    .await?;
    let session = scope(Provider::Codex, "gpt-6-luna", &fixture.path)?;
    let conversation = store
        .create_conversation(checkout.checkout_id, "Scoped Codex".into())
        .await?;
    let (run, status) = turn(&store, &conversation, session).await?;
    assert_eq!(status, RunStatus::Succeeded);
    assert!(fixture.path.join("approved-mcp").is_file());
    assert!(!fixture.path.join("pending-mcp").exists());
    verify_answer(&store, &conversation, "OMNI_CODEX_PROJECTED").await?;
    let dispatch = store
        .agent_dispatch(conversation.id.clone(), run)
        .await?
        .unwrap();
    assert_eq!(dispatch.snapshot.scoped_launch.unwrap().approved.len(), 1);
    Ok(())
}

#[tokio::test]
async fn incompatible_acp_profile_stops_before_creating_a_provider_process() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::create_dir(fixture.path.join(".agents"))?;
    fs::write(
        fixture.path.join(".agents/mcp_config.json"),
        r#"{"mcpServers":{
        "pending":{"command":"node","args":["./pending.js"]}
    }}"#,
    )?;
    let (store, checkout) = fixture.register().await?;
    let conversation = store
        .create_conversation(checkout.checkout_id, "Rejected startup".into())
        .await?;
    let session = SessionScope {
        provider: Provider::Antigravity,
        profile_id: "fixture".into(),
        account_binding: None,
        executable: fixture.path.join("invalid.exe"),
        checkout: fixture.path.clone(),
        cli_version: "1.5.9".into(),
        runner: "windows-direct".into(),
        permission_revision: "fixture".into(),
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    };
    let (run, status) = turn(&store, &conversation, session).await?;
    assert_eq!(status, RunStatus::Failed);
    let events = store.agent_observations(conversation.id, run, 0).await?;
    assert!(events.iter().any(
        |event| matches!(&event.event.data, ProviderEventData::Error { message }
        if message.contains("cannot enforce"))
    ));
    assert!(!fixture.path.join("pending-mcp").exists());
    Ok(())
}
