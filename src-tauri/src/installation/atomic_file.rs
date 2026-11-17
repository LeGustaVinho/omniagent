use std::{
    fs,
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::Path,
};

use tempfile::NamedTempFile;
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staged = stage(path, bytes)?;
    let temporary = staged.into_temp_path();
    move_path(&temporary, path, MOVEFILE_WRITE_THROUGH)
}

pub(crate) fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staged = stage(path, bytes)?;
    let temporary = staged.into_temp_path();
    move_path(
        &temporary,
        path,
        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
    )
}

pub(crate) fn publish_directory(source: &Path, destination: &Path) -> io::Result<()> {
    move_path(source, destination, MOVEFILE_WRITE_THROUGH)
}

fn stage(path: &Path, bytes: &[u8]) -> io::Result<NamedTempFile> {
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Missing installation metadata directory",
        )
    })?;
    let mut temporary = NamedTempFile::new_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    Ok(temporary)
}

fn move_path(source: &Path, destination: &Path, flags: u32) -> io::Result<()> {
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut units: Vec<_> = path.as_os_str().encode_wide().collect();
        if units.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid local path",
            ));
        }
        units.push(0);
        Ok(units)
    }
    let source = wide(source)?;
    let destination = wide(destination)?;
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn remove_record(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
