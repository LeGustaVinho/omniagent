use std::{
    collections::HashSet,
    fs,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use windows::{
    Win32::{
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::Shell::{
            FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE,
            FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName,
        },
    },
    core::PCWSTR,
};

use crate::{
    installation::atomic_file::publish_directory,
    providers::configuration::{open_locked_local_file, plain_metadata},
    store::Store,
};

use super::{
    Apartment, checkout_directory, item_path, validate_name, validated_root, write_in_checkout,
};
use crate::repository::documents::{
    lock_directories, open_directory_for_rename, open_for_rename, rename,
};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutItemOperation {
    Copy,
    Move,
    Recycle,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutItemEdit {
    source: String,
    directory: bool,
    destination: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutItemResult {
    completed: Vec<CheckoutItemEdit>,
    error: Option<String>,
}

#[tauri::command]
pub async fn mutate_checkout_items(
    store: State<'_, Store>,
    checkout_id: String,
    operation: CheckoutItemOperation,
    edits: Vec<CheckoutItemEdit>,
) -> Result<CheckoutItemResult, String> {
    let apply = async {
        let drafts = if operation == CheckoutItemOperation::Copy {
            Vec::new()
        } else {
            let checkout_id = checkout_id.clone();
            store.execute(move |db| {
                let mut query = db.prepare("SELECT document FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1")?;
                Ok(query.query_map([checkout_id], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }).await?
        };
        write_in_checkout(&store, checkout_id, move |root| {
            apply_items(root, operation, edits, &drafts)
        })
        .await
    };
    apply.await.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn import_checkout_items(
    app: AppHandle,
    window: WebviewWindow,
    store: State<'_, Store>,
    checkout_id: String,
    relative_directory: String,
    folder: bool,
) -> Result<CheckoutItemResult, String> {
    let import = async {
        let root = validated_root(&store, checkout_id.clone()).await?;
        let destination = checkout_directory(&root, &relative_directory)?;
        let sources = tauri::async_runtime::spawn_blocking(move || {
            let dialog = app
                .dialog()
                .file()
                .set_parent(&window)
                .set_directory(destination);
            let choices = if folder {
                dialog
                    .set_title("Import folder")
                    .blocking_pick_folder()
                    .into_iter()
                    .collect()
            } else {
                dialog
                    .set_title("Import files")
                    .blocking_pick_files()
                    .unwrap_or_default()
            };
            choices
                .into_iter()
                .map(|choice| choice.into_path())
                .collect::<Result<Vec<_>, _>>()
        })
        .await??;
        if sources.is_empty() {
            return Ok(CheckoutItemResult {
                completed: Vec::new(),
                error: None,
            });
        }
        write_in_checkout(&store, checkout_id, move |root| {
            let plans = plan_imports(root, &relative_directory, sources, folder)?;
            Ok(apply_planned(CheckoutItemOperation::Copy, plans))
        })
        .await
    };
    import.await.map_err(|error| format!("{error:#}"))
}

struct PlannedEdit {
    edit: CheckoutItemEdit,
    source: PathBuf,
    destination: Option<PathBuf>,
}

fn plan_imports(
    root: &Path,
    folder: &str,
    sources: Vec<PathBuf>,
    directory: bool,
) -> Result<Vec<PlannedEdit>> {
    let parent = checkout_directory(root, folder)?;
    let mut destinations = HashSet::new();
    sources
        .into_iter()
        .map(|source| {
            validate_existing(&source, directory)?;
            let canonical = dunce::canonicalize(&source)?;
            ensure!(
                dunce::simplified(&canonical)
                    .as_os_str()
                    .eq_ignore_ascii_case(dunce::simplified(&source).as_os_str()),
                "The selected import path changed. Select it again."
            );
            let source = canonical;
            let name = source
                .file_name()
                .and_then(|name| name.to_str())
                .context("The selected item has no file name.")?;
            validate_name(name)?;
            let relative = if folder.is_empty() {
                name.to_owned()
            } else {
                format!("{folder}/{name}")
            };
            let destination = parent.join(name);
            ensure!(
                destinations.insert(relative.to_lowercase()),
                "More than one item is named {name}."
            );
            ensure!(
                plain_metadata(&destination)?.is_none(),
                "The destination already exists: {relative}"
            );
            if directory {
                let source_key = source.to_string_lossy().to_lowercase();
                ensure!(
                    !parent
                        .ancestors()
                        .any(|ancestor| ancestor.to_string_lossy().to_lowercase() == source_key),
                    "A folder cannot be imported into itself."
                );
            }
            let edit = CheckoutItemEdit {
                source: name.to_owned(),
                directory,
                destination: Some(relative),
            };
            Ok(PlannedEdit {
                edit,
                source,
                destination: Some(destination),
            })
        })
        .collect()
}

fn contains_item(parent: &str, path: &str) -> bool {
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_existing(path: &Path, directory: bool) -> Result<()> {
    let metadata = plain_metadata(path)?.context("The selected checkout item no longer exists.")?;
    ensure!(
        (directory && metadata.is_dir()) || (!directory && metadata.is_file()),
        "The selected checkout item changed type."
    );
    Ok(())
}

fn plan_items(
    root: &Path,
    operation: CheckoutItemOperation,
    edits: Vec<CheckoutItemEdit>,
    drafts: &[String],
) -> Result<Vec<PlannedEdit>> {
    ensure!(!edits.is_empty(), "Select at least one checkout item.");
    let mut sources = HashSet::new();
    let mut destinations = HashSet::new();
    for edit in &edits {
        ensure!(
            !edit.source.is_empty(),
            "Select an item inside the checkout."
        );
        ensure!(
            sources.insert(edit.source.to_lowercase()),
            "An item was selected more than once."
        );
    }
    let drafts = drafts
        .iter()
        .map(|path| path.to_lowercase())
        .collect::<Vec<_>>();
    edits
        .into_iter()
        .map(|edit| {
            let source_key = edit.source.to_lowercase();
            if operation != CheckoutItemOperation::Copy {
                ensure!(
                    !drafts.iter().any(|path| path == &source_key
                        || edit.directory && contains_item(&source_key, path)),
                    "Save or resolve the draft under {} before moving or recycling it.",
                    edit.source
                );
            }
            ensure!(
                !sources
                    .iter()
                    .any(|other| other != &source_key && contains_item(other, &source_key)),
                "Select a folder or its children for this operation."
            );
            let source = item_path(root, &edit.source)?;
            let (parent, _) = edit.source.rsplit_once('/').unwrap_or(("", &edit.source));
            checkout_directory(root, parent)?;
            validate_existing(&source, edit.directory)?;
            let destination = match (&edit.destination, operation) {
                (None, CheckoutItemOperation::Recycle) => None,
                (Some(relative), CheckoutItemOperation::Copy | CheckoutItemOperation::Move) => {
                    ensure!(
                        !relative.is_empty() && relative != &edit.source,
                        "Choose a different destination inside the checkout."
                    );
                    let destination_key = relative.to_lowercase();
                    let case_only =
                        operation == CheckoutItemOperation::Move && source_key == destination_key;
                    ensure!(
                        destinations.insert(destination_key.clone()),
                        "More than one item has the same destination."
                    );
                    ensure!(
                        case_only || !sources.contains(&destination_key),
                        "A destination is also a selected source."
                    );
                    ensure!(
                        !edit.directory
                            || case_only
                            || !contains_item(&source_key, &destination_key),
                        "A folder cannot be copied or moved into itself."
                    );
                    let destination = item_path(root, relative)?;
                    let (parent, _) = relative.rsplit_once('/').unwrap_or(("", relative));
                    checkout_directory(root, parent)?;
                    ensure!(
                        case_only || plain_metadata(&destination)?.is_none(),
                        "The destination already exists: {relative}"
                    );
                    Some(destination)
                }
                _ => anyhow::bail!("The destination does not match the requested file operation."),
            };
            Ok(PlannedEdit {
                edit,
                source,
                destination,
            })
        })
        .collect()
}

fn apply_items(
    root: &Path,
    operation: CheckoutItemOperation,
    edits: Vec<CheckoutItemEdit>,
    drafts: &[String],
) -> Result<CheckoutItemResult> {
    Ok(apply_planned(
        operation,
        plan_items(root, operation, edits, drafts)?,
    ))
}

fn apply_planned(
    operation: CheckoutItemOperation,
    planned: Vec<PlannedEdit>,
) -> CheckoutItemResult {
    let mut result = CheckoutItemResult {
        completed: Vec::new(),
        error: None,
    };
    for plan in planned {
        match apply_item(operation, &plan) {
            Ok(()) => result.completed.push(plan.edit),
            Err(error) => {
                result.error = Some(format!("{}: {error:#}", plan.edit.source));
                break;
            }
        }
    }
    result
}

fn apply_item(operation: CheckoutItemOperation, plan: &PlannedEdit) -> Result<()> {
    let parent = plan
        .source
        .parent()
        .context("The source has no parent folder.")?;
    let _source_parents = lock_directories(parent)?;
    validate_existing(&plan.source, plan.edit.directory)?;
    if plan.edit.directory && operation != CheckoutItemOperation::Copy {
        check_subtree(&plan.source)?;
    }
    match (operation, &plan.destination) {
        (CheckoutItemOperation::Recycle, None) => recycle(&plan.source),
        (CheckoutItemOperation::Move | CheckoutItemOperation::Copy, Some(destination)) => {
            let destination_parent = destination
                .parent()
                .context("The destination has no parent folder.")?;
            let _destination_parents = lock_directories(destination_parent)?;
            if operation == CheckoutItemOperation::Copy {
                let staging = tempfile::Builder::new()
                    .prefix(".omniagent-copy-")
                    .tempdir_in(destination_parent)?;
                let staged = staging.path().join("item");
                copy_entry(&plan.source, &staged, plan.edit.directory)?;
                publish_directory(&staged, destination)?;
            } else {
                let item = if plan.edit.directory {
                    open_directory_for_rename(&plan.source)?
                } else {
                    open_for_rename(&plan.source)?
                };
                rename(&item, destination)?;
            }
            Ok(())
        }
        _ => anyhow::bail!("The file operation has no valid destination."),
    }
}

fn check_subtree(directory: &Path) -> Result<()> {
    let _guard = lock_directories(directory)?;
    for item in fs::read_dir(directory)? {
        let item = item?;
        ensure!(
            !item.file_name().eq_ignore_ascii_case(".git"),
            "The folder contains a nested Git checkout."
        );
        let path = item.path();
        let metadata = plain_metadata(&path)?.context("An item no longer exists.")?;
        if metadata.is_dir() {
            check_subtree(&path)?;
        }
    }
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path, directory: bool) -> Result<()> {
    if directory {
        let _guard = lock_directories(source)?;
        fs::create_dir(destination)?;
        for item in fs::read_dir(source)? {
            let item = item?;
            let name = item.file_name();
            ensure!(
                !name.eq_ignore_ascii_case(".git"),
                "The folder contains a nested Git checkout."
            );
            let metadata = plain_metadata(&item.path())?.context("An item no longer exists.")?;
            copy_entry(&item.path(), &destination.join(name), metadata.is_dir())?;
        }
    } else {
        let mut input = open_locked_local_file(source)?;
        let mut output = fs::File::create_new(destination)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
    }
    Ok(())
}

fn recycle(path: &Path) -> Result<()> {
    let _apartment = Apartment::initialize()?;
    let name = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(
            FOFX_RECYCLEONDELETE
                | FOFX_EARLYFAILURE
                | FOF_NOCONFIRMATION
                | FOF_NOERRORUI
                | FOF_SILENT,
        )?;
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None)?;
        operation.DeleteItem(&item, None)?;
        operation.PerformOperations()?;
        ensure!(
            !operation.GetAnyOperationsAborted()?.as_bool(),
            "Windows aborted the Recycle Bin operation."
        );
    }
    ensure!(
        plain_metadata(path)?.is_none(),
        "Windows did not move the selected item to the Recycle Bin."
    );
    Ok(())
}

#[cfg(test)]
mod tests;
