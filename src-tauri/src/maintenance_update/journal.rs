use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    installation::{InstallationIdentity, atomic_file},
    providers::configuration::open_locked_local_file,
    store::Store,
};

use super::{SignedUpdatePreview, VerifiedMaintenanceUpdate};

const MAX_RECEIPT_BYTES: u64 = 2 * 1024 * 1024;
const JOURNAL_DOMAIN: &[u8] = b"maintenance-updater-journal-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBackup {
    pub checkpoint_id: String,
    pub manifest_sha256: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ActivationOutcome {
    Installing,
    InstallFailed { reason: String },
    CheckingStartup,
    Verified,
    RecoveryRequired { reason: String },
    RollbackInstalling { previous_activation_id: String },
    RestoringPrevious { previous_activation_id: String },
    Recovered { previous_activation_id: String },
}

impl ActivationOutcome {
    fn blocks_install(&self) -> bool {
        matches!(
            self,
            Self::Installing
                | Self::CheckingStartup
                | Self::RecoveryRequired { .. }
                | Self::RollbackInstalling { .. }
                | Self::RestoringPrevious { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateActivationReceipt {
    pub id: String,
    pub installation_id: String,
    pub project_id: String,
    pub update: SignedUpdatePreview,
    pub source_before_install: Option<String>,
    pub backup: UpdateBackup,
    pub recorded_at_ms: u64,
    pub outcome: ActivationOutcome,
    #[serde(default)]
    pub automatic_policy_revision: Option<String>,
}

pub(super) struct AutomaticJournalState {
    pub pending: bool,
    pub attempted_handoffs: std::collections::HashSet<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct JournalEntry {
    pub(super) receipt: UpdateActivationReceipt,
    pub(super) manifest: serde_json::Value,
}

pub struct UpdateJournal {
    root: PathBuf,
    identity: InstallationIdentity,
}

impl UpdateJournal {
    pub fn open(installation: &InstallationIdentity) -> Result<Self> {
        installation.require_current()?;
        let root = installation.app_data().join("maintenance-update-journal");
        fs::create_dir_all(&root)?;
        ensure!(
            dunce::canonicalize(&root)? == root,
            "The installation update journal moved outside its registered path"
        );
        Ok(Self {
            root,
            identity: installation.clone(),
        })
    }

    pub fn latest(&self) -> Result<Option<UpdateActivationReceipt>> {
        let _lock = self.lock()?;
        Ok(self
            .entries()?
            .into_iter()
            .max_by_key(|entry| (entry.receipt.recorded_at_ms, entry.receipt.id.clone()))
            .map(|entry| entry.receipt))
    }

    pub(super) fn automatic_state(&self) -> Result<AutomaticJournalState> {
        let _lock = self.lock()?;
        let entries = self.entries()?;
        Ok(AutomaticJournalState {
            pending: entries
                .iter()
                .any(|entry| entry.receipt.outcome.blocks_install()),
            attempted_handoffs: entries
                .into_iter()
                .map(|entry| entry.receipt.update.handoff_id)
                .collect(),
        })
    }

    /// Publish the verified activation to its pinned workflow after startup.
    pub async fn project_verified_startup(
        &self,
        store: &Store,
        package_version: &str,
    ) -> Result<()> {
        let source = running_source().filter(|_| package_version == env!("CARGO_PKG_VERSION"));
        self.project_verified_for_build(store, package_version, source)
            .await
    }

    pub(super) async fn project_verified_for_build(
        &self,
        store: &Store,
        version: &str,
        source: Option<&str>,
    ) -> Result<()> {
        let Some(source) = source else {
            return Ok(());
        };
        let receipt = {
            let _lock = self.lock()?;
            self.entries()?
                .into_iter()
                .map(|entry| entry.receipt)
                .filter(|receipt| {
                    receipt.outcome == ActivationOutcome::Verified
                        && receipt.update.version == version
                        && receipt.update.source_revision == source
                })
                .max_by_key(|receipt| receipt.recorded_at_ms)
        };
        if let Some(receipt) = receipt {
            store.record_verified_signed_activation(receipt).await?;
        }
        Ok(())
    }

    pub(super) fn start_install(
        &self,
        update: &VerifiedMaintenanceUpdate,
        backup: UpdateBackup,
    ) -> Result<UpdateActivationReceipt> {
        self.start_install_from(update, backup, running_source())
    }

    pub(super) fn require_new_install(&self, handoff_id: &str) -> Result<()> {
        let _lock = self.lock()?;
        Self::check_no_pending_install(&self.entries()?, handoff_id)
    }

    fn check_no_pending_install(prior: &[JournalEntry], handoff_id: &str) -> Result<()> {
        ensure!(
            prior
                .iter()
                .all(|entry| !entry.receipt.outcome.blocks_install()),
            "A previous activation still needs startup verification or recovery"
        );
        ensure!(
            prior
                .iter()
                .all(|entry| entry.receipt.update.handoff_id != handoff_id
                    || entry.receipt.outcome != ActivationOutcome::Verified),
            "This accepted maintenance package was already activated"
        );
        Ok(())
    }

    pub(super) fn start_install_from(
        &self,
        update: &VerifiedMaintenanceUpdate,
        backup: UpdateBackup,
        source_before_install: Option<&str>,
    ) -> Result<UpdateActivationReceipt> {
        let _lock = self.lock()?;
        let prior = self.entries()?;
        Self::check_no_pending_install(&prior, &update.handoff.id)?;
        if update.automatic_policy_revision.is_some() {
            ensure!(
                prior.iter().all(|entry| {
                    let receipt = &entry.receipt;
                    receipt.update.handoff_id != update.handoff.id
                        && !(receipt.project_id == update.handoff.project_id
                            && receipt.update.version == update.updater.version
                            && receipt.update.source_revision == update.handoff.source_revision
                            && receipt.update.sha256 == update.handoff.sha256)
                }),
                "An installation attempt for this handoff is already recorded"
            );
        }
        let id = Uuid::new_v4();
        let clock: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .try_into()?;
        let receipt = UpdateActivationReceipt {
            id: id.to_string(),
            installation_id: self.identity.id().into(),
            project_id: update.handoff.project_id.clone(),
            update: update.preview(),
            source_before_install: source_before_install.map(str::to_owned),
            automatic_policy_revision: update.automatic_policy_revision.clone(),
            backup,
            recorded_at_ms: prior
                .iter()
                .map(|entry| entry.receipt.recorded_at_ms.saturating_add(1))
                .max()
                .unwrap_or(clock)
                .max(clock),
            outcome: ActivationOutcome::Installing,
        };
        let entry = JournalEntry {
            receipt,
            manifest: update.updater.raw_json.clone(),
        };
        let mut package = tempfile::NamedTempFile::new_in(&self.root)?;
        package.write_all(&update.bytes)?;
        package.as_file().sync_all()?;
        let package_path = self.root.join(format!("{id}.package"));
        package.persist_noclobber(&package_path)?;
        if let Err(error) = self.write(&entry) {
            let _ = fs::remove_file(package_path);
            return Err(error);
        }
        Ok(entry.receipt)
    }

    pub(super) fn install_failed(
        &self,
        id: &str,
        reason: String,
    ) -> Result<UpdateActivationReceipt> {
        self.change(
            id,
            ActivationOutcome::Installing,
            ActivationOutcome::InstallFailed { reason },
        )
    }

    /// Records startup before opening or migrating the live application database.
    pub fn begin_startup(&self, package_version: &str) -> Result<Option<UpdateActivationReceipt>> {
        let _maintenance = self.identity.lock_maintenance_change()?;
        let source = running_source().filter(|_| package_version == env!("CARGO_PKG_VERSION"));
        self.begin_startup_for(package_version, source)
    }

    pub(super) fn begin_startup_for(
        &self,
        version: &str,
        commit: Option<&str>,
    ) -> Result<Option<UpdateActivationReceipt>> {
        let _lock = self.lock()?;
        let mut entries = self
            .entries()?
            .into_iter()
            .filter(|entry| entry.receipt.outcome.blocks_install());
        let Some(mut entry) = entries.next() else {
            return Ok(None);
        };
        ensure!(
            entries.next().is_none(),
            "More than one update activation needs reconciliation"
        );
        entry.receipt.outcome = match &entry.receipt.outcome {
            ActivationOutcome::Installing
                if version == entry.receipt.update.version
                    && commit == Some(entry.receipt.update.source_revision.as_str()) =>
            {
                ActivationOutcome::CheckingStartup
            }
            ActivationOutcome::Installing
                if version == entry.receipt.update.current_version
                    && commit.is_some()
                    && commit == entry.receipt.source_before_install.as_deref() =>
            {
                ActivationOutcome::InstallFailed {
                    reason: "The previous build is still installed and the update never started"
                        .into(),
                }
            }
            ActivationOutcome::Installing => ActivationOutcome::RecoveryRequired {
                reason: "The installed build does not match the accepted version and source commit"
                    .into(),
            },
            ActivationOutcome::CheckingStartup => ActivationOutcome::RecoveryRequired {
                reason: "The accepted build exited before startup verification completed".into(),
            },
            ActivationOutcome::RollbackInstalling {
                previous_activation_id,
            } => {
                let previous = self.read(previous_activation_id)?;
                if Self::is_qualified_previous(&entry, &previous)
                    && version == previous.receipt.update.version
                    && commit == Some(previous.receipt.update.source_revision.as_str())
                {
                    ActivationOutcome::RestoringPrevious {
                        previous_activation_id: previous_activation_id.clone(),
                    }
                } else {
                    ActivationOutcome::RecoveryRequired {
                        reason: "The previous qualified build did not start after rollback".into(),
                    }
                }
            }
            ActivationOutcome::RestoringPrevious { .. } => ActivationOutcome::RecoveryRequired {
                reason:
                    "The previous build exited before it finished verifying the restored snapshot"
                        .into(),
            },
            ActivationOutcome::RecoveryRequired { .. } => return Ok(Some(entry.receipt)),
            _ => unreachable!(),
        };
        self.write(&entry)?;
        Ok(Some(entry.receipt))
    }

    /// Called after the accepted build passes its application and migration checks.
    pub fn finish_startup(&self, id: &str) -> Result<UpdateActivationReceipt> {
        let _lock = self.lock()?;
        let mut entry = self.read(id)?;
        entry.receipt.outcome = match entry.receipt.outcome {
            ActivationOutcome::CheckingStartup => ActivationOutcome::Verified,
            ActivationOutcome::RestoringPrevious {
                previous_activation_id,
            } => ActivationOutcome::Recovered {
                previous_activation_id,
            },
            _ => anyhow::bail!("The installation update state changed"),
        };
        self.write(&entry)?;
        Ok(entry.receipt)
    }

    pub fn startup_failed(&self, id: &str, reason: String) -> Result<UpdateActivationReceipt> {
        let _lock = self.lock()?;
        let mut entry = self.read(id)?;
        ensure!(
            matches!(
                entry.receipt.outcome,
                ActivationOutcome::CheckingStartup | ActivationOutcome::RestoringPrevious { .. }
            ),
            "The installation update state changed"
        );
        entry.receipt.outcome = ActivationOutcome::RecoveryRequired { reason };
        self.write(&entry)?;
        Ok(entry.receipt)
    }

    pub(super) fn qualified_previous(
        &self,
        id: &str,
    ) -> Result<(UpdateActivationReceipt, JournalEntry)> {
        let _lock = self.lock()?;
        let failed = self.read(id)?;
        ensure!(
            matches!(
                failed.receipt.outcome,
                ActivationOutcome::RecoveryRequired { .. }
            ),
            "This activation does not require recovery"
        );
        let previous = self
            .entries()?
            .into_iter()
            .filter(|entry| Self::is_qualified_previous(&failed, entry))
            .max_by_key(|entry| entry.receipt.recorded_at_ms)
            .context("No previously verified installer is cached for the prior build")?;
        Ok((failed.receipt, previous))
    }

    fn is_qualified_previous(failed: &JournalEntry, previous: &JournalEntry) -> bool {
        previous.receipt.outcome == ActivationOutcome::Verified
            && previous.receipt.recorded_at_ms < failed.receipt.recorded_at_ms
            && previous.receipt.update.version == failed.receipt.update.current_version
            && failed.receipt.source_before_install.as_deref()
                == Some(previous.receipt.update.source_revision.as_str())
    }

    pub(super) fn read_previous_payload(&self, previous: &JournalEntry) -> Result<Vec<u8>> {
        let id = Uuid::parse_str(&previous.receipt.id)?;
        let file = open_locked_local_file(&self.root.join(format!("{id}.package")))?;
        super::read_package_bytes(file, previous.receipt.update.size)
    }

    pub(super) fn recheck_restore_startup(
        &self,
        expected: &UpdateActivationReceipt,
        version: &str,
        source: Option<&str>,
    ) -> Result<()> {
        let _lock = self.lock()?;
        let entry = self.read(&expected.id)?;
        let ActivationOutcome::RestoringPrevious {
            previous_activation_id,
        } = &entry.receipt.outcome
        else {
            anyhow::bail!("No previous build is ready to restore the update snapshot");
        };
        let previous = self.read(previous_activation_id)?;
        ensure!(
            &entry.receipt == expected
                && Self::is_qualified_previous(&entry, &previous)
                && source == Some(previous.receipt.update.source_revision.as_str())
                && version == previous.receipt.update.version,
            "The rollback startup does not match the previous qualified build"
        );
        Ok(())
    }

    pub(super) fn start_rollback(
        &self,
        failed: &UpdateActivationReceipt,
        previous_id: &str,
    ) -> Result<UpdateActivationReceipt> {
        let _lock = self.lock()?;
        let mut entry = self.read(&failed.id)?;
        let previous = self.read(previous_id)?;
        ensure!(
            &entry.receipt == failed
                && matches!(
                    entry.receipt.outcome,
                    ActivationOutcome::RecoveryRequired { .. }
                )
                && Self::is_qualified_previous(&entry, &previous),
            "The recovery handoff changed before launch"
        );
        entry.receipt.outcome = ActivationOutcome::RollbackInstalling {
            previous_activation_id: previous_id.to_owned(),
        };
        self.write(&entry)?;
        Ok(entry.receipt)
    }

    pub(super) fn rollback_failed(
        &self,
        id: &str,
        previous_id: &str,
        reason: String,
    ) -> Result<UpdateActivationReceipt> {
        self.change(
            id,
            ActivationOutcome::RollbackInstalling {
                previous_activation_id: previous_id.to_owned(),
            },
            ActivationOutcome::RecoveryRequired { reason },
        )
    }

    fn change(
        &self,
        id: &str,
        expected: ActivationOutcome,
        outcome: ActivationOutcome,
    ) -> Result<UpdateActivationReceipt> {
        let _lock = self.lock()?;
        let mut entry = self.read(id)?;
        ensure!(
            entry.receipt.outcome == expected,
            "The installation update state changed"
        );
        entry.receipt.outcome = outcome;
        self.write(&entry)?;
        Ok(entry.receipt)
    }

    fn lock(&self) -> Result<File> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(self.root.join("activation.lock"))
            .context("Another process owns the installation update journal")
    }

    fn entries(&self) -> Result<Vec<JournalEntry>> {
        fs::read_dir(&self.root)?
            .filter_map(|entry| {
                let path = match entry {
                    Ok(entry) => entry.path(),
                    Err(error) => return Some(Err(error.into())),
                };
                if path.extension().and_then(|extension| extension.to_str()) != Some("activation") {
                    return None;
                }
                Some(
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .context("Invalid update receipt filename")
                        .and_then(|id| self.read(id)),
                )
            })
            .collect()
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        let id = Uuid::parse_str(id).context("Invalid update activation ID")?;
        Ok(self.root.join(format!("{id}.activation")))
    }

    fn read(&self, id: &str) -> Result<JournalEntry> {
        let path = self.path(id)?;
        let mut cipher = Vec::new();
        open_locked_local_file(&path)?
            .take(MAX_RECEIPT_BYTES + 1)
            .read_to_end(&mut cipher)?;
        ensure!(
            cipher.len() as u64 <= MAX_RECEIPT_BYTES,
            "The installation update receipt is too large"
        );
        let entry: JournalEntry =
            serde_json::from_slice(&self.identity.open_record(JOURNAL_DOMAIN, &cipher)?)?;
        ensure!(
            entry.receipt.id == id && entry.receipt.installation_id == self.identity.id(),
            "The update receipt belongs to a different installation"
        );
        Ok(entry)
    }

    fn write(&self, entry: &JournalEntry) -> Result<()> {
        let cipher = self
            .identity
            .seal_record(JOURNAL_DOMAIN, &serde_json::to_vec(entry)?)?;
        ensure!(
            cipher.len() as u64 <= MAX_RECEIPT_BYTES,
            "The installation update receipt is too large"
        );
        Ok(atomic_file::replace(
            &self.path(&entry.receipt.id)?,
            &cipher,
        )?)
    }
}

pub(super) fn running_source() -> Option<&'static str> {
    let commit = env!("OMNIAGENT_BUILD_COMMIT");
    (!commit.is_empty() && env!("OMNIAGENT_BUILD_DIRTY") == "false").then_some(commit)
}
