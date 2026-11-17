use std::{
    collections::HashSet,
    fs,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
};

use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use tauri::State;
use windows_sys::Win32::{
    System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
    UI::{
        Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

use crate::{providers::configuration::plain_metadata, store::Store};

use super::{
    checkout_root, documents::relative_name, git_path, git_query, verify_checkout,
    with_managed_worktree_writer,
};

fn item_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty() {
        return Ok(root.into());
    }
    for name in relative.split('/') {
        validate_name(name)?;
    }
    let path = root.join(relative);
    relative_name(root, &path)?;
    Ok(path)
}

fn validate_name(name: &str) -> Result<()> {
    let device = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let numbered_device = device
        .strip_prefix("COM")
        .or_else(|| device.strip_prefix("LPT"))
        .is_some_and(|index| {
            matches!(
                index,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    ensure!(
        !name.is_empty()
            && !name.ends_with([' ', '.'])
            && !name.eq_ignore_ascii_case(".git")
            && !name.contains(['/', '\\', ':', '?', '*', '"', '<', '>', '|'])
            && !name.chars().any(|ch| ch < ' ')
            && !matches!(
                device.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            )
            && !numbered_device,
        "Invalid Windows checkout item name: {name}"
    );
    Ok(())
}

/// Resolve an explicit subdirectory without following junctions or nested checkouts.
pub fn checkout_directory(root: &Path, relative: &str) -> Result<PathBuf> {
    let directory = item_path(root, relative)?;
    ensure!(
        plain_metadata(&directory)?.is_some_and(|metadata| metadata.is_dir()),
        "The checkout folder no longer exists."
    );
    for parent in directory.ancestors().take_while(|parent| *parent != root) {
        ensure!(
            !parent.join(".git").symlink_metadata().is_ok(),
            "The folder belongs to a nested Git checkout."
        );
    }
    Ok(directory)
}

pub(crate) fn checkout_regular_file(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = item_path(root, relative)?;
    let (folder, _) = relative.rsplit_once('/').unwrap_or(("", relative));
    checkout_directory(root, folder)?;
    ensure!(
        plain_metadata(&path)?.is_some_and(|metadata| metadata.is_file()),
        "The checkout file no longer exists."
    );
    Ok(path)
}

async fn validated_root(store: &Store, checkout: String) -> Result<PathBuf> {
    let root = checkout_root(store, checkout.clone()).await?;
    Ok(verify_checkout(store, checkout, root).await?.root)
}

#[tauri::command]
pub async fn list_checkout_folders(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<String>, String> {
    let list = async {
        let root = validated_root(&store, checkout_id).await?;
        tokio::task::spawn_blocking(move || list_folders(&root)).await?
    };
    list.await.map_err(|error| format!("{error:#}"))
}

fn list_folders(root: &Path) -> Result<Vec<String>> {
    let ignored = git_query(
        &git_path()?,
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--directory",
            "--exclude-standard",
        ],
    )?;
    let ignored: HashSet<_> = ignored
        .split_terminator('\0')
        .filter_map(|path| path.strip_suffix('/'))
        .collect();
    let mut directories = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(parent) = pending.pop() {
        for child in fs::read_dir(parent)? {
            let child = child?;
            if !child.file_type()?.is_dir() {
                continue;
            }
            let path = child.path();
            let Ok(relative) = relative_name(root, &path) else {
                continue;
            };
            if ignored.contains(relative.as_str())
                || !plain_metadata(&path).is_ok_and(|metadata| metadata.is_some())
                || path.join(".git").symlink_metadata().is_ok()
            {
                continue;
            }
            directories.push(relative);
            pending.push(path);
        }
    }
    directories.sort_unstable();
    Ok(directories)
}

#[tauri::command]
pub async fn create_checkout_item(
    store: State<'_, Store>,
    checkout_id: String,
    relative_directory: String,
    name: String,
    directory: bool,
) -> Result<String, String> {
    let create = async {
        let root = validated_root(&store, checkout_id.clone()).await?;
        let managed = store
            .execute({
                let checkout_id = checkout_id.clone();
                move |db| {
                    Ok(db.query_row(
                "SELECT project_id, task_id FROM managed_task_worktrees WHERE checkout_id = ?1",
                [checkout_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ).optional()?)
                }
            })
            .await?;
        let write = move || create_item(&root, &relative_directory, &name, directory);
        if let Some((project, task)) = managed {
            with_managed_worktree_writer(&store, project, checkout_id, task, move |_| write()).await
        } else {
            tokio::task::spawn_blocking(write).await?
        }
    };
    create.await.map_err(|error| format!("{error:#}"))
}

fn create_item(
    root: &Path,
    relative_directory: &str,
    name: &str,
    directory: bool,
) -> Result<String> {
    validate_name(name)?;
    let parent = checkout_directory(root, relative_directory)?;
    let path = parent.join(name);
    let relative = relative_name(root, &path)?;
    if directory {
        fs::create_dir(path)?;
    } else {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
    }
    Ok(relative)
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellAction {
    Reveal,
    Open,
}

#[tauri::command]
pub async fn open_checkout_item_in_shell(
    store: State<'_, Store>,
    checkout_id: String,
    relative_path: String,
    action: ShellAction,
) -> Result<(), String> {
    let open = async {
        let root = validated_root(&store, checkout_id).await?;
        tokio::task::spawn_blocking(move || {
            let path = item_path(&root, &relative_path)?;
            let metadata = plain_metadata(&path)?.context("The checkout item no longer exists.")?;
            shell_item(&path, metadata.is_dir(), action)
        })
        .await?
    };
    open.await.map_err(|error| format!("{error:#}"))
}

struct Apartment;

impl Apartment {
    fn initialize() -> Result<Self> {
        let result = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        ensure!(
            result >= 0,
            "Windows shell initialization failed ({result:#x})."
        );
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn shell_item(path: &Path, directory: bool, action: ShellAction) -> Result<()> {
    let _apartment = Apartment::initialize()?;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    if matches!(action, ShellAction::Reveal) && !directory {
        let mut item = ptr::null_mut();
        let parsed = unsafe {
            SHParseDisplayName(
                path.as_ptr(),
                ptr::null_mut(),
                &mut item,
                0,
                ptr::null_mut(),
            )
        };
        ensure!(
            parsed >= 0 && !item.is_null(),
            "Windows could not locate the file ({parsed:#x})."
        );
        let result = unsafe { SHOpenFolderAndSelectItems(item, 0, ptr::null(), 0) };
        unsafe { CoTaskMemFree(item.cast()) };
        ensure!(
            result >= 0,
            "Windows could not reveal the file ({result:#x})."
        );
    } else {
        let result = unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                ptr::null(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        ensure!(
            result > 32,
            "Windows could not open the checkout item (code {result})."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_empty_folders_and_refuses_replacement_or_checkout_escape() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = dunce::canonicalize(temporary.path())?;
        git_query(&git_path()?, &root, &["init", "--quiet"])?;
        assert_eq!(create_item(&root, "", "notes", true)?, "notes");
        assert_eq!(create_item(&root, "notes", "empty", true)?, "notes/empty");
        assert_eq!(create_item(&root, "notes", "é.md", false)?, "notes/é.md");
        fs::write(root.join("notes/é.md"), "saved")?;
        assert!(create_item(&root, "notes", "é.md", false).is_err());
        assert!(create_item(&root, "", "CON.md", false).is_err());
        assert!(create_item(&root, "../notes", "doc.md", false).is_err());
        assert!(create_item(&root, ".git", "doc.md", false).is_err());
        fs::write(root.join(".gitignore"), "ignored/\n")?;
        fs::create_dir(root.join("ignored"))?;
        assert_eq!(list_folders(&root)?, ["notes", "notes/empty"]);
        assert_eq!(fs::read_to_string(root.join("notes/é.md"))?, "saved");
        Ok(())
    }
}
