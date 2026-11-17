use std::{
    fs::File,
    os::windows::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use windows_sys::Win32::{
    Storage::FileSystem::GetBinaryTypeW,
    System::WindowsProgramming::{SCS_32BIT_BINARY, SCS_64BIT_BINARY},
};

use crate::{
    diagnostics,
    providers::configuration::{expand_local_dos_path, open_locked_local_file, plain_metadata},
    store::Store,
};

use super::{checkout_regular_file, lock_directories, validated_root};

const SETTING: &str = "external_editor_executable";

fn report(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

async fn configured_editor(store: &Store) -> Result<Option<PathBuf>> {
    store
        .execute(|db| {
            let path = db
                .query_row(
                    "SELECT value FROM workspace_settings WHERE key = ?1",
                    [SETTING],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            Ok(path.map(PathBuf::from))
        })
        .await
}

async fn configure_editor(store: &Store, selected: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let selected = match selected {
        Some(path) => Some(
            tokio::task::spawn_blocking(move || {
                let path = verified_editor_path(&path)?;
                verify_editor_binary(&path)?;
                Ok::<_, anyhow::Error>(path)
            })
            .await??,
        ),
        None => None,
    };
    let saved = selected.clone();
    store
        .execute(move |db| {
            if let Some(path) = saved {
                let executable = path
                    .to_str()
                    .context("The external editor path is not UTF-8.")?;
                db.execute(
                    "INSERT INTO workspace_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![SETTING, executable],
                )?;
            } else {
                db.execute("DELETE FROM workspace_settings WHERE key = ?1", [SETTING])?;
            }
            Ok(())
        })
        .await?;
    Ok(selected)
}

fn verified_editor_path(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute()
            && path
                .extension()
                .is_some_and(|value| value.eq_ignore_ascii_case("exe"))
            && path.components().all(|part| match part {
                Component::ParentDir => false,
                Component::Normal(name) => name
                    .to_str()
                    .is_some_and(|name| !name.contains([':', '\0'])),
                _ => true,
            }),
        "Select an absolute path to a local Windows editor executable (.exe)."
    );
    let path = expand_local_dos_path(path)
        .context("The selected editor executable could not be opened.")?;
    ensure!(
        plain_metadata(&path)?.is_some_and(|metadata| metadata.is_file()),
        "The external editor must be a local executable file."
    );
    Ok(path)
}

fn verify_editor_binary(path: &Path) -> Result<File> {
    let file = open_locked_local_file(path)
        .context("The selected editor executable could not be opened.")?;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut binary_type = 0;
    let recognized = unsafe { GetBinaryTypeW(name.as_ptr(), &mut binary_type) };
    ensure!(
        recognized != 0 && matches!(binary_type, SCS_32BIT_BINARY | SCS_64BIT_BINARY),
        "The selected file is not a Windows executable."
    );
    Ok(file)
}

fn editor_command(executable: &Path, directory: &Path, file: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(directory)
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn open_editor(root: &Path, relative_path: &str, executable: &Path) -> Result<()> {
    let file = checkout_regular_file(root, relative_path)?;
    let file_parent = file
        .parent()
        .context("The checkout file has no parent folder.")?;
    let _checkout_directories = lock_directories(file_parent)?;
    let file = checkout_regular_file(root, relative_path)?;
    let _checked_file = open_locked_local_file(&file)?;
    let file = dunce::canonicalize(file)?;
    let executable = verified_editor_path(executable)?;
    let directory = executable
        .parent()
        .context("The editor executable has no parent folder.")?;
    let _editor_directories = lock_directories(directory)?;
    let _editor_binary = verify_editor_binary(&executable)?;
    editor_command(&executable, directory, &file)
        .spawn()
        .context("Could not start the configured external editor.")?;
    Ok(())
}

#[tauri::command]
pub async fn external_editor_setting(store: State<'_, Store>) -> Result<Option<PathBuf>, String> {
    configured_editor(&store).await.map_err(report)
}

#[tauri::command]
pub async fn choose_external_editor(
    app: AppHandle,
    window: WebviewWindow,
    store: State<'_, Store>,
) -> Result<Option<PathBuf>, String> {
    let selection = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_parent(&window)
            .set_title("Select a Windows editor executable")
            .add_filter("Windows executable", &["exe"])
            .blocking_pick_file()
            .map(|choice| choice.into_path())
            .transpose()
    })
    .await
    .map_err(report)?
    .map_err(report)?;
    let Some(selected) = selection else {
        return Ok(None);
    };
    configure_editor(&store, Some(selected))
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn clear_external_editor(store: State<'_, Store>) -> Result<(), String> {
    configure_editor(&store, None)
        .await
        .map(|_| ())
        .map_err(report)
}

#[tauri::command]
pub async fn open_checkout_file_in_external_editor(
    store: State<'_, Store>,
    checkout_id: String,
    relative_path: String,
) -> Result<(), String> {
    let open = async {
        let executable = configured_editor(&store)
            .await?
            .context("Choose an external editor under Settings → System → External editor.")?;
        let root = validated_root(&store, checkout_id).await?;
        tokio::task::spawn_blocking(move || open_editor(&root, &relative_path, &executable)).await?
    };
    open.await.map_err(report)
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    fn system_executable() -> Result<PathBuf> {
        Ok(PathBuf::from(
            env::var_os("SystemRoot").context("Windows system directory is unavailable")?,
        )
        .join("System32")
        .join("where.exe"))
    }

    #[tokio::test]
    async fn persists_and_clears_only_a_valid_local_windows_executable() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let database = folder.path().join("editor.sqlite");
        let store = Store::open(database.clone())?;
        assert_eq!(configured_editor(&store).await?, None);
        let executable = verified_editor_path(&system_executable()?)?;
        assert_eq!(
            configure_editor(&store, Some(executable.clone())).await?,
            Some(executable.clone())
        );

        let fake = folder.path().join("not an editor.exe");
        fs::write(&fake, b"not a Windows executable")?;
        for invalid in [
            PathBuf::from("editor.exe"),
            fake,
            PathBuf::from(r"\\server\share\editor.exe"),
        ] {
            assert!(configure_editor(&store, Some(invalid)).await.is_err());
        }
        drop(store);

        let reopened = Store::open(database)?;
        assert_eq!(configured_editor(&reopened).await?, Some(executable));
        configure_editor(&reopened, None).await?;
        assert_eq!(configured_editor(&reopened).await?, None);
        Ok(())
    }

    #[test]
    fn uses_the_exact_checked_file_as_one_argument_and_rejects_out_of_scope_targets() -> Result<()>
    {
        let folder = tempfile::tempdir()?;
        let checkout = dunce::canonicalize(folder.path())?.join("checkout");
        fs::create_dir_all(checkout.join("notes"))?;
        let path = "notes/A & (draft) %review% ü.txt";
        let expected = checkout.join("notes").join("A & (draft) %review% ü.txt");
        fs::write(&expected, b"fixture")?;
        let executable = verified_editor_path(&system_executable()?)?;
        let file = dunce::canonicalize(checkout_regular_file(&checkout, path)?)?;
        let directory = executable.parent().unwrap();
        let command = editor_command(&executable, directory, &file);
        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(command.get_current_dir(), Some(directory));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [expected.as_os_str()]
        );
        open_editor(&checkout, path, &executable)?;
        fs::create_dir_all(checkout.join("nested/.git"))?;
        fs::write(checkout.join("nested/file.txt"), b"nested checkout")?;
        for invalid in [
            "../outside.txt",
            "notes",
            "notes/missing.txt",
            ".git/config",
            "nested/file.txt",
        ] {
            assert!(
                open_editor(&checkout, invalid, &executable).is_err(),
                "{invalid}"
            );
        }
        Ok(())
    }
}
