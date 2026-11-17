use serde_json::{Value, json};

use crate::{
    installation::InstallationIdentity,
    jobs::JobScheduler,
    runner::DirectWindowsRunner,
    workflows::{ApprovalDecisionSource, WorkflowRevision},
};

use super::*;

const PAYLOAD: &[u8] = b"a disposable, signed NSIS-payload fixture";
const SOURCE: &str = "e302253cc8c7b010218eebb735595ede57e1dd13";
const VERSION: &str = "90.12.1";

fn handoff() -> MaintenanceHandoff {
    MaintenanceHandoff {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: "maintenance-project".into(),
        workflow_run_id: "workflow-run".into(),
        workflow: WorkflowRevision {
            id: "maintenance-workflow".into(),
            revision: "pinned".into(),
        },
        node_id: "activate".into(),
        git_artifact_id: "commit-artifact".into(),
        task_id: "maintenance-task".into(),
        unit_id: "release-unit".into(),
        plan_revision: "release-plan".into(),
        settings_revision: "workflow-settings".into(),
        version: VERSION.into(),
        source_revision: SOURCE.into(),
        source_tree: "fixture-tree".into(),
        candidate_revision: "selected-candidate".into(),
        sha256: format!("{:x}", Sha256::digest(PAYLOAD)),
        size: PAYLOAD.len() as u64,
        package_name: "fixture.exe".into(),
        commit_evidence_id: "verified-commit".into(),
        verification_id: "verified-checks".into(),
        checks: vec!["build".into(), "test".into()],
        approval_artifact_id: "approved-scope".into(),
        approval_source: ApprovalDecisionSource::Operator,
        acceptance_artifact_id: "accepted-diff".into(),
        review_artifact_id: "reviewed-result".into(),
        review_run_id: "review-run".into(),
        build_executable_revision: "pinned-executable".into(),
        recorded_at: "2026-11-16T00:00:00Z".into(),
        activation: None,
        signed_activation: None,
    }
}

fn metadata(keys: &minisign::KeyPair, announced: &str, comment: &str, url: &str) -> Result<Value> {
    signed_release(keys, announced, comment, url, PAYLOAD)
}

fn signed_release(
    keys: &minisign::KeyPair,
    announced: &str,
    comment: &str,
    url: &str,
    bytes: &[u8],
) -> Result<Value> {
    let signature =
        minisign::sign(Some(&keys.pk), &keys.sk, bytes, Some(comment), None)?.into_string();
    Ok(json!({ "version": announced, "url": url, "signature": STANDARD.encode(signature) }))
}

fn fixture_updater(keys: &minisign::KeyPair) -> Result<Updater> {
    let app = fixture_app("0.1.0")?;
    fixture_trust(keys)?.build(app.handle())
}

fn fixture_trust(keys: &minisign::KeyPair) -> Result<TrustedUpdateFeed> {
    let key = STANDARD.encode(keys.pk.to_box()?.into_string());
    TrustedUpdateFeed::new("https://release.example.test/update.json", &key)
}

fn fixture_app(version: &str) -> Result<tauri::App<tauri::test::MockRuntime>> {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.package_info_mut().version = version.parse()?;
    context.config_mut().plugins.0.insert(
        "updater".into(),
        json!({ "pubkey": "", "requireSignedVersion": true }),
    );
    Ok(tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(context)?)
}

fn fixture_update(updater: &Updater, keys: &minisign::KeyPair) -> Result<Update> {
    Ok(updater
        .restore_update(metadata(
            keys,
            VERSION,
            &format!("version:{VERSION}\tsource:{SOURCE}"),
            "https://release.example.test/update.exe",
        )?)?
        .context("missing update")?)
}

fn verified_fixture(
    keys: &minisign::KeyPair,
    current: &str,
    handoff: MaintenanceHandoff,
) -> Result<VerifiedMaintenanceUpdate> {
    let app = fixture_app(current)?;
    let updater = fixture_trust(keys)?.build(app.handle())?;
    let update = updater
        .restore_update(metadata(
            keys,
            &handoff.version,
            &format!(
                "version:{}\tsource:{}",
                handoff.version, handoff.source_revision
            ),
            "https://release.example.test/update.exe",
        )?)?
        .context("missing signed update fixture")?;
    verify_package(&update, &handoff, PAYLOAD)?;
    Ok(VerifiedMaintenanceUpdate {
        request: MaintenanceActivationRequest {
            project_id: handoff.project_id.clone(),
            handoff_id: handoff.id.clone(),
            expected_settings_revision: handoff.settings_revision.clone(),
            expected_sha256: handoff.sha256.clone(),
        },
        handoff,
        updater: update,
        bytes: PAYLOAD.to_vec(),
        automatic_policy_revision: None,
    })
}

