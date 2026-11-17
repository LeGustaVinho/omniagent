use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    repository,
    restore::{open_installation, stage_restore},
    workflows::maintenance::{MaintenanceBuild, MaintenanceConfiguration},
};

use super::*;

#[tokio::test]
async fn a_grant_is_bound_to_the_reviewed_project_and_settings_revision() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let installation = open_installation(&scratch.path().join("installation"))?;
    let identity = installation.identity;
    let store = Store::open(installation.layout.database_path())?;
    assert_eq!(
        store.automatic_update_policy(identity.clone()).await?,
        AutomaticUpdatePolicy::default()
    );
    let (project, mut configuration) =
        maintenance_project(&store, &scratch.path().join("source")).await?;

    let disabled = store
        .set_workflow_maintenance(project.clone(), None, configuration.clone())
        .await?;
    let disabled_target = AutomaticUpdateTarget {
        project_id: project.clone(),
        maintenance_settings_revision: disabled.revision.clone(),
    };
    assert!(
        store
            .set_automatic_update_policy(identity.clone(), None, Some(disabled_target))
            .await
            .is_err()
    );
    assert!(!identity.app_data().join(POLICY_FILE).exists());

    configuration.activation = MaintenanceActivationMode::SignedUpdater;
    let reviewed = store
        .set_workflow_maintenance(
            project.clone(),
            Some(disabled.revision),
            configuration.clone(),
        )
        .await?;
    let target = AutomaticUpdateTarget {
        project_id: project.clone(),
        maintenance_settings_revision: reviewed.revision.clone(),
    };
    let grant = store
        .set_automatic_update_policy(identity.clone(), None, Some(target.clone()))
        .await?;
    let revision = grant
        .revision
        .clone()
        .context("The grant has no revision")?;
    assert_eq!(grant.target, Some(target.clone()));
    let request = candidate(&target);
    recheck(&store, &identity, &revision, request.clone()).await?;

    let mut different_project = request.clone();
    different_project.project_id = Uuid::new_v4().to_string();
    assert!(
        recheck(&store, &identity, &revision, different_project)
            .await
            .is_err()
    );
    let mut different_settings = request.clone();
    different_settings.expected_settings_revision = Uuid::new_v4().to_string();
    assert!(
        recheck(&store, &identity, &revision, different_settings)
            .await
            .is_err()
    );

    configuration
        .required_checks
        .push("different required check".into());
    store
        .set_workflow_maintenance(project, Some(reviewed.revision), configuration)
        .await?;
    assert!(
        recheck(&store, &identity, &revision, request)
            .await
            .is_err()
    );
    assert!(
        store
            .set_automatic_update_policy(identity.clone(), grant.revision.clone(), Some(target))
            .await
            .is_err()
    );
    assert_eq!(
        store.automatic_update_policy(identity.clone()).await?,
        grant
    );
    let revoked = store
        .set_automatic_update_policy(identity, grant.revision, None)
        .await?;
    assert_eq!(revoked.target, None);
    assert_ne!(revoked.revision, Some(revision));
    Ok(())
}

