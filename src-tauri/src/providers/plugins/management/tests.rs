use super::*;
use serde_json::json;
use std::{fs, process::Command};

#[test]
fn management_accepts_only_exact_marketplace_names() -> Result<()> {
    assert_eq!(
        plugin_selector("sample-plugin@local-market")?,
        ("sample-plugin", "local-market")
    );
    for invalid in [
        "--all",
        "--npm@local",
        "plugin",
        "@market",
        "../path@market",
        "name@https://example.test",
        "plugin@market@other",
        "plugin@npm",
        "plugin@synced",
        "plugin@skills-dir",
        "plugin@market --yes",
        "plugin@market\n",
    ] {
        assert!(plugin_selector(invalid).is_err(), "{invalid}");
    }
    let exact = json!({"pluginId":"sample@local","marketplaceName":"local","version":"1.0","source":"./plugin"});
    let original = catalog_entry(&json!({"available":[exact.clone()]}), "sample@local")?;
    let mut moved = exact.clone();
    moved["source"] = "./other".into();
    assert_ne!(
        catalog_entry(&json!({"available":[moved]}), "sample@local")?,
        original
    );
    assert!(catalog_entry(&json!({"available":[exact.clone(), exact]}), "sample@local").is_err());
    let helper = json!({"pluginId":"sample@local","marketplaceName":"local","version":"1.0",
        "source":{"source":"archive","url":"https://example.invalid/test.zip","headersHelper":{"command":"unknown"}}});
    assert!(catalog_entry(&json!({"available":[helper]}), "sample@local")?.requires_confirmation);
    Ok(())
}

#[test]
fn command_confirmation_returns_only_a_fingerprint() -> Result<()> {
    let request = ClaudeUserPluginRequest {
        action: ClaudeUserPluginAction::Install,
        plugin_id: "sample@local".into(),
    };
    let response = json!({"success":false,"shownCommand":{"command":"never dispatch this payload", "sha256":"a".repeat(64)}});
    let outcome = classify_result(
        CommandOutput {
            status: TerminalOutcome::Exited(1),
            stdout: response.to_string(),
            stderr: String::new(),
        },
        &request,
        &fixture_evaluation(),
        None,
    )
    .with_inventory(None);
    let encoded = serde_json::to_string(&outcome)?;
    assert!(
        matches!(outcome, ClaudeUserPluginOutcome::NeedsConfirmation { command_fingerprint: Some(hash), .. } if hash == "a".repeat(64))
    );
    assert!(!encoded.contains("never dispatch this payload"));
    let mut receipt =
        json!({"command":"install","outcome":"ok","pluginId":"sample@local","scope":"user"});
    assert!(native_success(&receipt, &request));
    receipt["scope"] = "project".into();
    assert!(!native_success(&receipt, &request));
    Ok(())
}

fn fixture_evaluation() -> Evaluation {
    Evaluation {
        profile: EffectiveProfile {
            executable: PathBuf::from("claude.exe"),
            executable_revision: "test-cli-digest".into(),
            cli_version: "fixture".into(),
            configuration_root: PathBuf::from(".claude"),
            configuration_identity: "test-root".into(),
        },
        report: PluginReport {
            provider: Provider::Claude,
            executable: None,
            version: None,
            status: PluginInventoryStatus::Reported,
            plugins: Vec::new(),
            load_telemetry_reported: false,
        },
        config_revision: [None, None],
        installed: None,
        catalog: None,
    }
}

