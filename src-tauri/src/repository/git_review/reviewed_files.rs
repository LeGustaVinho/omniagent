use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

use crate::{
    providers::configuration::plain_metadata,
    repository::{
        checkout_items::checkout_directory,
        documents::{file_identity, open_for_rename, rename},
    },
};

use super::lock_directories;

pub(super) struct ReviewedFile {
    pub(super) path: String,
    pub(super) before: Option<Vec<u8>>,
    pub(super) after: Option<Vec<u8>>,
    pub(super) held: Option<File>,
}

#[derive(Default)]
pub(super) struct ReviewedFiles {
    pub(super) entries: Vec<ReviewedFile>,
    pub(super) missing_folders: BTreeSet<String>,
    pub(super) guards: Vec<File>,
}

pub(super) fn open_target(path: &Path) -> Result<Option<File>> {
    match open_for_rename(path) {
        Ok(file) => Ok(Some(file)),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

impl ReviewedFiles {
    pub(super) fn lock_parent(&mut self, root: &Path, path: &str) -> Result<(String, String)> {
        let (folder, _) = path.rsplit_once('/').unwrap_or(("", path));
        let mut nearest = folder;
        while plain_metadata(&root.join(nearest))?.is_none() {
            ensure!(
                !nearest.is_empty(),
                "The destination worktree was removed during review."
            );
            self.missing_folders.insert(nearest.into());
            nearest = nearest.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        let checked = checkout_directory(root, nearest)?;
        let guards = lock_directories(&checked)?;
        let identity = file_identity(
            guards
                .last()
                .context("missing destination directory handle")?,
        )?;
        self.guards.extend(guards);
        Ok((nearest.into(), identity))
    }

    pub(super) fn verify_no_drafts(&self, db: &Connection, checkout: &str) -> Result<()> {
        let paths = self
            .entries
            .iter()
            .map(|entry| entry.path.to_uppercase())
            .collect::<BTreeSet<_>>();
        let mut query = db.prepare(
            "SELECT document FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1",
        )?;
        for draft in query.query_map([checkout], |row| row.get::<_, String>(0))? {
            ensure!(
                !paths.contains(&draft?.to_uppercase()),
                "Save or discard protected editor drafts for the affected files before applying the reviewed changes."
            );
        }
        Ok(())
    }
}

impl ReviewedFiles {
    pub(super) fn publish(self, root: &Path) -> Result<()> {
        let vault = tempfile::Builder::new()
            .prefix(".omniagent-review-")
            .tempdir_in(root)?;
        let mut staged = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let original = vault.path().join(format!("{index}.before"));
            let edited = vault.path().join(format!("{index}.after"));
            let replacement = if let Some(bytes) = &entry.after {
                let mut file = File::create(&edited)?;
                file.write_all(bytes)?;
                file.sync_all()?;
                drop(file);
                Some(open_for_rename(&edited)?)
            } else {
                None
            };
            staged.push((original, edited, replacement));
        }
        let mut manifest = File::create(vault.path().join("working-files.json"))?;
        serde_json::to_writer(
            &mut manifest,
            &self
                .entries
                .iter()
                .map(|entry| &entry.path)
                .collect::<Vec<_>>(),
        )?;
        manifest.sync_all()?;
        drop(manifest);
        let mut created = Vec::new();
        let mut created_handles = Vec::new();
        let mut moved = BTreeSet::new();
        let mut installed = BTreeSet::new();
        let result = (|| -> Result<()> {
            for folder in &self.missing_folders {
                let directory = root.join(folder);
                fs::create_dir(&directory)?;
                let handles = lock_directories(&directory)?;
                let identity = file_identity(handles.last().context("missing directory handle")?)?;
                created.push((directory, identity));
                created_handles.extend(handles);
            }
            for (index, entry) in self.entries.iter().enumerate() {
                let destination = root.join(&entry.path);
                if let Some(original) = &entry.held {
                    rename(original, &staged[index].0)?;
                    moved.insert(index);
                }
                if let Some(replacement) = &staged[index].2 {
                    rename(replacement, &destination)?;
                    installed.insert(index);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let mut recovery = false;
            for (index, entry) in self.entries.iter().enumerate().rev() {
                if installed.contains(&index)
                    && let Some(replacement) = &staged[index].2
                {
                    recovery |= rename(replacement, &staged[index].1).is_err();
                }
                if moved.contains(&index)
                    && let Some(original) = &entry.held
                {
                    recovery |= rename(original, &root.join(&entry.path)).is_err();
                }
            }
            drop(created_handles);
            for (directory, identity) in created.iter().rev() {
                let observed = lock_directories(directory).and_then(|locks| {
                    Ok(file_identity(
                        locks.last().context("missing directory handle")?,
                    )?)
                });
                if observed.as_ref().is_ok_and(|observed| observed == identity) {
                    let _ = fs::remove_dir(directory);
                }
            }
            if recovery {
                anyhow::bail!(
                    "Could not finish or roll back the reviewed changes. {error:#}. Recovery files are in {}.",
                    vault.keep().display()
                );
            }
            return Err(error).context(
                "Could not apply the reviewed changes. The affected files were rolled back",
            );
        }
        drop(staged);
        drop(self);
        vault.close().context(
            "The reviewed changes were applied. The temporary recovery copies could not be removed.",
        )
    }
}
