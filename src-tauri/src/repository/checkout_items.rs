use std::{
    collections::HashSet,
    fs,
    io::Write,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
};

use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tauri::{State, WebviewWindow};
use windows_sys::Win32::{
    System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
    UI::{
        Shell::{
            OAIF_EXEC, OPENASINFO, SHOpenFolderAndSelectItems, SHOpenWithDialog,
            SHParseDisplayName, ShellExecuteW,
        },
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

use crate::{providers::configuration::plain_metadata, store::Store};

use super::{
    checkout_root,
    documents::{lock_directories, relative_name},
    git_command, git_path, git_query, verify_checkout, with_managed_worktree_writer,
};

pub mod mutations;

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

fn ignored_roots(root: &Path) -> Result<String> {
    git_query(
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
    )
}

fn list_folders(root: &Path) -> Result<Vec<String>> {
    let ignored = ignored_roots(root)?;
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

#[derive(Default, Serialize)]
pub struct CheckoutIgnoredItems {
    files: Vec<String>,
    folders: Vec<String>,
}

#[tauri::command]
pub async fn list_checkout_ignored_items(
    store: State<'_, Store>,
    checkout_id: String,
    folder: Option<String>,
) -> Result<CheckoutIgnoredItems, String> {
    let list = async {
        let root = validated_root(&store, checkout_id).await?;
        tokio::task::spawn_blocking(move || list_ignored(&root, folder.as_deref())).await?
    };
    list.await.map_err(|error| format!("{error:#}"))
}

fn list_ignored(root: &Path, folder: Option<&str>) -> Result<CheckoutIgnoredItems> {
    let mut items = CheckoutIgnoredItems::default();
    let mut push = |path: &Path| {
        let Ok(path_name) = relative_name(root, path) else {
            return;
        };
        let Ok(Some(metadata)) = plain_metadata(path) else {
            return;
        };
        if metadata.is_file() {
            items.files.push(path_name);
        } else if metadata.is_dir() && path.join(".git").symlink_metadata().is_err() {
            items.folders.push(path_name);
        }
    };
    if let Some(folder) = folder {
        ensure!(!folder.is_empty(), "Choose an ignored subfolder.");
        let directory = checkout_directory(root, folder)?;
        let _parents = lock_directories(&directory)?;
        let checked = git_command(&git_path()?, root)
            .args(["check-ignore", "--quiet", "--", &format!("{folder}/")])
            .output()?;
        ensure!(
            checked.status.success(),
            "The folder is no longer ignored by Git."
        );
        for entry in fs::read_dir(directory)? {
            push(&entry?.path());
        }
    } else {
        for path in ignored_roots(root)?.split_terminator('\0') {
            let relative = path.trim_end_matches('/');
            if let Ok(path) = item_path(root, relative) {
                push(&path);
            }
        }
    }
    items.files.sort_unstable();
    items.files.dedup();
    items.folders.sort_unstable();
    items.folders.dedup();
    Ok(items)
}

#[tauri::command]
pub async fn create_checkout_item(
    store: State<'_, Store>,
    checkout_id: String,
    relative_directory: String,
    name: String,
    directory: bool,
    contents: Option<String>,
) -> Result<String, String> {
    write_in_checkout(&store, checkout_id, move |root| {
        create_item(
            root,
            &relative_directory,
            &name,
            directory,
            contents.as_deref(),
        )
    })
    .await
    .map_err(|error| format!("{error:#}"))
}

async fn write_in_checkout<T: Send + 'static>(
    store: &Store,
    checkout_id: String,
    write: impl FnOnce(&Path) -> Result<T> + Send + 'static,
) -> Result<T> {
    let root = validated_root(store, checkout_id.clone()).await?;
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
    if let Some((project, task)) = managed {
        with_managed_worktree_writer(store, project, checkout_id, task, move |worktree| {
            ensure!(
                worktree.root == root,
                "The managed worktree moved. Reopen it before changing files."
            );
            write(&root)
        })
        .await
    } else {
        tokio::task::spawn_blocking(move || write(&root)).await?
    }
}

fn create_item(
    root: &Path,
    relative_directory: &str,
    name: &str,
    directory: bool,
    contents: Option<&str>,
) -> Result<String> {
    validate_name(name)?;
    let parent = checkout_directory(root, relative_directory)?;
    let _parents = super::documents::lock_directories(&parent)?;
    let path = parent.join(name);
    let relative = relative_name(root, &path)?;
    if directory {
        ensure!(contents.is_none(), "A folder cannot contain text.");
        fs::create_dir(path)?;
    } else if let Some(contents) = contents {
        let mut staged = tempfile::NamedTempFile::new_in(&parent)?;
        staged.as_file_mut().write_all(contents.as_bytes())?;
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(path)
            .context("Save the new file without replacing an existing item")?;
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
    OpenWith,
}

pub(crate) fn reveal_local_file(path: &Path) -> Result<()> {
    ensure!(
        plain_metadata(path)?.is_some_and(|metadata| metadata.is_file()),
        "The executable no longer exists."
    );
    shell_item(path, false, ShellAction::Reveal)
}

#[tauri::command]
pub async fn open_checkout_item_in_shell(
    store: State<'_, Store>,
    window: WebviewWindow,
    checkout_id: String,
    relative_path: String,
    action: ShellAction,
) -> Result<(), String> {
    let open = async {
        let root = validated_root(&store, checkout_id).await?;
        let hwnd = window.hwnd().map_or(0, |handle| handle.0 as isize);
        tokio::task::spawn_blocking(move || {
            let path = item_path(&root, &relative_path)?;
            let metadata = plain_metadata(&path)?.context("The checkout item no longer exists.")?;
            if matches!(action, ShellAction::OpenWith) {
                ensure!(
                    metadata.is_file(),
                    "Select a file to open with another application."
                );
                return shell_open_with(&path, hwnd);
            }
            shell_item(&path, metadata.is_dir(), action)
        })
        .await?
    };
    open.await.map_err(|error| format!("{error:#}"))
}

fn shell_open_with(path: &Path, parent: isize) -> Result<()> {
    let _apartment = Apartment::initialize()?;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let options = OPENASINFO {
        pcszFile: path.as_ptr(),
        pcszClass: ptr::null(),
        oaifInFlags: OAIF_EXEC,
    };
    let result = unsafe { SHOpenWithDialog(parent as *mut std::ffi::c_void, &options) };
    ensure!(
        result >= 0 || matches!(result as u32, 0x8007_04C7 | 0x8000_4004),
        "Windows could not open the application picker ({result:#x})."
    );
    Ok(())
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
    fn lists_ignored_roots_and_reads_only_requested_direct_children() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = dunce::canonicalize(temporary.path())?;
        git_query(&git_path()?, &root, &["init", "--quiet"])?;
        fs::write(root.join(".gitignore"), "cache/\n*.private.log\n")?;
        fs::create_dir_all(root.join("cache/logs/archived"))?;
        fs::create_dir_all(root.join("cache/nested-repository"))?;
        fs::create_dir(root.join("notes"))?;
        fs::write(root.join("cache/direct.txt"), "direct child")?;
        fs::write(root.join("cache/logs/archived/old.log"), "deep child")?;
        fs::write(root.join("notes/debug.private.log"), "ignored by pattern")?;
        fs::write(root.join("notes/guide.md"), "ordinary file")?;
        git_query(
            &git_path()?,
            &root.join("cache/nested-repository"),
            &["init", "--quiet"],
        )?;

        let ignored = list_ignored(&root, None)?;
        assert_eq!(ignored.folders, ["cache"]);
        assert_eq!(ignored.files, ["notes/debug.private.log"]);
        assert_eq!(list_folders(&root)?, ["notes"]);
        let cache = list_ignored(&root, Some("cache"))?;
        assert_eq!(cache.folders, ["cache/logs"]);
        assert_eq!(cache.files, ["cache/direct.txt"]);
        let logs = list_ignored(&root, Some("cache/logs"))?;
        assert_eq!(logs.folders, ["cache/logs/archived"]);
        assert!(logs.files.is_empty());
        assert!(list_ignored(&root, Some("notes")).is_err());
        assert!(list_ignored(&root, Some("cache/nested-repository")).is_err());
        assert!(list_ignored(&root, Some("../cache")).is_err());
        Ok(())
    }

    #[test]
    fn creates_empty_folders_and_refuses_replacement_or_checkout_escape() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = dunce::canonicalize(temporary.path())?;
        git_query(&git_path()?, &root, &["init", "--quiet"])?;
        assert_eq!(create_item(&root, "", "notes", true, None)?, "notes");
        assert_eq!(
            create_item(&root, "notes", "empty", true, None)?,
            "notes/empty"
        );
        assert_eq!(
            create_item(&root, "notes", "é.md", false, None)?,
            "notes/é.md"
        );
        fs::write(root.join("notes/é.md"), "saved")?;
        assert!(create_item(&root, "notes", "é.md", false, Some("Must not overwrite")).is_err());
        assert_eq!(
            create_item(
                &root,
                "notes",
                "draft copy.md",
                false,
                Some("Edited text\n")
            )?,
            "notes/draft copy.md"
        );
        assert_eq!(
            fs::read_to_string(root.join("notes/draft copy.md"))?,
            "Edited text\n"
        );
        assert!(create_item(&root, "", "CON.md", false, None).is_err());
        assert!(create_item(&root, "../notes", "doc.md", false, None).is_err());
        assert!(create_item(&root, ".git", "doc.md", false, None).is_err());
        fs::write(root.join(".gitignore"), "ignored/\n")?;
        fs::create_dir(root.join("ignored"))?;
        assert_eq!(list_folders(&root)?, ["notes", "notes/empty"]);
        assert_eq!(fs::read_to_string(root.join("notes/é.md"))?, "saved");
        Ok(())
    }
}
