use std::path::{Component, Path, Prefix};

use anyhow::{Context, Result, ensure};

use crate::{
    providers::configuration::{open_verified_local_file, strip_local_dos_prefix},
    store::Store,
};

use super::{
    checkout_items::{checkout_directory, checkout_regular_file, validate_name},
    checkout_root,
    documents::relative_name,
    verify_checkout,
};

pub async fn resolve_terminal_file(
    store: &Store,
    checkout_id: String,
    relative_directory: Option<String>,
    path: String,
) -> Result<String> {
    let root = checkout_root(store, checkout_id.clone()).await?;
    let layout = verify_checkout(store, checkout_id, root).await?;
    tokio::task::spawn_blocking(move || {
        resolve_path(
            &layout.root,
            relative_directory.as_deref().unwrap_or(""),
            &path,
        )
    })
    .await?
}

fn resolve_path(root: &Path, directory: &str, printed: &str) -> Result<String> {
    ensure!(
        !printed.is_empty()
            && printed.encode_utf16().count() < 32_767
            && !printed.chars().any(char::is_control)
            && !printed.ends_with(['/', '\\']),
        "Invalid terminal file reference"
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
    let relative = relative_name(root, &target)?;
    let file = checkout_regular_file(root, &relative)?;
    open_verified_local_file(&file)?;
    Ok(relative)
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
        assert_eq!(resolve_path(&root, "docs", ".\\notes.md")?, "docs/notes.md");
        assert_eq!(resolve_path(&root, "docs", "../notes.md")?, "notes.md");
        let absolute = root.join("docs/revisão #1.md");
        assert_eq!(
            resolve_path(&root, "", absolute.to_str().unwrap())?,
            "docs/revisão #1.md"
        );
        let junction = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(root.join("escape"))
            .arg(&outside)
            .output()?;
        ensure!(junction.status.success(), "fixture junction failed");
        for printed in [
            "../checkout-other/outside.ts",
            "escape/outside.ts",
            "escape/../notes.md",
            "docs/notes.md:Zone.Identifier",
            "docs",
            "docs/notes.md/",
            "C:notes.md",
            "\\notes.md",
            ".git/config",
            "NUL.md",
            outside.join("outside.ts").to_str().unwrap(),
        ] {
            assert!(resolve_path(&root, "", printed).is_err(), "{printed}");
        }
        assert!(resolve_path(&root, "escape", "outside.ts").is_err());
        Ok(())
    }
}
