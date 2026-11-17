use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{installation::InstallationIdentity, store::Store};

mod files;
mod manifest;
mod snapshot;

pub(crate) use manifest::write_manifest;
pub use manifest::{
    BackupEntry, BackupManifest, BackupReceipt, DATABASE_FILE, read_verified_manifest,
    read_verified_manifest_with_digest,
};
pub(crate) use snapshot::validate_backup_inventory;

impl Store {
    /// Publish a complete snapshot in a new directory under the selected destination.
    pub async fn create_application_backup(
        &self,
        identity: InstallationIdentity,
        managed_packages_root: PathBuf,
        destination_parent: PathBuf,
    ) -> Result<BackupReceipt> {
        let pinned = self
            .execute(move |writer| snapshot::pin(writer, &managed_packages_root))
            .await?;
        tokio::task::spawn_blocking(move || {
            snapshot::publish(pinned, &identity, &destination_parent)
        })
        .await
        .context("The application backup worker stopped")?
    }
}

impl BackupEntry {
    /// Verify the file again while staging a private copy for restore.
    pub fn copy_verified(&self, archive_root: &Path, staging_root: &Path) -> Result<()> {
        files::copy_verified(self, archive_root, staging_root)
    }
}

#[cfg(test)]
mod tests;
