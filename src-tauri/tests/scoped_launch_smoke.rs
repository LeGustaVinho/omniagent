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
    providers::{
        Provider, ProviderEventData, SessionMode,
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
    let current = store.conversation(conversation.id.clone()).await?;
    let attempt = store.prepare_agent_attempt(current.id, current.context_id, scope,
        "Instruction audit. Follow the repository instruction context. Do not read files or use tools.".into()).await?;
    let run = attempt.run.id.clone();
    let status = tokio::time::timeout(
        Duration::from_secs(55),
        start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), attempt).wait(),
    )
    .await??;
    Ok((run, status))
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
