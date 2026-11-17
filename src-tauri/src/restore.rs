use std::{
    fs, io,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

use crate::{
    backup::{DATABASE_FILE, read_verified_manifest},
    diagnostics,
    installation::{InstallationIdentity, atomic_file, read_metadata, require_canonical_id},
    providers::configuration::open_locked_local_file,
    store::current_schema_version,
};

mod stage;
#[cfg(test)]
mod tests;

pub use stage::{preview_restore, stage_restore};

const GENERATIONS: &str = "restored-generations";
const MAINTENANCE_PACKAGES: &str = "maintenance-packages";
const ACTIVE: &str = "active-generation.dpapi";
const PENDING: &str = "pending-restore.dpapi";
const REJECTION: &str = "rejected-restore.dpapi";

#[derive(Clone, Debug)]
pub struct InstallationLayout {
    app_data: PathBuf,
    data: PathBuf,
}

impl InstallationLayout {
    pub fn app_data(&self) -> &Path {
        &self.app_data
    }

    pub fn database_path(&self) -> PathBuf {
        self.data.join(DATABASE_FILE)
    }

    pub fn maintenance_packages(&self) -> PathBuf {
        self.data.join(MAINTENANCE_PACKAGES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreReceipt {
    pub generation_id: String,
    pub checkpoint_id: String,
    pub schema_before: u32,
    pub schema_after: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview {
    pub checkpoint_id: String,
    pub schema_version: u32,
    pub manifest_sha256: String,
    pub package_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreStatus {
    pub active: Option<RestoreReceipt>,
    pub pending: Option<RestoreReceipt>,
    pub rejection: Option<String>,
}

#[derive(Debug)]
pub struct InstallationStartup {
    pub identity: InstallationIdentity,
    pub layout: InstallationLayout,
    pub restore: RestoreStatus,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRestore {
    receipt: RestoreReceipt,
    previous_generation: Option<String>,
    staged_database_sha256: String,
}

/// Resolve a complete database and package generation before opening the live Store.
pub fn open_installation(app_data: &Path) -> Result<InstallationStartup> {
    let identity = InstallationIdentity::load_or_create(app_data)?;
    let _maintenance = identity.lock_maintenance_change()?;
    let mut active = active_receipt(&identity)?;
    if record_exists(&identity, PENDING)? {
        let pending = read_record::<PendingRestore>(&identity, PENDING)
            .and_then(|pending| pending.context("The staged restore marker is unavailable"));
        match pending.and_then(|pending| verify_pending(&identity, active.as_ref(), pending)) {
            Ok(receipt) => {
                if active.as_ref() != Some(&receipt) {
                    replace_record(&identity, ACTIVE, &receipt)
                        .context("Select the verified restore generation")?;
                    active = Some(receipt);
                }
                atomic_file::remove_record(&identity.app_data().join(REJECTION))?;
            }
            Err(error) => {
                let reason = diagnostics::preview(&format!("{error:#}"));
                replace_record(&identity, REJECTION, &reason)?;
            }
        }
        atomic_file::remove_record(&identity.app_data().join(PENDING))
            .context("Settle the pending restore marker")?;
    }
    let layout = layout(&identity, active.as_ref())?;
    let restore = RestoreStatus {
        active,
        pending: None,
        rejection: read_record(&identity, REJECTION)?,
    };
    Ok(InstallationStartup {
        identity,
        layout,
        restore,
    })
}

pub fn restore_status(identity: &InstallationIdentity) -> Result<RestoreStatus> {
    identity.require_current()?;
    let pending = read_record::<PendingRestore>(identity, PENDING)?;
    Ok(RestoreStatus {
        active: active_receipt(identity)?,
        pending: pending.map(|state| state.receipt),
        rejection: read_record(identity, REJECTION)?,
    })
}

fn verify_pending(
    identity: &InstallationIdentity,
    active: Option<&RestoreReceipt>,
    pending: PendingRestore,
) -> Result<RestoreReceipt> {
    let receipt = pending.receipt;
    validate_receipt(&receipt)?;
    // The previous process may have published the pointer just before it exited.
    // The selected database is writable from that instant, so only staged data has a fixed hash.
    if active == Some(&receipt) {
        return Ok(receipt);
    }
    ensure!(
        active.map(|state| &state.generation_id) == pending.previous_generation.as_ref(),
        "The active data generation changed after this restore was staged"
    );
    ensure!(
        receipt.schema_after == current_schema_version(),
        "The application schema changed after this restore was staged. Preview and stage this backup again."
    );
    let root = layout(identity, Some(&receipt))?.data;
    stage::require_standalone_database(&root.join(DATABASE_FILE))?;
    let manifest = read_verified_manifest(&root, identity)
        .context("Revalidate the private restore generation")?;
    ensure!(
        manifest.checkpoint_id == receipt.checkpoint_id
            && manifest.schema_version == receipt.schema_after
            && manifest.settings_revision == pending.staged_database_sha256,
        "The staged generation no longer matches its restore receipt"
    );
    Ok(receipt)
}

fn active_receipt(identity: &InstallationIdentity) -> Result<Option<RestoreReceipt>> {
    let selected = read_record::<RestoreReceipt>(identity, ACTIVE)
        .context("Read the active application data selection")?;
    if let Some(receipt) = &selected {
        validate_receipt(receipt)?;
    }
    Ok(selected)
}

fn validate_receipt(receipt: &RestoreReceipt) -> Result<()> {
    require_canonical_id(&receipt.generation_id)?;
    require_canonical_id(&receipt.checkpoint_id)?;
    ensure!(
        receipt.schema_before > 0 && receipt.schema_after >= receipt.schema_before,
        "Invalid schema revisions in restore receipt"
    );
    Ok(())
}

fn layout(
    identity: &InstallationIdentity,
    active: Option<&RestoreReceipt>,
) -> Result<InstallationLayout> {
    let app_data = identity.app_data().to_owned();
    let data = if let Some(receipt) = active {
        let target = generation_path(identity, &receipt.generation_id)?;
        require_plain_directory(&target)?;
        open_locked_local_file(&target.join(DATABASE_FILE))
            .context("Open the active restored database")?;
        target
    } else {
        app_data.clone()
    };
    let packages = data.join(MAINTENANCE_PACKAGES);
    match fs::symlink_metadata(&packages) {
        Ok(_) => require_plain_directory(&packages)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Open the active maintenance package directory"),
    }
    Ok(InstallationLayout { app_data, data })
}

fn generation_path(identity: &InstallationIdentity, id: &str) -> Result<PathBuf> {
    require_canonical_id(id)?;
    let parent = identity.app_data().join(GENERATIONS);
    require_plain_directory(&parent)?;
    Ok(parent.join(id))
}

fn require_plain_directory(path: &Path) -> Result<()> {
    let info = fs::symlink_metadata(path).context("Open the private restore directory")?;
    ensure!(
        info.is_dir() && info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "The private restore directory changed"
    );
    Ok(())
}

fn record_exists(identity: &InstallationIdentity, name: &str) -> Result<bool> {
    match fs::symlink_metadata(identity.app_data().join(name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("Read the restore marker"),
    }
}

fn record_bytes<T: Serialize>(
    identity: &InstallationIdentity,
    name: &str,
    value: &T,
) -> Result<Vec<u8>> {
    identity.seal_record(name.as_bytes(), &serde_json::to_vec(value)?)
}

fn replace_record<T: Serialize>(
    identity: &InstallationIdentity,
    name: &str,
    value: &T,
) -> Result<()> {
    let bytes = record_bytes(identity, name, value)?;
    atomic_file::replace(&identity.app_data().join(name), &bytes)?;
    Ok(())
}

fn read_record<T: DeserializeOwned>(
    identity: &InstallationIdentity,
    name: &str,
) -> Result<Option<T>> {
    let sealed = match read_metadata(&identity.app_data().join(name)) {
        Ok(sealed) => sealed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Read the installation restore record"),
    };
    let contents = identity.open_record(name.as_bytes(), &sealed)?;
    Ok(Some(
        serde_json::from_slice(&contents).context("Read the protected restore state")?,
    ))
}
