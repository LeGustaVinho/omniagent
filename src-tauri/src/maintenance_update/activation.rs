use super::automatic_policy::recheck_automatic_update_policy;
use super::*;
use crate::{
    backup::{BackupEntry, BackupReceipt},
    diagnostics,
    installation::InstallationIdentity,
    jobs::{JobScheduler, UpdateDrain},
    restore::restore_status,
    workflows::maintenance::validate_signed_update_handoff,
};

const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

impl VerifiedMaintenanceUpdate {
    pub(super) async fn install_after_drain_automatically(
        mut self,
        store: &Store,
        identity: &InstallationIdentity,
        scheduler: &JobScheduler,
        package_root: PathBuf,
        backup_parent: PathBuf,
        policy_revision: String,
    ) -> Result<UpdateActivationReceipt> {
        self.automatic_policy_revision = Some(policy_revision);
        self.install_after_drain(store, identity, scheduler, package_root, backup_parent)
            .await
    }

    /// An accepted Windows installer exits this process and restarts the app.
    /// A launch failure returns its durable receipt and resumes job admission.
    pub async fn install_after_drain(
        self,
        store: &Store,
        identity: &InstallationIdentity,
        scheduler: &JobScheduler,
        package_root: PathBuf,
        backup_parent: PathBuf,
    ) -> Result<UpdateActivationReceipt> {
        let journal = UpdateJournal::open(identity)?;
        let maintenance = identity.lock_maintenance_change()?;
        ensure!(
            restore_status(identity)?.pending.is_none(),
            "Complete the staged restore before activating an update"
        );
        journal.require_new_install(&self.handoff.id)?;
        let request = self.request.clone();
        let revision = self.automatic_policy_revision.clone();
        let installation = identity.clone();
        let reviewed = store
            .execute(move |db| {
                if let Some(revision) = revision.as_deref() {
                    recheck_automatic_update_policy(db, &installation, revision, &request)?;
                }
                validate_signed_update_handoff(db, &request)
            })
            .await?;
        ensure!(
            reviewed == self.handoff,
            "The accepted maintenance handoff changed before backup"
        );
        let drain: UpdateDrain = scheduler.drain_for_update(store, DRAIN_TIMEOUT).await?;
        let backup = store
            .create_application_backup(identity.clone(), package_root, backup_parent)
            .await
            .context("The pre-update application backup failed")?;
        let backup = self.bind_backup(identity, backup)?;

        // Keep the drain and launch boundary owned by the queued callback.
        // Disconnecting the invoking webview cannot reopen admission mid-launch.
        let launch_boundary = drain.lock_launch_boundary().await?;
        let installation = identity.clone();
        store
            .execute(move |db| {
                let _launch_boundary = launch_boundary;
                let _maintenance = maintenance;
                drain.confirm(db)?;
                if let Some(revision) = self.automatic_policy_revision.as_deref() {
                    recheck_automatic_update_policy(db, &installation, revision, &self.request)?;
                }
                ensure!(
                    validate_signed_update_handoff(db, &self.request)? == self.handoff,
                    "The accepted maintenance handoff changed after backup"
                );
                self.launch(&journal, backup)
            })
            .await
    }

    fn bind_backup(
        &self,
        identity: &InstallationIdentity,
        backup: BackupReceipt,
    ) -> Result<UpdateBackup> {
        ensure!(
            backup.manifest.installation_id == identity.id()
                && backup.manifest.entries.iter().any(|entry| matches!(
                    entry,
                    BackupEntry::MaintenancePackage { handoff_id, size, sha256 }
                        if handoff_id == &self.handoff.id
                            && *size == self.handoff.size
                            && sha256 == &self.handoff.sha256
                )),
            "The pre-update backup does not contain the exact accepted package"
        );
        Ok(UpdateBackup {
            checkpoint_id: backup.manifest.checkpoint_id,
            manifest_sha256: backup.manifest_sha256,
            root: backup.root,
        })
    }

    pub(super) fn launch(
        &self,
        journal: &UpdateJournal,
        backup: UpdateBackup,
    ) -> Result<UpdateActivationReceipt> {
        verify_package(&self.updater, &self.handoff, &self.bytes)?;
        let receipt = journal.start_install(self, backup)?;
        match self.updater.install(&self.bytes) {
            Ok(()) => Ok(receipt),
            Err(error) => {
                journal.install_failed(&receipt.id, diagnostics::preview(&error.to_string()))
            }
        }
    }
}
