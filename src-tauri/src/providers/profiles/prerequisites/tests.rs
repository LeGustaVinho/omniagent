use super::*;

#[test]
fn git_prerequisite_only_keeps_the_reported_version() {
    assert_eq!(
        git_version("\r\ngit version 2.55.0.windows.3\r\n"),
        Some("2.55.0.windows.3".into())
    );
    assert!(git_version("git version 2.55.0.windows.3 unexpected output").is_none());
    assert!(git_version("only stdout from another process").is_none());
}

#[tokio::test]
async fn an_unusable_saved_provider_override_stays_visible_without_path_fallback() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let store = Store::open(folder.path().join("profile.sqlite"))?;
    let claimed = folder.path().join("not-a-cli.exe");
    std::fs::write(&claimed, b"fixture without an executable image")?;
    let saved = store
        .set_cli_executable(Provider::Codex, Some(claimed.clone()))
        .await?;
    let status = inspect_provider(saved.clone());
    assert_eq!(status.configured_executable, saved.executable);
    assert_eq!(
        status.status.executable,
        Some(dunce::canonicalize(&claimed)?)
    );
    assert!(status.status.version.is_none());
    assert!(status.status.issue.is_some());
    std::fs::remove_file(claimed)?;
    let missing = inspect_provider(store.configured_executable(Provider::Codex).await?);
    assert_eq!(missing.configured_executable, saved.executable);
    assert!(missing.status.executable.is_none());
    assert!(missing.status.issue.is_some());
    Ok(())
}

#[tokio::test]
#[ignore = "requires Git for Windows, ripgrep, Codex, Claude Code and agy on PATH"]
async fn real_desktop_prerequisites_resolve_overrides_and_register_a_disposable_repository()
-> Result<()> {
    let folder = tempfile::tempdir()?;
    let database = folder.path().join("profile.sqlite");
    let store = Store::open(database.clone())?;
    let defaults = store.desktop_cli_prerequisites().await?;
    assert!(defaults.git.issue.is_none());
    let git = defaults
        .git
        .executable
        .as_deref()
        .expect("Git needs an executable");
    assert!(
        defaults
            .git
            .version
            .as_deref()
            .is_some_and(|version| version.contains(".windows."))
    );
    assert_eq!(defaults.providers.len(), Provider::ALL.len());
    for entry in &defaults.providers {
        assert!(entry.configured_executable.is_none());
        assert!(entry.status.version.is_some(), "{entry:?}");
        let executable = entry.status.executable.as_ref().expect("Provider path");
        store
            .set_cli_executable(entry.provider, Some(executable.clone()))
            .await?;
        assert!(
            store
                .set_cli_executable(entry.provider, Some(folder.path().join("missing.exe")))
                .await
                .is_err()
        );
    }
    let rg = store.ripgrep_status().await?;
    assert!(
        rg.configured_executable.is_none() && rg.version.is_some(),
        "{rg:?}"
    );
    store.set_ripgrep_executable(rg.executable.clone()).await?;

    let repository = folder.path().join("Repositório descartável");
    let nested = repository.join("docs");
    std::fs::create_dir_all(&nested)?;
    assert_eq!(
        command_probe(git, &repository, vec!["init".into(), "--quiet".into()])?.status,
        TerminalOutcome::Exited(0)
    );
    let registered = crate::repository::register(&store, nested).await?;
    assert_eq!(
        Path::new(&registered.checkout_root),
        dunce::canonicalize(&repository)?
    );
    store
        .select_active_checkout(registered.checkout_id.clone())
        .await?;
    drop(store);

    let reopened = Store::open(database)?;
    assert_eq!(reopened.active_checkout().await?, Some(registered));
    let configured = reopened.desktop_cli_prerequisites().await?;
    for (manual, automatic) in configured.providers.iter().zip(&defaults.providers) {
        assert_eq!(
            manual.configured_executable.as_deref(),
            automatic
                .status
                .executable
                .as_deref()
                .and_then(Path::to_str)
        );
        assert_eq!(manual.status, automatic.status);
        reopened.set_cli_executable(manual.provider, None).await?;
    }
    let configured_rg = reopened.ripgrep_status().await?;
    assert_eq!(configured_rg.configured_executable, rg.executable);
    assert_eq!(configured_rg.version, rg.version);
    assert!(
        reopened
            .set_ripgrep_executable(None)
            .await?
            .configured_executable
            .is_none()
    );
    assert_eq!(reopened.desktop_cli_prerequisites().await?, defaults);
    Ok(())
}
