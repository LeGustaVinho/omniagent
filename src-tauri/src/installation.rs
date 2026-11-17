use std::{
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    mcp_bridge::current_user_sid, protection, providers::configuration::open_locked_local_file,
};

pub(crate) mod atomic_file;

const IDENTITY_FILE: &str = "installation.dpapi";
const IDENTITY_FORMAT: u16 = 1;
const MANIFEST_DOMAIN: &[u8] = b"backup-manifest-v1";

#[derive(Clone)]
pub struct InstallationIdentity {
    app_data: PathBuf,
    record: IdentityRecord,
}

impl fmt::Debug for InstallationIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallationIdentity")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityRecord {
    format_version: u16,
    id: String,
    user_sid: String,
    secret: [u8; 32],
}

impl InstallationIdentity {
    pub fn load_or_create(app_data: &Path) -> Result<Self> {
        fs::create_dir_all(app_data).context("Create the application data directory")?;
        let root =
            dunce::canonicalize(app_data).context("Resolve the application data directory")?;
        let path = root.join(IDENTITY_FILE);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Self::load(&root),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Read the installed OmniAgent identity"),
        }

        let mut secret = [0; 32];
        for part in secret.chunks_exact_mut(16) {
            part.copy_from_slice(Uuid::new_v4().as_bytes());
        }
        let record = IdentityRecord {
            format_version: IDENTITY_FORMAT,
            id: Uuid::new_v4().to_string(),
            user_sid: current_user_sid().context("Identify the current Windows user")?,
            secret,
        };
        let sealed = protection::protect(&serde_json::to_vec(&record)?)
            .context("Protect the installation identity for the current Windows user")?;
        match atomic_file::write_new(&path, &sealed) {
            Ok(()) => Ok(Self {
                app_data: root,
                record,
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Self::load(&root),
            Err(error) => Err(error).context("Persist the installation identity"),
        }
    }

    pub fn load(app_data: &Path) -> Result<Self> {
        let root =
            dunce::canonicalize(app_data).context("Resolve the application data directory")?;
        let sealed = read_metadata(&root.join(IDENTITY_FILE))
            .context("Read the installed OmniAgent identity")?;
        let record: IdentityRecord = serde_json::from_slice(
            &protection::unprotect(&sealed)
                .context("The installation identity cannot be opened by this Windows profile")?,
        )
        .context("Read the protected installation identity")?;
        ensure!(
            record.format_version == IDENTITY_FORMAT,
            "Unsupported installation identity format"
        );
        require_canonical_id(&record.id)?;
        ensure!(
            record.user_sid == current_user_sid().context("Identify the current Windows user")?,
            "The installation identity belongs to another Windows profile"
        );
        Ok(Self {
            app_data: root,
            record,
        })
    }

    pub fn id(&self) -> &str {
        &self.record.id
    }

    pub fn app_data(&self) -> &Path {
        &self.app_data
    }

    pub(crate) fn require_current(&self) -> Result<()> {
        let installed = Self::load(self.app_data())?;
        ensure!(
            installed.id() == self.id()
                && bool::from(installed.record.secret.ct_eq(&self.record.secret)),
            "The installation identity changed while the app was running"
        );
        Ok(())
    }

    pub fn seal_manifest(&self, manifest: &[u8]) -> Result<Vec<u8>> {
        self.seal_record(MANIFEST_DOMAIN, &Sha256::digest(manifest))
    }

    pub fn verify_manifest(&self, manifest: &[u8], seal: &[u8]) -> Result<()> {
        let saved = self.open_record(MANIFEST_DOMAIN, seal)?;
        ensure!(
            bool::from(saved.as_slice().ct_eq(Sha256::digest(manifest).as_slice())),
            "The backup manifest does not match its protected digest"
        );
        Ok(())
    }

    pub(crate) fn seal_record(&self, domain: &[u8], data: &[u8]) -> Result<Vec<u8>> {
        let mut record = self.prefix(domain);
        record.extend_from_slice(data);
        protection::protect(&record).context("Protect the installation record")
    }

    pub(crate) fn open_record(&self, domain: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        let record = protection::unprotect(sealed)
            .context("This protected record cannot be opened by the current Windows profile")?;
        let prefix = self.prefix(domain);
        ensure!(
            record
                .get(..prefix.len())
                .is_some_and(|actual| bool::from(actual.ct_eq(&prefix))),
            "The protected record belongs to another installation or purpose"
        );
        Ok(record[prefix.len()..].to_vec())
    }

    fn prefix(&self, domain: &[u8]) -> Vec<u8> {
        let mut prefix = b"OmniAgent installation record\0".to_vec();
        prefix.extend_from_slice(domain);
        prefix.push(0);
        prefix.extend_from_slice(self.record.id.as_bytes());
        prefix.extend_from_slice(&self.record.secret);
        prefix
    }
}

pub(crate) fn require_canonical_id(id: &str) -> Result<()> {
    let parsed = Uuid::try_parse(id).context("Invalid installation record identifier")?;
    ensure!(
        parsed.to_string() == id,
        "Noncanonical installation record identifier"
    );
    Ok(())
}

pub(crate) fn read_metadata(path: &Path) -> io::Result<Vec<u8>> {
    const LIMIT: u64 = 64 * 1024;
    let file = open_locked_local_file(path)?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid installation metadata size",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
