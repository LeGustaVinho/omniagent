use std::{fs, process::Command};

use serde_json::json;

use super::*;

const FIXTURE_ROOT: &str = "OMNIAGENT_DISPOSABLE_NATIVE_PLUGINS";

#[test]
fn validates_exact_plugin_tokens_and_rejects_path_like_names() {
    for invalid in [
        "--help",
        "../demo",
        "demo@marketplace",
        "demo/extra",
        "",
        "demo name",
    ] {
        assert!(exact_name(invalid).is_err(), "{invalid}");
    }
    exact_name("fixture-plugin").unwrap();
}

#[test]
#[ignore = "requires installed agy with local native plugin management"]
fn antigravity_mutates_only_its_disposable_user_plugin_scope() -> Result<()> {
    run_disposable(
        "antigravity_mutates_only_its_disposable_user_plugin_scope",
        exercise_antigravity,
    )
}

#[test]
#[ignore = "requires installed Codex with native local marketplace support"]
fn codex_installs_only_the_reviewed_local_marketplace_candidate() -> Result<()> {
    run_disposable(
        "codex_installs_only_the_reviewed_local_marketplace_candidate",
        exercise_codex,
    )
}

fn run_disposable<F, Fut>(name: &str, exercise: F) -> Result<()>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if let Some(root) = env::var_os(FIXTURE_ROOT) {
        let root = PathBuf::from(root);
        ensure!(
            root.join("disposable-marker").is_file()
                && env::var_os("USERPROFILE").map(PathBuf::from) == Some(root.join("user"))
                && env::var_os("CODEX_HOME").map(PathBuf::from) == Some(root.join("codex")),
            "The plugin fixture requires isolated CLI profiles"
        );
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(exercise(root));
    }
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    let user = root.join("user");
    let codex = root.join("codex");
    let alternate = root.join("alternate-environment");
    fs::create_dir(&user)?;
    fs::create_dir(&codex)?;
    fs::create_dir(&alternate)?;
    fs::write(
        codex.join("config.toml"),
        "chatgpt_base_url = 'http://127.0.0.1:9/backend-api/'\nweb_search = 'disabled'\n",
    )?;
    fs::write(root.join("disposable-marker"), b"local plugin fixture")?;
    let module = module_path!()
        .strip_prefix("omniagent::")
        .unwrap_or(module_path!());
    let selector = format!("{module}::{name}");
    let result = Command::new(env::current_exe()?)
        .args(["--ignored", "--exact", &selector, "--nocapture"])
        .env(FIXTURE_ROOT, &root)
        .env("HOME", &alternate)
        .env("USERPROFILE", &user)
        .env("APPDATA", &alternate)
        .env("LOCALAPPDATA", &alternate)
        .env("CODEX_HOME", &codex)
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_APPLICATION_CREDENTIALS")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .output()?;
    let stdout = String::from_utf8_lossy(&result.stdout);
    ensure!(
        result.status.success() && stdout.contains("1 passed"),
        "Disposable native plugin fixture failed: {stdout}\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

async fn exercise_antigravity(root: PathBuf) -> Result<()> {
    const PLUGIN: &str = "omniagent-local-agy";
    let path = root.join("native-plugins.sqlite");
    let store = Store::open(path.clone())?;
    let configured = store.configured_executable(Provider::Antigravity).await?;
    let cli = LocalCli::qualify(configured)?;
    assert!(cli.installed()?.plugins.is_empty());
    drop(cli);
    let source = root.join("source");
    fs::create_dir(&source)?;
    fs::write(
        source.join("plugin.json"),
        json!({ "name": PLUGIN, "version": "1.0.0", "description": "Disposable local plugin" })
            .to_string(),
    )?;
    let text = source.join("README.md");
    fs::write(&text, "Initial review")?;
    let install = || NativeUserPluginRequest {
        provider: Provider::Antigravity,
        action: UserPluginAction::Install,
        target: source.to_string_lossy().into_owned(),
    };
    let old = store.preview_native_user_plugin_action(install()).await?;
    fs::write(&text, "Reviewed revision two")?;
    assert!(matches!(
        store.apply_native_user_plugin_action(old.id).await?,
        ClaudeUserPluginOutcome::Stale { .. }
    ));
    let preview = store.preview_native_user_plugin_action(install()).await?;
    let user_config = root.join("user/.gemini/config");
    assert_eq!(preview.configuration_root, user_config);
    assert_eq!(preview.plugin_id, PLUGIN);
    assert!(preview.inventory_before.report.plugins.is_empty());
    let installed = store
        .apply_native_user_plugin_action(preview.id.clone())
        .await?;
    assert!(
        matches!(&installed, ClaudeUserPluginOutcome::Applied { inventory_after }
        if installed_target(&inventory_after.report, PLUGIN)?.unwrap().enabled == Some(true)),
        "{installed:?}"
    );
    let staged = user_config.join("plugins").join(PLUGIN);
    assert!(
        !root
            .join("alternate-environment/.gemini/config/plugins")
            .exists()
    );
    assert_eq!(
        fs::read_to_string(staged.join("README.md"))?,
        "Reviewed revision two"
    );
    let toggle = |action| NativeUserPluginRequest {
        provider: Provider::Antigravity,
        action,
        target: PLUGIN.into(),
    };
    let disabled = store
        .preview_native_user_plugin_action(toggle(UserPluginAction::Disable))
        .await?;
    let disabled = store.apply_native_user_plugin_action(disabled.id).await?;
    assert!(
        matches!(&disabled, ClaudeUserPluginOutcome::Applied { inventory_after }
        if installed_target(&inventory_after.report, PLUGIN)?.unwrap().enabled == Some(false)),
        "{disabled:?}"
    );
    let enabled = store
        .preview_native_user_plugin_action(toggle(UserPluginAction::Enable))
        .await?;
    let enabled = store.apply_native_user_plugin_action(enabled.id).await?;
    assert!(
        matches!(&enabled, ClaudeUserPluginOutcome::Applied { inventory_after }
        if installed_target(&inventory_after.report, PLUGIN)?.unwrap().enabled == Some(true)),
        "{enabled:?}"
    );
    drop(store);
    let reopened = Store::open(path)?;
    assert_eq!(
        reopened.apply_native_user_plugin_action(preview.id).await?,
        installed
    );
    assert_eq!(
        reopened
            .latest_plugin_inventory(Provider::Antigravity)
            .await?,
        enabled.inventory_after().cloned()
    );
    Ok(())
}

async fn exercise_codex(root: PathBuf) -> Result<()> {
    const MARKET: &str = "omniagent-local-market";
    const PLUGIN: &str = "formatter@omniagent-local-market";
    let market = root.join("market");
    fs::create_dir_all(market.join(".agents/plugins"))?;
    let plugin = market.join("plugins/formatter");
    fs::create_dir_all(plugin.join(".codex-plugin"))?;
    fs::write(
        market.join(".agents/plugins/marketplace.json"),
        json!({ "name": MARKET, "interface": { "displayName": "Local fixture" },
                "plugins": [{ "name": "formatter", "source": { "source": "local", "path": "./plugins/formatter" } }] }).to_string(),
    )?;
    fs::write(
        plugin.join(".codex-plugin/plugin.json"),
        json!({ "name": "formatter", "version": "1.0.0", "description": "Local fixture" })
            .to_string(),
    )?;
    let store = Store::open(root.join("codex-plugins.sqlite"))?;
    let cli = LocalCli::qualify(store.configured_executable(Provider::Codex).await?)?;
    ensure!(
        cli.probe(
            &[
                "plugin",
                "marketplace",
                "add",
                market.to_str().unwrap(),
                "--json"
            ],
            READ_TIMEOUT
        )?
        .status
            == TerminalOutcome::Exited(0),
        "Could not seed the disposable Codex marketplace"
    );
    drop(cli);
    let entries = store.native_codex_plugin_catalog(MARKET.into()).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].plugin_id, PLUGIN);
    assert_eq!(entries[0].source, plugin);
    let request = |action| NativeUserPluginRequest {
        provider: Provider::Codex,
        action,
        target: PLUGIN.into(),
    };
    let preview = store
        .preview_native_user_plugin_action(request(UserPluginAction::Install))
        .await?;
    assert_eq!(preview.configuration_root, root.join("codex"));
    assert_eq!(preview.source, plugin);
    assert!(
        store
            .preview_native_user_plugin_action(request(UserPluginAction::Disable))
            .await
            .is_err()
    );
    let applied = store
        .apply_native_user_plugin_action(preview.id.clone())
        .await?;
    assert!(
        matches!(&applied, ClaudeUserPluginOutcome::Applied { inventory_after }
        if installed_target(&inventory_after.report, PLUGIN)?.unwrap().enabled == Some(true)),
        "{applied:?}"
    );
    assert!(
        store
            .native_codex_plugin_catalog(MARKET.into())
            .await?
            .is_empty()
    );
    assert_eq!(
        store.apply_native_user_plugin_action(preview.id).await?,
        applied
    );
    Ok(())
}
