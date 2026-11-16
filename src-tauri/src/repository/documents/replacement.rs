use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    ptr,
};

use anyhow::{Context, Result, ensure};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::GENERIC_READ,
    Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
        FILE_RENAME_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo, FileRenameInfo,
        GetFileInformationByHandleEx, SetFileInformationByHandle,
    },
};

use super::{OpenedFile, RepositoryDocumentRecovery, hash, read_file, read_open_file};

mod movement;
pub(super) use movement::{FileMove, move_exact};

pub(super) enum FileSave {
    Verified {
        file: OpenedFile,
        recovery: Option<RecoveryFiles>,
    },
    Conflict(RepositoryDocumentRecovery),
    RecoveryRequired(RepositoryDocumentRecovery),
}

#[derive(PartialEq, Eq)]
pub(super) enum ReplacementStep {
    BeforeDisplacement,
    BeforeInstall,
}

pub(super) struct RecoveryFiles {
    destination: PathBuf,
    edited: PathBuf,
    displaced: Option<PathBuf>,
    _directories: Vec<File>,
}

impl RecoveryFiles {
    pub fn describe(&self, message: impl Into<String>) -> RepositoryDocumentRecovery {
        RepositoryDocumentRecovery {
            destination_path: self.destination.clone(),
            message: message.into(),
            edited_path: Some(self.edited.clone()),
            displaced_path: self.displaced.clone(),
        }
    }

    pub fn clean_verified(&self) {
        let _ = fs::remove_file(&self.edited);
        if let Some(path) = &self.displaced {
            let _ = fs::remove_file(path);
        }
    }
}

pub(super) fn replace(
    root: &Path,
    destination: &Path,
    expected: &str,
    edited: &[u8],
    max: u64,
    observe_step: impl Fn(ReplacementStep) -> Result<()>,
) -> Result<FileSave> {
    ensure!(
        destination.starts_with(root),
        "the Markdown destination is outside its checkout"
    );
    let parent = destination
        .parent()
        .context("the Markdown document has no parent")?;
    let directories = lock_directories(parent)?;
    let staged = stage(parent, ".pending", edited)?;
    let mut recovery = RecoveryFiles {
        destination: destination.to_owned(),
        edited: staged.clone(),
        displaced: None,
        _directories: directories,
    };
    let current = match read_file(destination, max) {
        Ok(Some(current)) if hash(&current.bytes) == expected => current,
        Ok(_) => {
            return Ok(FileSave::Conflict(recovery.describe(
                "the document changed or was removed before replacement",
            )));
        }
        Err(error) => {
            return Ok(FileSave::RecoveryRequired(recovery.describe(format!(
                "could not validate the document before replacement: {error}"
            ))));
        }
    };
    if current.bytes == edited {
        recovery.clean_verified();
        return Ok(FileSave::Verified {
            file: current,
            recovery: None,
        });
    }
    // Keep an independent copy. The staged file can be moved to the destination before a
    // verification failure, and a subsequent writer can then modify the new document.
    recovery.edited = stage(parent, ".edited", edited).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })?;
    let displaced = parent.join(format!(".omniagent-markdown-{}.displaced", Uuid::new_v4()));
    let result = (|| -> Result<Option<OpenedFile>> {
        observe_step(ReplacementStep::BeforeDisplacement)?;
        let source = open_for_rename(destination)?;
        let replacement = open_for_rename(&staged)?;
        let replacement_id = file_identity(&replacement)?;
        // Rename by no-follow handles with no destination overwrite. If a new entry appears
        // between the renames, keep that entry and both known versions for recovery.
        rename(&source, &displaced).context("could not back up the current document")?;
        recovery.displaced = Some(displaced.clone());
        let backup = read_open_file(source.try_clone()?, max)?;
        if hash(&backup.bytes) != expected {
            // Restore the external edit if the filename is still available. Retain a
            // separate backup so its exact contents survive later changes to the document.
            if let Ok(conflicting) = stage(parent, ".displaced", &backup.bytes) {
                if rename(&source, destination).is_ok() {
                    recovery.displaced = Some(conflicting);
                } else {
                    let _ = fs::remove_file(conflicting);
                }
            }
            return Ok(None);
        }
        observe_step(ReplacementStep::BeforeInstall)?;
        rename(&replacement, destination).context("could not install the replacement")?;
        drop(source);
        drop(replacement);
        let written = read_file(destination, max)?.context("the replacement is unavailable")?;
        ensure!(
            written.bytes == edited && written.identity == replacement_id,
            "the saved file changed during verification"
        );
        Ok(Some(written))
    })();
    // The independent recovery file has the staged content if the first or second rename failed.
    let _ = fs::remove_file(staged);
    Ok(match result {
        Ok(Some(file)) => FileSave::Verified {
            file,
            recovery: Some(recovery),
        },
        Ok(None) => FileSave::Conflict(
            recovery.describe("the document changed during replacement; both versions were kept"),
        ),
        Err(error) => FileSave::RecoveryRequired(recovery.describe(format!(
            "could not verify the Markdown replacement: {error:#}"
        ))),
    })
}

fn stage(parent: &Path, suffix: &str, bytes: &[u8]) -> Result<PathBuf> {
    let mut temp = tempfile::Builder::new()
        .prefix(".omniagent-markdown-")
        .suffix(suffix)
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    let (file, path) = temp.keep()?;
    drop(file);
    Ok(path)
}

fn lock_directories(path: &Path) -> Result<Vec<File>> {
    let mut parents = path.ancestors().collect::<Vec<_>>();
    parents.reverse();
    parents
        .into_iter()
        .map(|path| {
            let directory = OpenOptions::new()
                .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            let metadata = directory.metadata()?;
            ensure!(
                metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                "the Markdown parent directory changed or is a reparse point"
            );
            Ok(directory)
        })
        .collect()
}

fn open_for_rename(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "the Markdown document changed into a link or non-file"
    );
    Ok(file)
}

fn rename(file: &File, name: &Path) -> io::Result<()> {
    let name = name.as_os_str().encode_wide().collect::<Vec<_>>();
    let name_offset = offset_of!(FILE_RENAME_INFO, FileName);
    let name_length = name.len() * size_of::<u16>();
    let bytes = (name_offset + name_length + size_of::<u16>()).max(size_of::<FILE_RENAME_INFO>());
    let mut buffer = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // The buffer is suitably aligned and includes the variable-length UTF-16 name.
    let renamed = unsafe {
        (*info).FileNameLength = name_length as u32;
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            ptr::addr_of_mut!((*info).FileName).cast(),
            name.len(),
        );
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfo,
            info.cast(),
            bytes as u32,
        )
    };
    if renamed == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn file_identity(file: &File) -> io::Result<String> {
    let mut identity = FILE_ID_INFO::default();
    let loaded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            ptr::from_mut(&mut identity).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if loaded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(format!(
        "{:016x}:{:032x}",
        identity.VolumeSerialNumber,
        u128::from_be_bytes(identity.FileId.Identifier)
    ))
}
