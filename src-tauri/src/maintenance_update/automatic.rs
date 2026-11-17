use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::sync::{Notify, watch};

use crate::{
    diagnostics, installation::InstallationIdentity, jobs::JobScheduler,
    restore::InstallationLayout, workflows::maintenance::MaintenanceActivationMode,
};

use super::{automatic_policy::AutomaticUpdateTarget, *};

const POLL_INTERVAL: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AutomaticUpdateStatus {
    Checking,
    Disabled,
    TrustUnavailable,
    ReviewSettings,
    AwaitingRecovery,
    NoAcceptedHandoff,
    AlreadyAttempted,
    NoUpdate,
    NoMatchingHandoff {
        version: String,
    },
    VerificationRejected {
        version: String,
        reason: String,
    },
    PreparingInstall {
        version: String,
        handoff_id: String,
    },
    Deferred {
        reason: String,
    },
    InstallFailed {
        version: String,
        receipt_id: String,
        reason: String,
    },
    InstallationStarted {
        version: String,
        receipt_id: String,
    },
}

pub struct AutomaticUpdateMonitor {
    status: watch::Receiver<AutomaticUpdateStatus>,
    wake: Arc<Notify>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl AutomaticUpdateMonitor {
    pub fn start<R: Runtime>(
        app: AppHandle<R>,
        store: Store,
        identity: InstallationIdentity,
        scheduler: JobScheduler,
        layout: InstallationLayout,
    ) -> Self {
        let (changes, status) = watch::channel(AutomaticUpdateStatus::Checking);
        let wake = Arc::new(Notify::new());
        let task = tauri::async_runtime::spawn({
            let wake = wake.clone();
            async move {
                let mut poller = Poller {
                    store,
                    identity,
                    scheduler,
                    layout,
                    rejects: VerificationFailures::default(),
                };
                loop {
                    let status = poller.poll(&app, &changes).await.unwrap_or_else(|error| {
                        AutomaticUpdateStatus::Deferred {
                            reason: diagnostics::preview(&error.to_string()),
                        }
                    });
                    changes.send_replace(status);
                    tokio::select! {
                        _ = wake.notified() => {}
                        _ = tokio::time::sleep(POLL_INTERVAL) => {}
                    }
                }
            }
        });
        Self { status, wake, task }
    }

    pub fn status(&self) -> AutomaticUpdateStatus {
        self.status.borrow().clone()
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }
}

impl Drop for AutomaticUpdateMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Poller {
    store: Store,
    identity: InstallationIdentity,
    scheduler: JobScheduler,
    layout: InstallationLayout,
    rejects: VerificationFailures,
}

impl Poller {
    async fn poll<R: Runtime>(
        &mut self,
        app: &AppHandle<R>,
        changes: &watch::Sender<AutomaticUpdateStatus>,
    ) -> Result<AutomaticUpdateStatus> {
        let policy = self
            .store
            .automatic_update_policy(self.identity.clone())
            .await?;
        let Some(target) = policy.target else {
            return Ok(AutomaticUpdateStatus::Disabled);
        };
        let revision = policy
            .revision
            .context("The automatic update grant has no revision")?;
        let Some(trust) = TrustedUpdateFeed::application()? else {
            return Ok(AutomaticUpdateStatus::TrustUnavailable);
        };
        let journal = UpdateJournal::open(&self.identity)?;
        let state = journal.automatic_state()?;
        if state.pending {
            return Ok(AutomaticUpdateStatus::AwaitingRecovery);
        }

        let maintenance = self
            .store
            .workflow_maintenance(target.project_id.clone())
            .await?;
        if !maintenance.settings.is_some_and(|settings| {
            settings.revision == target.maintenance_settings_revision
                && settings.configuration.activation == MaintenanceActivationMode::SignedUpdater
        }) {
            return Ok(AutomaticUpdateStatus::ReviewSettings);
        }
        let mut candidates = eligible_handoffs(&maintenance.handoffs, &target);
        if candidates.is_empty() {
            return Ok(AutomaticUpdateStatus::NoAcceptedHandoff);
        }
        exclude_attempted_packages(
            &mut candidates,
            &maintenance.handoffs,
            &state.attempted_handoffs,
        );
        if candidates.is_empty() {
            return Ok(AutomaticUpdateStatus::AlreadyAttempted);
        }

        let Some(advertised) = trust.fetch_advertised_update(app).await? else {
            return Ok(AutomaticUpdateStatus::NoUpdate);
        };
        let version = advertised.version.clone();
        let comment_source = match signed_comment_source(&advertised) {
            Ok(source) => source,
            Err(error) => {
                return Ok(AutomaticUpdateStatus::VerificationRejected {
                    version,
                    reason: diagnostics::preview(&error.to_string()),
                });
            }
        };
        candidates.retain(|handoff| matches_advertisement(handoff, &version, &comment_source));
        if candidates.is_empty() {
            return Ok(AutomaticUpdateStatus::NoMatchingHandoff { version });
        }

        self.rejects.begin(&revision, &advertised);
        let package_root = self.layout.maintenance_packages();
        let mut last_rejection = None;
        for handoff in candidates {
            if let Some(reason) = self.rejects.reasons.get(&handoff.id) {
                last_rejection = Some(reason.clone());
                continue;
            }
            let request = MaintenanceActivationRequest {
                project_id: target.project_id.clone(),
                handoff_id: handoff.id.clone(),
                expected_settings_revision: target.maintenance_settings_revision.clone(),
                expected_sha256: handoff.sha256.clone(),
            };
            match VerifiedMaintenanceUpdate::prepare_advertised(
                &self.store,
                request,
                package_root.clone(),
                advertised.clone(),
            )
            .await
            {
                Ok(verified) => {
                    changes.send_replace(AutomaticUpdateStatus::PreparingInstall {
                        version: version.clone(),
                        handoff_id: handoff.id.clone(),
                    });
                    let receipt = self.install(verified, package_root, revision).await?;
                    return Ok(receipt_status(receipt));
                }
                Err(error) => {
                    let reason = diagnostics::preview(&error.to_string());
                    self.rejects
                        .reasons
                        .insert(handoff.id.clone(), reason.clone());
                    last_rejection = Some(reason);
                }
            }
        }
        Ok(AutomaticUpdateStatus::VerificationRejected {
            version,
            reason: last_rejection.context("No eligible package passed verification")?,
        })
    }