#[tokio::test]
async fn concurrent_edits_have_one_winner_and_restore_does_not_revive_revoked_permission()
-> Result<()> {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("installation");
    let installation = open_installation(&root)?;
    let identity = installation.identity;
    let store = Store::open(installation.layout.database_path())?;
    let (project, mut configuration) =
        maintenance_project(&store, &scratch.path().join("source")).await?;
    configuration.activation = MaintenanceActivationMode::SignedUpdater;
    let reviewed = store
        .set_workflow_maintenance(project.clone(), None, configuration)
        .await?;
    let target = AutomaticUpdateTarget {
        project_id: project,
        maintenance_settings_revision: reviewed.revision,
    };
    let request = candidate(&target);
    let (left, right) = tokio::join!(
        store.set_automatic_update_policy(identity.clone(), None, Some(target.clone())),
        store.set_automatic_update_policy(identity.clone(), None, Some(target.clone())),
    );
    let grant = match (left, right) {
        (Ok(grant), Err(_)) | (Err(_), Ok(grant)) => grant,
        _ => anyhow::bail!("Exactly one edit of the same policy revision must succeed"),
    };
    let revision = grant
        .revision
        .clone()
        .context("The grant has no revision")?;
    recheck(&store, &identity, &revision, request.clone()).await?;
    let backup = store
        .create_application_backup(
            identity.clone(),
            installation.layout.maintenance_packages(),
            scratch.path().to_owned(),
        )
        .await?;
    assert!(!backup.root.join(POLICY_FILE).exists());

    assert!(
        store
            .set_automatic_update_policy(identity.clone(), None, None)
            .await
            .is_err()
    );
    let revoked = store
        .set_automatic_update_policy(identity.clone(), grant.revision, None)
        .await?;
    assert_ne!(revoked.revision.as_deref(), Some(revision.as_str()));
    assert!(
        recheck(&store, &identity, &revision, request.clone())
            .await
            .is_err()
    );
    stage_restore(&identity, &backup.root, &backup.manifest_sha256)?;
    drop(store);

    let restored = open_installation(&root)?;
    assert_eq!(restored.identity.id(), identity.id());
    assert_ne!(
        restored.layout.database_path(),
        installation.layout.database_path()
    );
    let store = Store::open(restored.layout.database_path())?;
    assert_eq!(
        store
            .automatic_update_policy(restored.identity.clone())
            .await?,
        revoked
    );
    assert!(
        recheck(&store, &restored.identity, &revision, request.clone())
            .await
            .is_err()
    );
    let renewed = store
        .set_automatic_update_policy(restored.identity.clone(), revoked.revision, Some(target))
        .await?;
    let new_revision = renewed
        .revision
        .context("The renewed grant has no revision")?;
    assert_ne!(new_revision, revision);
    assert!(
        recheck(&store, &restored.identity, &revision, request.clone())
            .await
            .is_err()
    );
    recheck(&store, &restored.identity, &new_revision, request).await
}

#[tokio::test]
async fn a_tampered_or_copied_policy_cannot_authorize_this_installation() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let identity = InstallationIdentity::load_or_create(&scratch.path().join("installation"))?;
    let store = Store::open(identity.app_data().join("test.sqlite3"))?;
    store
        .set_automatic_update_policy(identity.clone(), None, None)
        .await?;
    let path = identity.app_data().join(POLICY_FILE);
    let foreign = InstallationIdentity::load_or_create(&scratch.path().join("foreign"))?;
    fs::copy(&path, foreign.app_data().join(POLICY_FILE))?;
    assert!(
        store
            .automatic_update_policy(foreign.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .set_automatic_update_policy(foreign, None, None)
            .await
            .is_err()
    );

    let mut bytes = fs::read(&path)?;
    let last = bytes.last_mut().context("The protected record is empty")?;
    *last ^= 1;
    fs::write(&path, bytes)?;
    assert!(
        store
            .automatic_update_policy(identity.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .set_automatic_update_policy(identity, None, None)
            .await
            .is_err()
    );
    Ok(())
}

async fn maintenance_project(
    store: &Store,
    root: &Path,
) -> Result<(String, MaintenanceConfiguration)> {
    fs::create_dir(root)?;
    let git = Command::new("git.exe")
        .current_dir(root)
        .args(["init", "--quiet"])
        .output()?;
    ensure!(
        git.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&git.stderr)
    );
    let checkout = repository::register(store, root.to_owned()).await?;
    let system = PathBuf::from(std::env::var_os("SystemRoot").context("Missing SystemRoot")?);
    Ok((
        checkout.project_id,
        MaintenanceConfiguration {
            source_checkout_id: checkout.checkout_id,
            workflow_id: "maintenance-workflow-fixture".into(),
            required_checks: vec!["required test fixture".into()],
            activation: MaintenanceActivationMode::Disabled,
            build: MaintenanceBuild {
                executable: system.join(r"System32\cmd.exe"),
                arguments: Vec::new(),
                timeout_seconds: Some(10),
                package_path: "candidate.exe".into(),
                version_path: "version.txt".into(),
            },
        },
    ))
}

fn candidate(target: &AutomaticUpdateTarget) -> MaintenanceActivationRequest {
    MaintenanceActivationRequest {
        project_id: target.project_id.clone(),
        expected_settings_revision: target.maintenance_settings_revision.clone(),
        handoff_id: Uuid::new_v4().to_string(),
        expected_sha256: "0".repeat(64),
    }
}

async fn recheck(
    store: &Store,
    identity: &InstallationIdentity,
    revision: &str,
    request: MaintenanceActivationRequest,
) -> Result<()> {
    let identity = identity.clone();
    let revision = revision.to_owned();
    store
        .execute(move |db| recheck_automatic_update_policy(db, &identity, &revision, &request))
        .await
}
