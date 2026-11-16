use std::{
    io::{BufRead, BufReader, Read},
    path::{Component, Path},
};

use anyhow::Result;
use serde::Serialize;

use crate::{
    providers::configuration::{open_verified_local_file, plain_metadata},
    store::Store,
};

use super::{checkout_root, git_path, git_query, verify_checkout};

const PROPERTY_LIMIT: u64 = 64 * 1024;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryNoteCatalogEntry {
    pub path: String,
    pub frontmatter: Option<String>,
}

impl Store {
    pub async fn repository_note_catalog(
        &self,
        checkout: String,
    ) -> Result<Vec<RepositoryNoteCatalogEntry>> {
        let root = checkout_root(self, checkout.clone()).await?;
        let layout = verify_checkout(self, checkout, root).await?;
        tokio::task::spawn_blocking(move || list_notes(&layout.root)).await?
    }
}

pub(super) fn list_notes(root: &Path) -> Result<Vec<RepositoryNoteCatalogEntry>> {
    Ok(note_paths(root)?
        .into_iter()
        .map(|path| RepositoryNoteCatalogEntry {
            frontmatter: read_frontmatter(&root.join(&path)).ok().flatten(),
            path,
        })
        .collect())
}

pub(super) fn note_paths(root: &Path) -> Result<Vec<String>> {
    let names = git_query(
        &git_path()?,
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
            "--",
            ":(icase)*.md",
            ":(icase)*.markdown",
        ],
    )?;
    let mut entries = Vec::new();
    for name in names.split_terminator('\0') {
        let relative = Path::new(name);
        if name.is_empty()
            || name.contains(':')
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            continue;
        }
        let path = root.join(relative);
        if !plain_metadata(&path)
            .ok()
            .flatten()
            .is_some_and(|entry| entry.is_file())
        {
            continue;
        }
        entries.push(name.to_owned());
    }
    entries.sort_unstable();
    entries.dedup();
    Ok(entries)
}

fn read_frontmatter(path: &Path) -> std::io::Result<Option<String>> {
    let mut reader = BufReader::new(open_verified_local_file(path)?.take(PROPERTY_LIMIT));
    let mut frontmatter = String::new();
    reader.read_line(&mut frontmatter)?;
    if frontmatter
        .trim_end_matches(['\n', '\r', ' ', '\t'])
        .trim_start_matches('\u{feff}')
        != "---"
    {
        return Ok(None);
    }
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        frontmatter.push_str(&line);
        if line.trim_end_matches(['\n', '\r', ' ', '\t']) == "---" {
            // A terminal delimiter cut off by the input limit does not establish a complete header.
            return Ok(
                (reader.get_ref().limit() > 0 || line.ends_with('\n')).then_some(frontmatter)
            );
        }
        line.clear();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn catalog_reads_only_repository_notes_and_bounded_frontmatter() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        let git = git_path()?;
        git_query(&git, &root, &["init", "--quiet"])?;
        fs::create_dir(root.join("docs"))?;
        fs::write(root.join(".gitignore"), "ignored.md\n")?;
        fs::write(root.join("ignored.md"), "Ignored")?;
        fs::write(root.join("removed.md"), "Removed")?;
        git_query(&git, &root, &["add", "removed.md"])?;
        fs::remove_file(root.join("removed.md"))?;
        fs::write(
            root.join("docs/Guide.Markdown"),
            "\u{feff}---\r\naliases: [Guia]\r\n---\r\nprivate body",
        )?;
        fs::write(root.join("README.md"), "# Read me\n")?;
        fs::write(root.join("notes.txt"), "Other file")?;
        let store = Store::open(root.join("notes.sqlite"))?;
        let checkout = super::super::register(&store, root).await?;
        let entries = store.repository_note_catalog(checkout.checkout_id).await?;
        assert_eq!(
            entries,
            [
                RepositoryNoteCatalogEntry {
                    path: "README.md".into(),
                    frontmatter: None
                },
                RepositoryNoteCatalogEntry {
                    path: "docs/Guide.Markdown".into(),
                    frontmatter: Some("\u{feff}---\r\naliases: [Guia]\r\n---\r\n".into()),
                },
            ]
        );
        assert!(
            store
                .repository_note_catalog("another-checkout".into())
                .await
                .is_err()
        );
        Ok(())
    }
}
