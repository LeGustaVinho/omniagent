use std::{fs, os::windows::fs::MetadataExt, path::Path};

use anyhow::Result;
use tauri::State;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
};

use super::{checkout_directory, item_path, lock_directories, relative_name, validated_root};
use crate::{providers::configuration::plain_metadata, store::Store};

#[tauri::command]
pub async fn list_checkout_windows_hidden_items(
    store: State<'_, Store>,
    checkout_id: String,
    folder: String,
) -> Result<Vec<String>, String> {
    checkout_hidden_items(&store, checkout_id, folder)
        .await
        .map_err(|error| format!("{error:#}"))
}

async fn checkout_hidden_items(
    store: &Store,
    checkout_id: String,
    folder: String,
) -> Result<Vec<String>> {
    let root = validated_root(store, checkout_id).await?;
    tokio::task::spawn_blocking(move || hidden_items(&root, &folder)).await?
}

fn hidden_items(root: &Path, folder: &str) -> Result<Vec<String>> {
    let path = item_path(root, folder)?;
    // A saved editor tab can leave an inferred folder after it has been removed from disk.
    if !folder.is_empty() && plain_metadata(&path)?.is_none() {
        return Ok(Vec::new());
    }
    let directory = checkout_directory(root, folder)?;
    let _parents = lock_directories(&directory)?;

    let mut hidden = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let Ok(relative) = relative_name(root, &path) else {
            continue;
        };
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let attributes = metadata.file_attributes();
        if attributes & FILE_ATTRIBUTE_HIDDEN != 0
            && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && (metadata.is_file() || metadata.is_dir())
            && (!metadata.is_dir() || path.join(".git").symlink_metadata().is_err())
        {
            hidden.push(relative);
        }
    }
    hidden.sort_unstable();
    Ok(hidden)
}

#[cfg(test)]
mod tests {
    use std::{io, os::windows::ffi::OsStrExt};

    use windows_sys::Win32::Storage::FileSystem::SetFileAttributesW;

    use super::*;
    use crate::repository::{git_path, git_query, register};

    fn mark_hidden(path: &Path) -> io::Result<()> {
        let name: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: name is a live, NUL-terminated Windows path inside the temporary fixture.
        if unsafe { SetFileAttributesW(name.as_ptr(), FILE_ATTRIBUTE_HIDDEN) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[tokio::test]
    async fn reads_windows_hidden_attributes_one_scoped_folder_at_a_time() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("checkout");
        fs::create_dir_all(root.join("reports/hidden archive"))?;
        let root = dunce::canonicalize(root)?;
        git_query(&git_path()?, &root, &["init", "--quiet"])?;
        fs::write(root.join(".visible-dotfile"), b"")?;
        fs::write(root.join("hidden.md"), b"")?;
        fs::write(root.join("reports/hidden archive/ordinary.md"), b"")?;
        fs::write(root.join("reports/hidden archive/private.md"), b"")?;
        mark_hidden(&root.join("hidden.md"))?;
        mark_hidden(&root.join("reports/hidden archive"))?;
        mark_hidden(&root.join("reports/hidden archive/private.md"))?;

        let nested = root.join("nested-checkout");
        fs::create_dir(&nested)?;
        git_query(&git_path()?, &nested, &["init", "--quiet"])?;
        mark_hidden(&nested)?;

        let store = Store::open(temporary.path().join("checkout.sqlite"))?;
        let checkout = register(&store, root.clone()).await?.checkout_id;
        assert_eq!(
            checkout_hidden_items(&store, checkout.clone(), "".into()).await?,
            ["hidden.md"]
        );
        assert_eq!(hidden_items(&root, "reports")?, ["reports/hidden archive"]);
        assert_eq!(
            hidden_items(&root, "reports/hidden archive")?,
            ["reports/hidden archive/private.md"]
        );
        assert!(hidden_items(&root, "removed-directory")?.is_empty());
        assert!(hidden_items(&root, "nested-checkout").is_err());
        assert!(hidden_items(&root, "hidden.md").is_err());
        assert!(hidden_items(&root, "../checkout").is_err());
        assert!(
            checkout_hidden_items(&store, "unknown-checkout".into(), "".into())
                .await
                .is_err()
        );
        Ok(())
    }
}
