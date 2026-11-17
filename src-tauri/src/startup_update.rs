use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    diagnostics,
    installation::InstallationIdentity,
    maintenance_update::{
        ActivationOutcome, PreviousMaintenanceUpdate, TrustedUpdateFeed, UpdateActivationReceipt,
        UpdateJournal, restore_previous_for_startup,
    },
    restore::InstallationStartup,
    store::Store,
};

pub struct StartupUpdate {
    journal: UpdateJournal,
    active: Option<UpdateActivationReceipt>,
    version: String,
}

impl StartupUpdate {
    pub fn begin(app: &tauri::AppHandle, identity: &InstallationIdentity) -> Result<Self> {
        let journal = UpdateJournal::open(identity)?;
        attempt_requested_recovery(app, identity, &journal)?;
        let version = app.package_info().version.to_string();
        let active = journal.begin_startup(&version)?;
        if let Some(receipt) = &active
            && let ActivationOutcome::RecoveryRequired { reason } = &receipt.outcome
        {
            bail!(
                "Update recovery is required: {reason}. Run OmniAgent with --recover-update {}",
                receipt.id
            );
        }
        let active = active.filter(|receipt| {
            matches!(
                receipt.outcome,
                ActivationOutcome::CheckingStartup | ActivationOutcome::RestoringPrevious { .. }
            )
        });
        Ok(Self {
            journal,
            active,
            version,
        })
    }

    fn restoring(&self) -> Option<&UpdateActivationReceipt> {
        self.active.as_ref().filter(|receipt| {
            matches!(receipt.outcome, ActivationOutcome::RestoringPrevious { .. })
        })
    }

    pub fn open_installation(
        &self,
        identity: &InstallationIdentity,
        root: &Path,
    ) -> Result<InstallationStartup> {
        if let Some(receipt) = self.restoring() {
            restore_previous_for_startup(identity, receipt)?;
        }
        let startup = omniagent::restore::open_installation(root)?;
        if let Some(receipt) = self.restoring() {
            ensure!(
                startup
                    .restore
                    .active
                    .as_ref()
                    .is_some_and(|active| active.checkpoint_id == receipt.backup.checkpoint_id),
                "The running installation did not select the verified rollback checkpoint"
            );
        }
        Ok(startup)
    }

    pub fn verify<T>(&self, check: impl FnOnce() -> Result<T>) -> Result<T> {
        let result = check().inspect_err(|error| {
            if let Some(receipt) = &self.active
                && let Err(journal_error) = self
                    .journal
                    .startup_failed(&receipt.id, diagnostics::preview(&format!("{error:#}")))
            {
                eprintln!("Could not record update startup failure: {journal_error}");
            }
        })?;
        if let Some(receipt) = &self.active {
            self.journal.finish_startup(&receipt.id)?;
        }
        Ok(result)
    }

    pub fn publish_workflow_evidence(&self, store: &Store) -> Result<()> {
        tauri::async_runtime::block_on(self.journal.project_verified_startup(store, &self.version))
    }
}

fn attempt_requested_recovery(
    app: &tauri::AppHandle,
    identity: &InstallationIdentity,
    journal: &UpdateJournal,
) -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new("--recover-update")) {
        return Ok(());
    }
    let requested_id = args.next();
    ensure!(
        args.next().is_none(),
        "Pass at most one update activation ID to --recover-update"
    );
    let last = journal
        .latest()?
        .context("No update activation was recorded")?;
    ensure!(
        requested_id.is_none_or(|id| id == OsStr::new(&last.id)),
        "The requested update is no longer the latest activation"
    );
    match last.outcome {
        // The NSIS updater forwards launch arguments to the recovered build.
        ActivationOutcome::RollbackInstalling { .. } | ActivationOutcome::Recovered { .. } => {
            Ok(())
        }
        ActivationOutcome::RecoveryRequired { .. } => {
            let trust = TrustedUpdateFeed::application()?
                .context("This build has no trusted update feed configured")?;
            let previous =
                PreviousMaintenanceUpdate::prepare_previous(app, identity, &trust, &last.id)?;
            let activation = previous.install_previous()?;
            bail!(
                "The previous installer did not start: {:?}",
                activation.outcome
            );
        }
        _ => bail!("The requested update does not need rollback"),
    }
}
