use crate::{
    diagnostics,
    installation::InstallationIdentity,
    restore::{RestoreReceipt, preview_restore, restore_status, stage_restore},
};

use super::*;

pub struct PreviousMaintenanceUpdate {
    failed: UpdateActivationReceipt,
    previous: UpdateActivationReceipt,
    installer: Update,
    bytes: Vec<u8>,
    identity: InstallationIdentity,
}

impl PreviousMaintenanceUpdate {
    /// Prepares a cached superseded installer before opening the application database.
    pub fn prepare_previous<R: Runtime>(
        app: &AppHandle<R>,
        identity: &InstallationIdentity,
        trust: &TrustedUpdateFeed,
        activation_id: &str,
    ) -> Result<Self> {
        let journal = UpdateJournal::open(identity)?;
        let (failed, previous) = journal.qualified_previous(activation_id)?;
        verify_recovery_checkpoint(identity, &failed.backup)?;
        let version = previous.receipt.update.version.clone();
        let updater = trust
            .builder(app)?
            .version_comparator(move |_, release| release.version.to_string() == version)
            .build()?;
        let bytes = journal.read_previous_payload(&previous)?;
        let installer = updater
            .restore_update(previous.manifest)?
            .context("The cached installer does not advertise the exact previous version")?;
        verify_previous(&installer, &previous.receipt.update, &bytes)?;
        Ok(Self {
            failed,
            previous: previous.receipt,
            installer,
            bytes,
            identity: identity.clone(),
        })
    }

    pub fn preview(&self) -> &UpdateActivationReceipt {
        &self.previous
    }

    /// Windows exits after launching the earlier signed installer.
    /// The older build restores the matched checkpoint after it starts.
    pub fn install_previous(self) -> Result<UpdateActivationReceipt> {
        let _maintenance = self.identity.lock_maintenance_change()?;
        ensure!(
            restore_status(&self.identity)?.pending.is_none(),
            "A different restoration is already staged"
        );
        verify_recovery_checkpoint(&self.identity, &self.failed.backup)?;
        verify_previous(&self.installer, &self.previous.update, &self.bytes)?;
        let journal = UpdateJournal::open(&self.identity)?;
        let receipt = journal.start_rollback(&self.failed, &self.previous.id)?;
        match self.installer.install(&self.bytes) {
            Ok(()) => Ok(receipt),
            Err(error) => journal.rollback_failed(
                &receipt.id,
                &self.previous.id,
                diagnostics::preview(&error.to_string()),
            ),
        }
    }
}

/// Stage the protected pre-update checkpoint before opening the old build's database.
pub fn restore_previous_for_startup(
    identity: &InstallationIdentity,
    activation: &UpdateActivationReceipt,
) -> Result<RestoreReceipt> {
    restore_previous_for_build(
        identity,
        activation,
        env!("CARGO_PKG_VERSION"),
        journal::running_source(),
    )
}

pub(super) fn restore_previous_for_build(
    identity: &InstallationIdentity,
    activation: &UpdateActivationReceipt,
    version: &str,
    source: Option<&str>,
) -> Result<RestoreReceipt> {
    let journal = UpdateJournal::open(identity)?;
    journal.recheck_restore_startup(activation, version, source)?;
    verify_recovery_checkpoint(identity, &activation.backup)?;
    let restore = stage_restore(
        identity,
        &activation.backup.root,
        &activation.backup.manifest_sha256,
    )?;
    ensure!(
        restore.checkpoint_id == activation.backup.checkpoint_id,
        "The staged snapshot belongs to a different update checkpoint"
    );
    Ok(restore)
}

fn verify_previous(update: &Update, expected: &SignedUpdatePreview, bytes: &[u8]) -> Result<()> {
    verify_payload(
        update,
        &expected.version,
        &expected.source_revision,
        expected.size,
        &expected.sha256,
        bytes,
    )
}

fn verify_recovery_checkpoint(
    identity: &InstallationIdentity,
    backup: &UpdateBackup,
) -> Result<()> {
    let verified = preview_restore(identity, &backup.root)?;
    ensure!(
        verified.checkpoint_id == backup.checkpoint_id
            && verified.manifest_sha256 == backup.manifest_sha256,
        "The recovery snapshot does not match the pre-update checkpoint"
    );
    Ok(())
}