#[test]
fn tauri_verifies_exact_bytes_version_and_signed_source_with_ephemeral_keys() -> Result<()> {
    let keys = minisign::KeyPair::generate_unencrypted_keypair()?;
    let updater = fixture_updater(&keys)?;
    let update = fixture_update(&updater, &keys)?;
    let handoff = handoff();
    verify_package(&update, &handoff, PAYLOAD)?;
    assert!(verify_package(&update, &handoff, b"replaced bytes").is_err());
    let independent = minisign::KeyPair::generate_unencrypted_keypair()?;
    assert!(verify_package(&fixture_update(&updater, &independent)?, &handoff, PAYLOAD).is_err());

    for (version, comment, url) in [
        (
            VERSION,
            format!("version:{VERSION}\tsource:wrong"),
            "https://release.example.test/update.exe",
        ),
        (
            VERSION,
            format!("version:{VERSION}\tsource:{SOURCE}\tsource:{SOURCE}"),
            "https://release.example.test/update.exe",
        ),
        (
            VERSION,
            format!("source:{SOURCE}"),
            "https://release.example.test/update.exe",
        ),
        (
            VERSION,
            format!("version:90.12.2\tsource:{SOURCE}"),
            "https://release.example.test/update.exe",
        ),
        (
            "90.12.2",
            format!("version:90.12.2\tsource:{SOURCE}"),
            "https://release.example.test/update.exe",
        ),
        (
            VERSION,
            format!("version:{VERSION}\tsource:{SOURCE}"),
            "http://release.example.test/update.exe",
        ),
    ] {
        let changed = updater
            .restore_update(metadata(&keys, version, &comment, url)?)?
            .context("missing test update")?;
        assert!(
            verify_package(&changed, &handoff, PAYLOAD).is_err(),
            "accepted mismatched fixture {comment}"
        );
    }

    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join(format!("{}.package", handoff.id)),
        PAYLOAD,
    )?;
    assert_eq!(read_handoff_package(directory.path(), &handoff)?, PAYLOAD);
    Ok(())
}

#[test]
fn failures_and_verified_startup_survive_journal_reopen() -> Result<()> {
    let keys = minisign::KeyPair::generate_unencrypted_keypair()?;
    let temp = tempfile::tempdir()?;
    let identity = InstallationIdentity::load_or_create(temp.path())?;
    let journal = UpdateJournal::open(&identity)?;
    let update = verified_fixture(&keys, "0.1.0", handoff())?;
    let backup = UpdateBackup {
        checkpoint_id: "verified-checkpoint".into(),
        manifest_sha256: "sha256".into(),
        root: temp.path().into(),
    };
    // The fixture is correctly signed but is neither a Windows executable nor an MSI.
    // This exercises Tauri's real installer failure path without shelling out.
    let failed = update.launch(&journal, backup.clone())?;
    assert!(matches!(
        failed.outcome,
        ActivationOutcome::InstallFailed { .. }
    ));
    assert_eq!(UpdateJournal::open(&identity)?.latest()?, Some(failed));
    let mut automatic_retry = verified_fixture(&keys, "0.1.0", handoff())?;
    automatic_retry.automatic_policy_revision = Some(uuid::Uuid::new_v4().to_string());
    assert_ne!(automatic_retry.handoff.id, update.handoff.id);
    assert!(
        journal
            .start_install(&automatic_retry, backup.clone())
            .unwrap_err()
            .to_string()
            .contains("already recorded")
    );
    let started = journal.start_install(&update, backup.clone())?;
    assert!(journal.start_install(&update, backup).is_err());
    assert_eq!(
        journal
            .begin_startup_for(VERSION, Some(SOURCE))?
            .unwrap()
            .outcome,
        ActivationOutcome::CheckingStartup
    );
    assert_eq!(
        journal.finish_startup(&started.id)?.outcome,
        ActivationOutcome::Verified
    );
    assert!(journal.begin_startup_for(VERSION, Some(SOURCE))?.is_none());
    let latest = UpdateJournal::open(&identity)?.latest()?.unwrap();
    assert_eq!(latest.id, started.id);
    assert_eq!(latest.outcome, ActivationOutcome::Verified);
    Ok(())
}