    async fn install(
        &self,
        verified: VerifiedMaintenanceUpdate,
        package_root: PathBuf,
        revision: String,
    ) -> Result<UpdateActivationReceipt> {
        let backup_parent = self.layout.app_data().join("update-backups");
        tokio::fs::create_dir_all(&backup_parent)
            .await
            .context("Create the pre-update backup directory")?;
        verified
            .install_after_drain_automatically(
                &self.store,
                &self.identity,
                &self.scheduler,
                package_root,
                backup_parent,
                revision,
            )
            .await
    }
}

fn eligible_handoffs<'a>(
    handoffs: &'a [MaintenanceHandoff],
    target: &AutomaticUpdateTarget,
) -> Vec<&'a MaintenanceHandoff> {
    // Workflow maintenance lists handoffs in a stable order.
    handoffs
        .iter()
        .filter(|handoff| {
            handoff.project_id == target.project_id
                && handoff.settings_revision == target.maintenance_settings_revision
                && handoff.activation.is_none()
                && handoff.signed_activation.is_none()
        })
        .collect()
}

fn exclude_attempted_packages(
    candidates: &mut Vec<&MaintenanceHandoff>,
    history: &[MaintenanceHandoff],
    attempted: &HashSet<String>,
) {
    let attempted_packages: HashSet<_> = history
        .iter()
        .filter(|handoff| attempted.contains(&handoff.id))
        .map(package_identity)
        .collect();
    candidates.retain(|handoff| {
        !attempted.contains(&handoff.id) && !attempted_packages.contains(&package_identity(handoff))
    });
}

fn normalized_version(handoff: &MaintenanceHandoff) -> &str {
    handoff
        .version
        .strip_prefix('v')
        .unwrap_or(&handoff.version)
}

fn matches_advertisement(handoff: &MaintenanceHandoff, version: &str, source: &str) -> bool {
    normalized_version(handoff) == version && handoff.source_revision == source
}

fn package_identity(handoff: &MaintenanceHandoff) -> (&str, &str, &str) {
    (
        normalized_version(handoff),
        &handoff.source_revision,
        &handoff.sha256,
    )
}

fn signed_comment_source(update: &Update) -> Result<String> {
    // The comment only narrows candidates. Tauri still verifies it
    // cryptographically with the bytes before it can authorize installation.
    let signature = Signature::decode(&decode_text(&update.signature)?)?;
    Ok(single_signed_field(signature.trusted_comment(), "source:")?.to_owned())
}

#[derive(Default)]
struct VerificationFailures {
    release: Option<(String, [u8; 32])>,
    reasons: HashMap<String, String>,
}

impl VerificationFailures {
    fn begin(&mut self, policy_revision: &str, update: &Update) {
        let mut release = Sha256::new();
        release.update(update.version.as_bytes());
        // A corrected HTTPS URL can requalify the same signature. Rotating
        // tokens on an already valid URL do not invalidate a rejection.
        release.update([0, u8::from(require_https(&update.download_url).is_ok())]);
        release.update(update.signature.as_bytes());
        let key = (policy_revision.to_owned(), release.finalize().into());
        if self.release.as_ref() != Some(&key) {
            self.release = Some(key);
            self.reasons.clear();
        }
    }
}

fn receipt_status(receipt: UpdateActivationReceipt) -> AutomaticUpdateStatus {
    let version = receipt.update.version;
    let receipt_id = receipt.id;
    match receipt.outcome {
        ActivationOutcome::InstallFailed { reason } => AutomaticUpdateStatus::InstallFailed {
            version,
            receipt_id,
            reason,
        },
        ActivationOutcome::Installing => AutomaticUpdateStatus::InstallationStarted {
            version,
            receipt_id,
        },
        _ => AutomaticUpdateStatus::Deferred {
            reason: "The installation journal changed before the automatic update completed".into(),
        },
    }
}
