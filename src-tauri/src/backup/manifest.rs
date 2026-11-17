use std::{
    collections::HashSet,
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{installation::InstallationIdentity, providers::configuration::open_locked_local_file};

use super::files;

const FORMAT_VERSION: u16 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SEAL_BYTES: u64 = 64 * 1024;
const MANIFEST_FILE: &str = "manifest.json";
const SEAL_FILE: &str = "manifest.dpapi";
pub const DATABASE_FILE: &str = "omniagent.sqlite3";
pub(super) const PACKAGES_DIRECTORY: &str = "maintenance-packages";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupManifest {
    pub format_version: u16,
    pub installation_id: String,
    pub checkpoint_id: String,
    pub schema_version: u32,
    /// The exact database image containing every durable application setting.
    pub settings_revision: String,
    pub entries: Vec<BackupEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BackupEntry {
    Database {
        size: u64,
        sha256: String,
    },
    MaintenancePackage {
        handoff_id: String,
        size: u64,
        sha256: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReceipt {
    pub root: PathBuf,
    pub manifest: BackupManifest,
    pub manifest_sha256: String,
}

impl BackupEntry {
    pub fn relative_path(&self) -> Result<PathBuf> {
        match self {
            Self::Database { .. } => Ok(DATABASE_FILE.into()),
            Self::MaintenancePackage { handoff_id, .. } => {
                Ok(Path::new(PACKAGES_DIRECTORY).join(package_filename(handoff_id)?))
            }
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::Database { size, .. } | Self::MaintenancePackage { size, .. } => *size,
        }
    }

    pub fn sha256(&self) -> &str {
        match self {
            Self::Database { sha256, .. } | Self::MaintenancePackage { sha256, .. } => sha256,
        }
    }
}

impl BackupManifest {
    pub(super) fn new(
        identity: &InstallationIdentity,
        checkpoint_id: String,
        schema_version: u32,
        entries: Vec<BackupEntry>,
    ) -> Result<Self> {
        let settings_revision = entries
            .iter()
            .find_map(|entry| match entry {
                BackupEntry::Database { sha256, .. } => Some(sha256.clone()),
                _ => None,
            })
            .context("The backup is missing its database")?;
        Ok(Self {
            format_version: FORMAT_VERSION,
            installation_id: identity.id().to_owned(),
            checkpoint_id,
            schema_version,
            settings_revision,
            entries,
        })
    }

    fn validate(&self, identity: &InstallationIdentity) -> Result<()> {
        ensure!(
            self.format_version == FORMAT_VERSION,
            "The backup format version is unsupported"
        );
        ensure!(
            self.installation_id == identity.id(),
            "The backup belongs to another installation"
        );
        require_uuid(&self.checkpoint_id)?;
        ensure!(
            self.schema_version > 0,
            "The backup has no application schema version"
        );
        let mut seen = HashSet::new();
        let mut has_database = false;
        for entry in &self.entries {
            ensure!(
                seen.insert(entry.relative_path()?),
                "The backup lists a file more than once"
            );
            ensure!(
                entry.size() > 0 && valid_sha256(entry.sha256()),
                "The backup entry has invalid size or SHA-256"
            );
            if let BackupEntry::Database { sha256, .. } = entry {
                ensure!(
                    sha256 == &self.settings_revision,
                    "The database does not match the settings checkpoint"
                );
                has_database = true;
            }
        }
        ensure!(has_database, "The backup is missing its database");
        Ok(())
    }
}

pub(super) fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn require_uuid(value: &str) -> Result<()> {
    let id = Uuid::try_parse(value).context("The backup identifier is invalid")?;
    ensure!(
        id.to_string() == value,
        "The backup identifier is not canonical"
    );
    Ok(())
}

pub(super) fn package_filename(handoff_id: &str) -> Result<String> {
    require_uuid(handoff_id)?;
    Ok(format!("{handoff_id}.package"))
}

pub(crate) fn write_manifest(
    root: &Path,
    identity: &InstallationIdentity,
    manifest: &BackupManifest,
) -> Result<String> {
    manifest.validate(identity)?;
    let bytes = serde_json::to_vec_pretty(manifest)?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "The backup manifest exceeds its size limit"
    );
    let seal = identity.seal_manifest(&bytes)?;
    ensure!(
        !seal.is_empty() && seal.len() as u64 <= MAX_SEAL_BYTES,
        "The backup authentication seal has an invalid size"
    );
    files::write_new(&root.join(MANIFEST_FILE), &bytes)?;
    files::write_new(&root.join(SEAL_FILE), &seal)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

pub fn read_verified_manifest(
    root: &Path,
    identity: &InstallationIdentity,
) -> Result<BackupManifest> {
    Ok(read_verified_manifest_with_digest(root, identity)?.0)
}

pub fn read_verified_manifest_with_digest(
    root: &Path,
    identity: &InstallationIdentity,
) -> Result<(BackupManifest, String)> {
    let root = dunce::canonicalize(root).context("Resolve the backup directory")?;
    let bytes = read_bounded(&root.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
    let seal = read_bounded(&root.join(SEAL_FILE), MAX_SEAL_BYTES)?;
    identity.verify_manifest(&bytes, &seal)?;
    let manifest: BackupManifest =
        serde_json::from_slice(&bytes).context("Read the authenticated backup manifest")?;
    manifest.validate(identity)?;
    for entry in &manifest.entries {
        let relative = entry.relative_path()?;
        let source = open_locked_local_file(&root.join(&relative))
            .with_context(|| format!("Open backup file {}", relative.display()))?;
        files::verify_contents(entry, source, &mut io::sink())?;
    }
    Ok((manifest, format!("{:x}", Sha256::digest(&bytes))))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = open_locked_local_file(path).with_context(|| format!("Open {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= limit,
        "Invalid backup metadata size"
    );
    Ok(bytes)
}