#[tokio::test]
async fn failed_startup_recovers_the_exact_previous_signature_and_database_checkpoint() -> Result<()>
{
    const NEXT_VERSION: &str = "90.13.0";
    const NEXT_SOURCE: &str = "2aa3c992ababdd488b5036fc11754b54d27a76f4";
    let keys = minisign::KeyPair::generate_unencrypted_keypair()?;
    let temp = tempfile::tempdir()?;
    let identity = InstallationIdentity::load_or_create(temp.path())?;
    let store = Store::open(identity.app_data().join(crate::backup::DATABASE_FILE))?;
    let package_root = identity.app_data().join("maintenance-packages");
    std::fs::create_dir(&package_root)?;
    let archive = store
        .create_application_backup(identity.clone(), package_root, identity.app_data().into())
        .await?;
    let backup = UpdateBackup {
        checkpoint_id: archive.manifest.checkpoint_id,
        manifest_sha256: archive.manifest_sha256,
        root: archive.root,
    };
    store
        .execute(|db| {
            db.execute(
                "UPDATE job_queue_settings SET capacity = 7 WHERE id = 1",
                [],
            )?;
            Ok(())
        })
        .await?;
    let journal = UpdateJournal::open(&identity)?;

    let previous = verified_fixture(&keys, "0.1.0", handoff())?;
    let previous = journal.start_install(&previous, backup.clone())?;
    journal.begin_startup_for(VERSION, Some(SOURCE))?;
    journal.finish_startup(&previous.id)?;

    let next_handoff = MaintenanceHandoff {
        version: NEXT_VERSION.into(),
        source_revision: NEXT_SOURCE.into(),
        ..handoff()
    };
    let next = verified_fixture(&keys, VERSION, next_handoff)?;
    let failed = journal.start_install_from(&next, backup.clone(), Some(SOURCE))?;
    assert_eq!(
        journal
            .begin_startup_for(NEXT_VERSION, Some(NEXT_SOURCE))?
            .unwrap()
            .outcome,
        ActivationOutcome::CheckingStartup,
    );
    let failed = journal.startup_failed(&failed.id, "fixture migration failure".into())?;
    assert_eq!(
        UpdateJournal::open(&identity)?.latest()?,
        Some(failed.clone())
    );
    let app = fixture_app(NEXT_VERSION)?;
    let trust = fixture_trust(&keys)?;
    let recovery =
        PreviousMaintenanceUpdate::prepare_previous(app.handle(), &identity, &trust, &failed.id)?;
    assert_eq!(recovery.preview().id, previous.id);
    let failed = recovery.install_previous()?;
    assert!(matches!(
        failed.outcome,
        ActivationOutcome::RecoveryRequired { .. }
    ));
    // The installer-launch success and old process identity are injected in the
    // fixture. Signature verification and the database restore are real.
    journal.start_rollback(&failed, &previous.id)?;
    let startup = journal.begin_startup_for(VERSION, Some(SOURCE))?.unwrap();
    assert!(matches!(
        startup.outcome,
        ActivationOutcome::RestoringPrevious { .. }
    ));
    assert!(
        recovery::restore_previous_for_build(&identity, &startup, NEXT_VERSION, Some(NEXT_SOURCE))
            .is_err()
    );
    let staged = recovery::restore_previous_for_build(&identity, &startup, VERSION, Some(SOURCE))?;
    assert_eq!(staged.checkpoint_id, backup.checkpoint_id);
    let restored = crate::restore::open_installation(identity.app_data())?;
    assert_eq!(restored.identity.id(), identity.id());
    assert_eq!(restored.restore.active, Some(staged));
    let recovered_store = Store::open(restored.layout.database_path())?;
    let capacity = |db: &mut rusqlite::Connection| {
        Ok(db.query_row(
            "SELECT capacity FROM job_queue_settings WHERE id = 1",
            [],
            |row| row.get::<_, u32>(0),
        )?)
    };
    assert_eq!(store.execute(capacity).await?, 7);
    assert_eq!(recovered_store.execute(capacity).await?, 3);
    let completed = journal.finish_startup(&failed.id)?;
    assert_eq!(
        completed.outcome,
        ActivationOutcome::Recovered {
            previous_activation_id: previous.id
        },
    );
    assert_eq!(
        UpdateJournal::open(&restored.identity)?.latest()?,
        Some(completed)
    );
    Ok(())
}