#[test]
#[ignore = "requires Claude Code with scoped JSON plugin commands"]
fn claude_plugin_management_operates_only_on_a_disposable_user_profile() -> Result<()> {
    const FIXTURE_ROOT: &str = "OMNIAGENT_DISPOSABLE_CLAUDE_PLUGINS";
    if let Some(root) = env::var_os(FIXTURE_ROOT) {
        let root = PathBuf::from(root);
        ensure!(
            root.join("fixture-marker").is_file() && config_root()? == root.join("profile"),
            "Native plugin fixture requires its own disposable profile"
        );
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(exercise_native(&root));
    }
    let directory = tempfile::Builder::new()
        .prefix("omniagent-native-claude-test-")
        .tempdir()?;
    let root = dunce::canonicalize(directory.path())?;
    fs::create_dir(root.join("profile"))?;
    fs::write(root.join("fixture-marker"), b"isolated")?;
    let name = format!(
        "{}::claude_plugin_management_operates_only_on_a_disposable_user_profile",
        module_path!()
    );
    let name = name.strip_prefix("omniagent::").unwrap_or(&name);
    let child = Command::new(env::current_exe()?)
        .args(["--ignored", "--exact", name, "--nocapture"])
        .env(FIXTURE_ROOT, &root)
        .env("CLAUDE_CONFIG_DIR", root.join("profile"))
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .output()?;
    let stdout = String::from_utf8_lossy(&child.stdout);
    ensure!(
        child.status.success() && stdout.contains("1 passed"),
        "Disposable Claude fixture failed: {stdout}\n{}",
        String::from_utf8_lossy(&child.stderr)
    );
    Ok(())
}

