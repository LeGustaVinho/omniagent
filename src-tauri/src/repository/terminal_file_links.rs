use std::path::{Component, Path, Prefix};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    providers::configuration::{open_verified_local_file, plain_metadata, strip_local_dos_prefix},
    store::Store,
};

use super::{
    checkout_items::{checkout_directory, checkout_regular_file, validate_name},
    checkout_root,
    documents::{lock_directories, relative_name},
    verify_checkout,
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalPathKind {
    File,
    Folder,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct TerminalPath {
    path: String,
    kind: TerminalPathKind,
}

pub async fn resolve_terminal_path(
    store: &Store,
    checkout_id: String,
    relative_directory: Option<String>,
    observed_directory: Option<String>,
    path: String,
    expected_kind: Option<TerminalPathKind>,
) -> Result<TerminalPath> {
    let root = checkout_root(store, checkout_id.clone()).await?;
    let layout = verify_checkout(store, checkout_id, root).await?;
    tokio::task::spawn_blocking(move || {
        let directory = match observed_directory {
            Some(observed) if !Path::new(&path).is_absolute() => {
                observed_directory_in_checkout(&layout.root, &observed)?
            }
            _ => relative_directory.unwrap_or_default(),
        };
        resolve_path(&layout.root, &directory, &path, expected_kind)
    })
    .await?
}

fn observed_directory_in_checkout(root: &Path, observed: &str) -> Result<String> {
    ensure!(
        !observed.is_empty()
            && observed.encode_utf16().count() < 32_767
            && !observed.chars().any(char::is_control),
        "The terminal is outside the checkout filesystem"
    );
    let directory = Path::new(observed);
    let relative =
        strip_local_dos_prefix(directory, root).context("The terminal is outside this checkout")?;
    if relative.as_os_str().is_empty() {
        return Ok(String::new());
    }
    let relative = relative_name(root, &root.join(relative))?;
    checkout_directory(root, &relative)?;
    Ok(relative)
}

fn resolve_path(
    root: &Path,
    directory: &str,
    printed: &str,
    expected_kind: Option<TerminalPathKind>,
) -> Result<TerminalPath> {
    ensure!(
        !printed.is_empty()
            && printed.encode_utf16().count() < 32_767
            && !printed.chars().any(char::is_control),
        "Invalid terminal path reference"
    );
    let path = Path::new(printed);
    let (base, suffix) = if path.is_absolute() {
        ensure!(
            matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))),
            "Expected a local drive path"
        );
        let relative = strip_local_dos_prefix(path, root)
            .context("The path is outside the current checkout")?;
        (root.to_owned(), relative)
    } else {
        (checkout_directory(root, directory)?, path)
    };
    let mut target = base;
    for component in suffix.components() {
        match component {
            Component::Normal(name) => {
                validate_name(name.to_str().context("The file path is not UTF-8")?)?;
                target.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let previous = relative_name(root, &target)?;
                checkout_directory(root, &previous)?;
                ensure!(target.pop(), "The path is outside the current checkout");
            }
            _ => anyhow::bail!("The path is outside the current checkout"),
        }
    }
    let named = |path: &Path| {
        if path == root {
            Ok(String::new())
        } else {
            relative_name(root, path)
        }
    };
    let relative = named(&target)?;
    let metadata = plain_metadata(&target)?.context("The checkout path no longer exists")?;
    let kind = if metadata.is_dir() {
        TerminalPathKind::Folder
    } else {
        TerminalPathKind::File
    };
    ensure!(
        expected_kind.is_none_or(|expected| expected == kind),
        "The checkout path has a different type"
    );
    let path = match kind {
        TerminalPathKind::File => {
            ensure!(
                !printed.ends_with(['/', '\\']),
                "Expected a checkout folder"
            );
            let file = checkout_regular_file(root, &relative)?;
            open_verified_local_file(&file)?;
            relative
        }
        TerminalPathKind::Folder => {
            let folder = checkout_directory(root, &relative)?;
            let _directories = lock_directories(&folder)?;
            checkout_directory(root, &relative)?;
            named(&dunce::canonicalize(folder)?)?
        }
    };
    Ok(TerminalPath { path, kind })
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[test]
    fn resolves_exact_scoped_files_and_rejects_escape_or_ambiguous_paths() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = dunce::canonicalize(fixture.path())?.join("checkout");
        let outside = root.with_file_name("checkout-other");
        fs::create_dir_all(root.join("docs"))?;
        fs::create_dir_all(&outside)?;
        fs::write(root.join("notes.md"), "root")?;
        fs::write(root.join("docs/notes.md"), "scoped")?;
        fs::write(root.join("docs/revisão #1.md"), "notes")?;
        fs::write(outside.join("outside.ts"), "outside")?;
        let file = resolve_path(&root, "docs", ".\\notes.md", Some(TerminalPathKind::File))?;
        assert_eq!(
            file,
            TerminalPath {
                path: "docs/notes.md".into(),
                kind: TerminalPathKind::File
            }
        );
        assert_eq!(
            resolve_path(&root, "docs", "../notes.md", None)?.path,
            "notes.md"
        );
        let absolute = root.join("docs/revisão #1.md");
        assert_eq!(
            resolve_path(&root, "", absolute.to_str().unwrap(), None)?.path,
            "docs/revisão #1.md"
        );
        fs::create_dir(root.join("docs/revisão #1"))?;
        for printed in [
            "./revisão #1/".to_owned(),
            root.join("docs/revisão #1").to_string_lossy().into_owned(),
        ] {
            assert_eq!(
                resolve_path(&root, "docs", &printed, None)?,
                TerminalPath {
                    path: "docs/revisão #1".into(),
                    kind: TerminalPathKind::Folder,
                }
            );
        }
        assert_eq!(
            resolve_path(&root, "docs", "../", Some(TerminalPathKind::Folder))?,
            TerminalPath {
                path: String::new(),
                kind: TerminalPathKind::Folder,
            }
        );
        assert!(resolve_path(&root, "", "docs", Some(TerminalPathKind::File)).is_err());
        assert!(resolve_path(&root, "", "notes.md", Some(TerminalPathKind::Folder)).is_err());
        assert_eq!(resolve_path(&root, "", ".\\DOCS", None)?.path, "docs");
        fs::create_dir_all(root.join("docs/nested-repository/.git"))?;
        let junction = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(root.join("escape"))
            .arg(&outside)
            .output()?;
        ensure!(junction.status.success(), "fixture junction failed");
        for printed in [
            "../checkout-other/outside.ts",
            "escape/outside.ts",
            "escape/",
            "escape/../notes.md",
            "docs/notes.md:Zone.Identifier",
            "notes.md/",
            "docs/nested-repository/",
            "docs/../../checkout-other/",
            "C:notes.md",
            "\\notes.md",
            ".git/config",
            "NUL.md",
            outside.join("outside.ts").to_str().unwrap(),
        ] {
            assert!(resolve_path(&root, "", printed, None).is_err(), "{printed}");
        }
        assert!(resolve_path(&root, "escape", "outside.ts", None).is_err());
        assert_eq!(
            observed_directory_in_checkout(&root, root.to_str().unwrap())?,
            ""
        );
        assert_eq!(
            observed_directory_in_checkout(&root, root.join("docs").to_str().unwrap())?,
            "docs"
        );
        for observed in [
            "",
            "docs",
            outside.to_str().unwrap(),
            root.join("escape").to_str().unwrap(),
        ] {
            assert!(
                observed_directory_in_checkout(&root, observed).is_err(),
                "{observed}"
            );
        }
        Ok(())
    }
}
