use std::{fs, io, path::Path};

use anyhow::{Context, Result, ensure};

use super::{OpenedFile, hash, lock_directories, open_for_rename, read_open_file, rename};

pub(in super::super) enum FileMove {
    Moved(OpenedFile),
    Conflict(String),
}

pub(in super::super) fn move_exact(
    root: &Path,
    source: &Path,
    destination: &Path,
    sha256: &str,
    identity: &str,
    max: u64,
) -> Result<FileMove> {
    move_exact_with(root, source, destination, sha256, identity, max, || Ok(()))
}

fn move_exact_with(
    root: &Path,
    source: &Path,
    destination: &Path,
    sha256: &str,
    identity: &str,
    max: u64,
    before_rename: impl FnOnce() -> Result<()>,
) -> Result<FileMove> {
    ensure!(
        source.starts_with(root) && destination.starts_with(root),
        "the Markdown rename must stay in one checkout"
    );
    let _source_parents = lock_directories(
        source
            .parent()
            .context("the source has no parent directory")?,
    )?;
    let _destination_parents = lock_directories(
        destination
            .parent()
            .context("the destination has no parent directory")?,
    )?;
    if entry_exists(destination)? {
        return Ok(FileMove::Conflict(
            "the Markdown rename destination already exists".into(),
        ));
    }
    let handle = match open_for_rename(source) {
        Ok(handle) => handle,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(FileMove::Conflict(
                "the Markdown rename source was removed".into(),
            ));
        }
        Err(error) => return Err(error),
    };
    // Keep the no-follow handle open and deny writers until the namespace move
    // is complete. The checked bytes and the file being moved are the same.
    let file = read_open_file(handle.try_clone()?, max)?;
    if file.identity != identity || hash(&file.bytes) != sha256 {
        return Ok(FileMove::Conflict(
            "the Markdown rename source changed after review".into(),
        ));
    }
    before_rename()?;
    if let Err(error) = rename(&handle, destination) {
        if entry_exists(destination)? {
            return Ok(FileMove::Conflict(
                "the Markdown rename destination appeared after review".into(),
            ));
        }
        return Err(error).context("could not move the reviewed Markdown document");
    }
    Ok(FileMove::Moved(file))
}

fn entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
