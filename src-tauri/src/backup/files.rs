use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use crate::providers::configuration::open_locked_local_file;

use super::BackupEntry;

pub(super) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = create_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn copy_verified(
    entry: &BackupEntry,
    archive_root: &Path,
    staging_root: &Path,
) -> Result<()> {
    let root = dunce::canonicalize(archive_root).context("Resolve the backup directory")?;
    let relative = entry.relative_path()?;
    let source = open_locked_local_file(&root.join(&relative))
        .with_context(|| format!("Open backup file {}", relative.display()))?;
    stage_file(entry, source, staging_root)
}

pub(super) fn stage_file(entry: &BackupEntry, source: File, root: &Path) -> Result<()> {
    let destination = root.join(entry.relative_path()?);
    fs::create_dir_all(
        destination
            .parent()
            .context("The backup file has no parent directory")?,
    )?;
    let mut target = create_file(&destination)?;
    verify_contents(entry, source, &mut target)?;
    target.sync_all()?;
    Ok(())
}

pub(super) fn verify_contents(
    entry: &BackupEntry,
    mut source: File,
    output: &mut dyn Write,
) -> Result<()> {
    ensure!(
        source.metadata()?.len() == entry.size(),
        "Backup file {} changed size",
        entry.relative_path()?.display()
    );
    let (size, sha256) = copy_digest(&mut source, output)?;
    ensure!(
        size == entry.size() && sha256 == entry.sha256(),
        "Backup file {} does not match its SHA-256",
        entry.relative_path()?.display()
    );
    Ok(())
}

pub(super) fn copy_digest(source: &mut impl Read, output: &mut dyn Write) -> Result<(u64, String)> {
    let mut buffer = [0u8; 64 * 1024];
    let mut digest = Sha256::new();
    let mut size = 0;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok((size, format!("{:x}", digest.finalize())));
        }
        let bytes = &buffer[..count];
        output.write_all(bytes)?;
        digest.update(bytes);
        size += count as u64;
    }
}

fn create_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("Create backup file {}", path.display()))
}