#[tokio::test]
async fn drain_blocks_new_work_waits_for_active_leases_and_reopens_on_drop() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("application.sqlite3"))?;
    let scheduler = JobScheduler::start(
        store.clone(),
        std::sync::Arc::new(DirectWindowsRunner),
        None,
    );
    let active = scheduler.admit_external_work().await?;
    let result = scheduler
        .drain_for_update(&store, Duration::from_millis(50))
        .await;
    assert!(result.is_err());
    let drain = tokio::spawn({
        let scheduler = scheduler.clone();
        let store = store.clone();
        async move {
            scheduler
                .drain_for_update(&store, Duration::from_secs(3))
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while scheduler.admit_external_work().await.is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(!drain.is_finished());
    drop(active);
    let drain = drain.await??;
    assert!(scheduler.admit_external_work().await.is_err());
    assert!(
        scheduler
            .drain_for_update(&store, Duration::from_millis(50))
            .await
            .is_err()
    );
    drop(drain);
    assert!(scheduler.admit_external_work().await.is_ok());
    scheduler.shutdown().await?;
    Ok(())
}

pub(crate) async fn exercise_signed_handoff_activation(
    store: &Store,
    identity: &InstallationIdentity,
    handoff: MaintenanceHandoff,
    package_root: PathBuf,
    backup_parent: PathBuf,
) -> Result<()> {
    let request = MaintenanceActivationRequest {
        project_id: handoff.project_id.clone(),
        handoff_id: handoff.id.clone(),
        expected_settings_revision: handoff.settings_revision.clone(),
        expected_sha256: handoff.sha256.clone(),
    };
    let mut mismatched = request.clone();
    mismatched.expected_sha256 = "01".repeat(32);
    assert!(
        store
            .require_signed_update_handoff(mismatched)
            .await
            .is_err()
    );
    assert_eq!(
        store.require_signed_update_handoff(request.clone()).await?,
        handoff
    );
    let keys = minisign::KeyPair::generate_unencrypted_keypair()?;
    let bytes = read_handoff_package(&package_root, &handoff)?;
    let app = fixture_app("0.1.0")?;
    let updater = fixture_trust(&keys)?.build(app.handle())?;
    let metadata = signed_release(
        &keys,
        &handoff.version,
        &format!(
            "version:{}\tsource:{}",
            handoff.version, handoff.source_revision
        ),
        "https://release.example.test/update.exe",
        &bytes,
    )?;
    let prepare = || -> Result<VerifiedMaintenanceUpdate> {
        let installer = updater
            .restore_update(metadata.clone())?
            .context("The signed fixture installer is unavailable")?;
        verify_package(&installer, &handoff, &bytes)?;
        Ok(VerifiedMaintenanceUpdate {
            request: request.clone(),
            handoff: handoff.clone(),
            updater: installer,
            bytes: bytes.clone(),
            automatic_policy_revision: None,
        })
    };
    let scheduler = JobScheduler::start(
        store.clone(),
        std::sync::Arc::new(DirectWindowsRunner),
        None,
    );
    let journal = UpdateJournal::open(identity)?;
    let failed = prepare()?
        .install_after_drain(store, identity, &scheduler, package_root, backup_parent)
        .await?;
    assert!(matches!(
        failed.outcome,
        ActivationOutcome::InstallFailed { .. }
    ));
    assert!(scheduler.admit_external_work().await.is_ok());
    scheduler.shutdown().await?;
    assert_eq!(journal.latest()?, Some(failed.clone()));
    assert!(crate::restore::preview_restore(identity, &failed.backup.root).is_ok());
    let read_handoff = || async {
        store
            .workflow_maintenance(handoff.project_id.clone())
            .await?
            .handoffs
            .into_iter()
            .find(|entry| entry.id == handoff.id)
            .context("The signed handoff is missing from the maintenance history")
    };
    assert!(read_handoff().await?.signed_activation.is_none());

    // The synthetic installer has valid provenance but cannot start Windows.
    // Inject the accepted startup transition to test graph publication.
    let started = journal.start_install(&prepare()?, failed.backup)?;
    journal.begin_startup_for(&handoff.version, Some(&handoff.source_revision))?;
    journal
        .project_verified_for_build(store, &handoff.version, Some(&handoff.source_revision))
        .await?;
    assert!(read_handoff().await?.signed_activation.is_none());
    journal.finish_startup(&started.id)?;
    journal
        .project_verified_for_build(store, &handoff.version, Some(&"f".repeat(40)))
        .await?;
    assert!(read_handoff().await?.signed_activation.is_none());
    for _ in 0..2 {
        UpdateJournal::open(identity)?
            .project_verified_for_build(store, &handoff.version, Some(&handoff.source_revision))
            .await?;
    }
    let projected = read_handoff().await?;
    assert!(projected.activation.is_none());
    let activation = projected
        .signed_activation
        .context("The signed activation is missing from the handoff")?;
    assert_eq!(activation.id, started.id);
    assert_eq!(activation.sha256, handoff.sha256);
    assert_eq!(activation.source_revision, handoff.source_revision);
    let execution = store
        .inspect_workflow_execution(handoff.project_id.clone(), handoff.workflow_run_id.clone())
        .await?
        .context("The signed artifact's workflow is unavailable")?;
    let outputs = execution
        .outputs
        .iter()
        .filter(|output| {
            output.artifact.node_instance_id == handoff.node_id && output.artifact.port == "source"
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].artifact.id, activation.workflow_artifact_id);
    assert!(outputs[0].current);
    assert!(
        outputs[0]
            .input_receipts
            .iter()
            .any(|receipt| receipt.inputs.get("git") == Some(&handoff.git_artifact_id))
    );
    assert!(store.require_signed_update_handoff(request).await.is_err());
    Ok(())
}
