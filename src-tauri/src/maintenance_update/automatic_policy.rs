use std::io;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    installation::{InstallationIdentity, atomic_file, read_metadata, require_canonical_id},
    store::Store,
    workflows::maintenance::{
        MaintenanceActivationMode, MaintenanceActivationRequest, load_settings,
    },
};

const POLICY_FILE: &str = "maintenance-automatic-update.dpapi";
const POLICY_DOMAIN: &[u8] = b"maintenance-automatic-update-policy-v1";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomaticUpdatePolicy {
    pub revision: Option<String>,
    pub target: Option<AutomaticUpdateTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomaticUpdateTarget {
    pub project_id: String,
    pub maintenance_settings_revision: String,
}

impl Store {
    pub async fn automatic_update_policy(
        &self,
        identity: InstallationIdentity,
    ) -> Result<AutomaticUpdatePolicy> {
        self.execute(move |_| read_policy(&identity)).await
    }

    pub async fn set_automatic_update_policy(
        &self,
        identity: InstallationIdentity,
        expected_revision: Option<String>,
        target: Option<AutomaticUpdateTarget>,
    ) -> Result<AutomaticUpdatePolicy> {
        self.execute(move |db| {
            let current = read_policy(&identity)?;
            ensure!(
                current.revision == expected_revision,
                "Automatic update settings changed. Review them again."
            );
            if let Some(target) = &target {
                verify_target(db, target)?;
            }
            let policy = AutomaticUpdatePolicy {
                revision: Some(Uuid::new_v4().to_string()),
                target,
            };
            let path = identity.app_data().join(POLICY_FILE);
            let sealed = identity.seal_record(POLICY_DOMAIN, &serde_json::to_vec(&policy)?)?;
            if current.revision.is_some() {
                atomic_file::replace(&path, &sealed)?;
            } else {
                atomic_file::write_new(&path, &sealed)?;
            }
            Ok(policy)
        })
        .await
    }
}

/// Revalidate the exact user grant while holding the Store writer for the installer launch.
pub(crate) fn recheck_automatic_update_policy(
    db: &Connection,
    identity: &InstallationIdentity,
    policy_revision: &str,
    request: &MaintenanceActivationRequest,
) -> Result<()> {
    let policy = read_policy(identity)?;
    ensure!(
        policy.revision.as_deref() == Some(policy_revision),
        "Automatic update permission changed before installation"
    );
    let target = policy.target.context("Automatic updates were disabled")?;
    ensure!(
        target.project_id == request.project_id
            && target.maintenance_settings_revision == request.expected_settings_revision,
        "The installer is outside the automatic update permission"
    );
    verify_target(db, &target)
}

fn verify_target(db: &Connection, target: &AutomaticUpdateTarget) -> Result<()> {
    let settings = load_settings(db, &target.project_id)?
        .context("The automatic update project has no saved maintenance settings")?;
    ensure!(
        settings.revision == target.maintenance_settings_revision,
        "The reviewed maintenance settings changed"
    );
    ensure!(
        settings.configuration.activation == MaintenanceActivationMode::SignedUpdater,
        "Enable signed updates for this maintenance workflow first"
    );
    Ok(())
}

fn read_policy(identity: &InstallationIdentity) -> Result<AutomaticUpdatePolicy> {
    identity.require_current()?;
    let sealed = match read_metadata(&identity.app_data().join(POLICY_FILE)) {
        Ok(sealed) => sealed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AutomaticUpdatePolicy::default());
        }
        Err(error) => return Err(error).context("Read automatic update settings"),
    };
    let payload = identity.open_record(POLICY_DOMAIN, &sealed)?;
    let policy: AutomaticUpdatePolicy =
        serde_json::from_slice(&payload).context("Read protected automatic update settings")?;
    require_canonical_id(
        policy
            .revision
            .as_deref()
            .context("The stored automatic update settings are missing a revision")?,
    )?;
    Ok(policy)
}

#[cfg(test)]
mod tests;
