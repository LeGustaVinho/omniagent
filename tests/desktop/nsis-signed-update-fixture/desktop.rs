use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use omniagent::{
    installation::InstallationIdentity,
    jobs::JobScheduler,
    maintenance_update::{UpdateActivationReceipt, UpdateJournal, VerifiedMaintenanceUpdate},
    restore::InstallationLayout,
    store::Store,
    workflows::maintenance::MaintenanceActivationRequest,
};

pub fn verify_identifier(identifier: &str) -> Result<()> {
    let expected = env!("OMNIAGENT_SIGNED_FIXTURE_ID");
    let tag = expected
        .strip_prefix("io.github.legustavinho.omniagent.nsis-update-test-")
        .context("Expected a disposable updater fixture identifier")?;
    ensure!(
        identifier == expected && tag.len() == 10 && tag.bytes().all(|b| b.is_ascii_hexdigit()),
        "Wrong installed product for the updater fixture"
    );
    Ok(())
}

pub fn after_sqlite(identifier: &str, root: &Path, store: &Store) -> Result<()> {
    verify_identifier(identifier)?;
    ensure!(
        root.file_name().is_some_and(|name| name == identifier),
        "Unexpected fixture data root"
    );
    if env!("CARGO_PKG_VERSION") == "0.1.2" && root.join("fixture-fail-startup.flag").is_file() {
        tauri::async_runtime::block_on(
            omniagent::maintenance_update::dirty_signed_fixture_migration(store),
        )?;
        std::fs::write(
            root.join("fixture-startup-failed.txt"),
            env!("CARGO_PKG_VERSION"),
        )?;
        anyhow::bail!("The disposable fixture failed after SQLite startup");
    }
    Ok(())
}

#[tauri::command]
pub async fn signed_fixture_activate(
    app: tauri::AppHandle,
    store: tauri::State<'_, Store>,
    identity: tauri::State<'_, InstallationIdentity>,
    layout: tauri::State<'_, InstallationLayout>,
    scheduler: tauri::State<'_, JobScheduler>,
    request: MaintenanceActivationRequest,
    manifest: serde_json::Value,
) -> Result<UpdateActivationReceipt, String> {
    let run = async {
        verify_identifier(&app.config().identifier)?;
        let packages = layout.maintenance_packages();
        let backups = layout.app_data().join("update-backups");
        std::fs::create_dir_all(&backups)?;
        VerifiedMaintenanceUpdate::prepare_signed_desktop_fixture(
            &app, &store, request, &packages, manifest,
        )
        .await?
        .install_after_drain(&store, &identity, &scheduler, packages, backups)
        .await
    };
    run.await.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn signed_fixture_migration_state(
    store: tauri::State<'_, Store>,
) -> Result<Option<String>, String> {
    omniagent::maintenance_update::read_signed_fixture_migration(&store)
        .await
        .map_err(|error| format!("{error:#}"))
}

pub fn read_journal_from_args(identifier: &str) -> Result<bool> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--read-signed-fixture-journal")) {
        return Ok(false);
    }
    verify_identifier(identifier)?;
    let output = PathBuf::from(args.next().context("Missing fixture journal output")?);
    ensure!(
        args.next().is_none(),
        "Unexpected fixture journal arguments"
    );
    let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("Missing LOCALAPPDATA")?)
        .join(identifier);
    ensure!(
        output == root.join("fixture-journal.json"),
        "Unexpected disposable journal output"
    );
    let identity = InstallationIdentity::load(&root)?;
    let result = UpdateJournal::open(&identity)?.latest()?;
    std::fs::write(output, serde_json::to_vec(&result)?)?;
    Ok(true)
}