async fn exercise_native(root: &Path) -> Result<()> {
    let marketplace = root.join("marketplace");
    fs::create_dir_all(marketplace.join(".claude-plugin"))?;
    let package = marketplace.join("fixture-plugin/.claude-plugin");
    fs::create_dir_all(&package)?;
    fs::write(
        package.join("plugin.json"),
        json!({
            "name":"fixture-plugin", "version":"1.0.0", "description":"A disposable native test"
        })
        .to_string(),
    )?;
    let helper_marker = root.join("helper-ran.txt");
    let unused_helper = format!("echo unexpected > \"{}\"", helper_marker.display());
    fs::write(
        marketplace.join(".claude-plugin/marketplace.json"),
        json!({
            "name":"omniagent-fixture", "owner":{"name":"OmniAgent test"},
            "plugins":[
                {"name":"fixture-plugin","version":"1.0.0","source":"./fixture-plugin"},
                {"name":"command-plugin","version":"1.0.0","source":{"source":"command","command":unused_helper}}
            ]
        })
        .to_string(),
    )?;
    let executable = dunce::canonicalize(Provider::Claude.adapter().discover(None)?)?;
    let added = run_plugin_command(
        &executable,
        root,
        &[
            "plugin",
            "marketplace",
            "add",
            marketplace.to_str().context("Fixture path unavailable")?,
            "--json",
        ],
        READ_TIMEOUT,
    )?;
    ensure!(
        added.status == TerminalOutcome::Exited(0),
        "Could not seed the disposable local marketplace"
    );
    let database = root.join("plugins.sqlite");
    let store = Store::open(database.clone())?;
    let helper = store
        .preview_claude_user_plugin_action(ClaudeUserPluginRequest {
            action: ClaudeUserPluginAction::Install,
            plugin_id: "command-plugin@omniagent-fixture".into(),
        })
        .await?;
    let skipped = store.apply_claude_user_plugin_action(helper.id).await?;
    assert!(
        matches!(&skipped, ClaudeUserPluginOutcome::NeedsConfirmation { inventory_after: Some(inventory), .. }
        if inventory.report.plugins.is_empty()),
        "{skipped:?}"
    );
    assert!(!helper_marker.exists());
    let selector = "fixture-plugin@omniagent-fixture";
    let request = |action| ClaudeUserPluginRequest {
        action,
        plugin_id: selector.into(),
    };
    let preview = store
        .preview_claude_user_plugin_action(request(ClaudeUserPluginAction::Install))
        .await?;
    assert!(preview.inventory_before.report.plugins.is_empty());
    assert_eq!(preview.configuration_root, root.join("profile"));
    assert_eq!(preview.execution_scope, USER_SCOPE);
    assert!(preview.persists);
    let installed = store
        .apply_claude_user_plugin_action(preview.id.clone())
        .await?;
    assert!(
        matches!(&installed, ClaudeUserPluginOutcome::Applied { inventory_after }
        if installed_target(&inventory_after.report, selector)?.unwrap().enabled == Some(true)),
        "{installed:?}"
    );
    let pending = store
        .preview_claude_user_plugin_action(request(ClaudeUserPluginAction::Disable))
        .await?;
    let setting_path = root.join("profile/settings.json");
    let mut settings: Value = serde_json::from_slice(&fs::read(&setting_path)?)?;
    settings["showTurnDuration"] = false.into();
    fs::write(&setting_path, serde_json::to_vec(&settings)?)?;
    let stale = store.apply_claude_user_plugin_action(pending.id).await?;
    assert!(
        matches!(stale, ClaudeUserPluginOutcome::Stale { .. }),
        "{stale:?}"
    );
    let pending = store
        .preview_claude_user_plugin_action(request(ClaudeUserPluginAction::Disable))
        .await?;
    let profile = root.join("profile");
    let parked = root.join("previous-profile");
    fs::rename(&profile, &parked)?;
    fs::create_dir(&profile)?;
    let replaced = store.apply_claude_user_plugin_action(pending.id).await?;
    fs::rename(&profile, root.join("replacement-profile"))?;
    fs::rename(&parked, &profile)?;
    assert!(
        matches!(replaced, ClaudeUserPluginOutcome::Stale { reason, .. } if reason.contains("root changed"))
    );

    let preview_disable = store
        .preview_claude_user_plugin_action(request(ClaudeUserPluginAction::Disable))
        .await?;
    let disabled = store
        .apply_claude_user_plugin_action(preview_disable.id.clone())
        .await?;
    assert!(
        matches!(&disabled, ClaudeUserPluginOutcome::Applied { inventory_after }
        if installed_target(&inventory_after.report, selector)?.unwrap().enabled == Some(false)),
        "{disabled:?}"
    );
    let preview_enable = store
        .preview_claude_user_plugin_action(request(ClaudeUserPluginAction::Enable))
        .await?;
    let enabled = store
        .apply_claude_user_plugin_action(preview_enable.id)
        .await?;
    assert!(
        matches!(&enabled, ClaudeUserPluginOutcome::Applied { inventory_after }
        if installed_target(&inventory_after.report, selector)?.unwrap().enabled == Some(true)),
        "{enabled:?}"
    );
    let pending = store
        .preview_claude_user_plugin_action(request(ClaudeUserPluginAction::Disable))
        .await?;
    let pending_id = pending.id.clone();
    let unknown = ClaudeUserPluginOutcome::Unconfirmed {
        reason: "Fixture process interrupted".into(),
        inventory_after: None,
    };
    store.execute({
        let unknown = unknown.clone();
        move |db| {
            db.execute("UPDATE claude_user_plugin_actions SET state='applying', outcome=?2 WHERE id=?1",
                params![pending_id, protected(&unknown)?])?;
            Ok(())
        }
    }).await?;
    drop(store);
    let store = Store::open(database)?;
    assert_eq!(
        store.apply_claude_user_plugin_action(pending.id).await?,
        unknown
    );
    assert_eq!(
        store
            .apply_claude_user_plugin_action(preview_disable.id)
            .await?,
        disabled
    );
    assert_eq!(
        store.apply_claude_user_plugin_action(preview.id).await?,
        installed
    );
    let latest = store
        .latest_plugin_inventory(Provider::Claude)
        .await?
        .unwrap();
    assert_eq!(
        installed_target(&latest.report, selector)?.unwrap().enabled,
        Some(true)
    );
    let historical = store
        .execute(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM plugin_inventories WHERE id=?1",
                [&preview.inventory_before.id],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await?;
    assert_eq!(historical, 1);
    Ok(())
}
