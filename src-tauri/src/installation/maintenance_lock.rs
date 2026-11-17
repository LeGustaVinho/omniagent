use std::{
    fs::{File, OpenOptions},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
};

use anyhow::{Context, Result, ensure};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};

use super::InstallationIdentity;

impl InstallationIdentity {
    pub(crate) fn lock_maintenance_change(&self) -> Result<File> {
        self.require_current()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(0)
            .open(self.app_data().join("maintenance-change.lock"))
            .context("Another process is preparing an update or restore")?;
        let metadata = lock.metadata()?;
        ensure!(
            metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "The installation maintenance lock is not a regular file"
        );
        Ok(lock)
    }
}
