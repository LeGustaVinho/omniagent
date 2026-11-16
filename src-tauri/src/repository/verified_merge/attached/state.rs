use std::{
    fs::{self, File, OpenOptions},
    io::{self, Seek},
    mem::size_of,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_DISPOSITION_INFO, FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, FileDispositionInfo, SetFileInformationByHandle,
};

use crate::{
    providers::configuration::open_locked_local_file,
    repository::documents::{
        file_identity, lock_directories, open_directory_for_rename, open_for_rename, rename,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileStamp {
    identity: String,
    contents: String,
}

impl FileStamp {
    fn capture(file: &mut File) -> Result<Self> {
        let identity = file_identity(file)?;
        let mut digest = Sha256::new();
        file.rewind()?;
        io::copy(file, &mut digest)?;
        Ok(Self {
            identity,
            contents: format!("{:x}", digest.finalize()),
        })
    }

    pub(super) fn inspect(path: &Path) -> Result<Option<Self>> {
        let Some(_parents) = pin_existing_parent(path)? else {
            return Ok(None);
        };
        if !exists(path)? {
            return Ok(None);
        }
        Self::capture(&mut open_locked_local_file(path)?).map(Some)
    }

    pub(super) fn pin(&self, path: &Path) -> Result<File> {
        let mut file = open_locked_local_file(path)?;
        ensure!(
            Self::capture(&mut file)? == *self,
            "{} changed after its merge replacement",
            path.display()
        );
        Ok(file)
    }

    pub(super) fn same_contents(&self, other: &Self) -> bool {
        self.contents == other.contents
    }
}

fn pin_existing_parent(path: &Path) -> Result<Option<Vec<File>>> {
    match lock_directories(path.parent().context("the merge path has no parent")?) {
        Ok(handles) => Ok(Some(handles)),
        Err(error)
            if error.downcast_ref::<io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                )
            }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn delete_exact(handle: &mut File, expected: &FileStamp) -> Result<()> {
    ensure!(
        &FileStamp::capture(handle)? == expected,
        "the retained backup changed before cleanup"
    );
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    let deleted = unsafe {
        SetFileInformationByHandle(
            handle.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::from_ref(&info).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if deleted == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

pub(super) fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn lock_index(git_dir: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ)
        .create_new(true)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(git_dir.join("index.lock"))
        .context("the destination index is locked by another Git operation")
}

pub(super) fn stage_file(source: &Path, staged: &Path) -> Result<FileStamp> {
    let mut open = open_locked_local_file(source)?;
    let mut copy = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(staged)?;
    io::copy(&mut open, &mut copy)?;
    copy.sync_all()?;
    FileStamp::capture(&mut copy)
}

pub(super) fn directory_identity(path: &Path) -> Result<String> {
    let handles = lock_directories(path)?;
    Ok(file_identity(
        handles
            .last()
            .context("destination directory has no handle")?,
    )?)
}

fn inspect_directory(path: &Path) -> Result<Option<String>> {
    let Some(_parents) = pin_existing_parent(path)? else {
        return Ok(None);
    };
    if !exists(path)? {
        return Ok(None);
    }
    directory_identity(path).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Progress {
    Pending,
    Displaced,
    Applied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileState {
    pub before: Option<FileStamp>,
    pub after: Option<FileStamp>,
}

impl FileState {
    pub(super) fn completed_backup(&self, staged: &Path, backup: &Path) -> Result<bool> {
        Ok(!exists(staged)? && FileStamp::inspect(backup)? == self.before)
    }

    pub(super) fn inspect(&self, live: &Path, staged: &Path, backup: &Path) -> Result<Progress> {
        let current = FileStamp::inspect(live)?;
        let staged_stamp = FileStamp::inspect(staged)?;
        let backup_stamp = FileStamp::inspect(backup)?;
        if current == self.before && staged_stamp == self.after && backup_stamp.is_none() {
            return Ok(Progress::Pending);
        }
        if current == self.after && staged_stamp.is_none() && backup_stamp == self.before {
            return Ok(Progress::Applied);
        }
        if current.is_none() && staged_stamp == self.after && backup_stamp == self.before {
            return Ok(Progress::Displaced);
        }
        anyhow::bail!(
            "{} or its recovery files changed outside the merge",
            live.display()
        )
    }

    pub(super) fn apply(&self, live: &Path, staged: &Path, backup: &Path) -> Result<()> {
        let progress = self.inspect(live, staged, backup)?;
        if progress == Progress::Applied {
            return Ok(());
        }
        let _parent = lock_directories(live.parent().context("changed path has no parent")?)?;
        if progress == Progress::Pending {
            if let Some(before) = &self.before {
                move_exact(live, backup, before)
                    .context("could not preserve the current destination file")?;
            }
        }
        if let Some(after) = &self.after {
            move_exact(staged, live, after)
                .context("could not install the reviewed destination file")?;
        }
        ensure!(
            self.inspect(live, staged, backup)? == Progress::Applied,
            "could not verify the reviewed destination file at {}",
            live.display()
        );
        Ok(())
    }

    pub(super) fn pin_applied(
        &self,
        live: &Path,
        staged: &Path,
        backup: &Path,
    ) -> Result<Option<File>> {
        ensure!(
            self.inspect(live, staged, backup)? == Progress::Applied,
            "the reviewed destination file was not installed at {}",
            live.display()
        );
        self.after.as_ref().map(|value| value.pin(live)).transpose()
    }
}

fn move_exact(source: &Path, target: &Path, expected: &FileStamp) -> Result<()> {
    let mut handle = open_for_rename(source)?;
    ensure!(
        &FileStamp::capture(&mut handle)? == expected,
        "{} changed before its merge replacement",
        source.display()
    );
    rename(&handle, target)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct NewDirectory {
    pub path: String,
    pub identity: String,
}

impl NewDirectory {
    pub(super) fn inspect(&self, live: &Path, staged: &Path) -> Result<Progress> {
        let current = inspect_directory(live)?;
        let scratch = inspect_directory(staged)?;
        if current.is_none() && scratch.as_ref() == Some(&self.identity) {
            return Ok(Progress::Pending);
        }
        if current.as_ref() == Some(&self.identity) && scratch.is_none() {
            return Ok(Progress::Applied);
        }
        anyhow::bail!(
            "{} was created or replaced outside the merge",
            live.display()
        )
    }

    pub(super) fn install(&self, live: &Path, staged: &Path) -> Result<()> {
        if self.inspect(live, staged)? == Progress::Pending {
            let _parent = lock_directories(live.parent().context("new directory has no parent")?)?;
            let handle = open_directory_for_rename(staged)?;
            ensure!(
                file_identity(&handle)? == self.identity,
                "a prepared directory changed before it was installed"
            );
            rename(&handle, live)?;
            drop(handle);
        }
        ensure!(
            self.inspect(live, staged)? == Progress::Applied,
            "could not verify the new directory at {}",
            live.display()
        );
        Ok(())
    }
}
