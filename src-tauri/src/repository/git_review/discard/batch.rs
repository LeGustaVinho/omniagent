use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result, ensure};
use rusqlite::TransactionBehavior;
use serde::Serialize;
use tokio::runtime::Handle;

use crate::{repository::documents::file_identity, store::Store};

use super::super::reviewed_files::{ReviewedFile, ReviewedFiles, open_target};
use super::{
    LiveReview, PreviewFile, ReviewScope, digest, lock_directories, lock_git_file, read_bytes,
    read_checkout_bytes, verify_discard_path, with_managed_worktree_writer,
};

const MAX_BATCH_PATHS: usize = 256;
const MAX_BATCH_BYTES: usize = 24 * 1024 * 1024;

#[derive(Serialize)]
pub struct GitDiscardBatchPreview {
    snapshot: String,
    files: Vec<PreviewEntry>,
}

#[derive(Serialize)]
struct PreviewEntry {
    path: String,
    before: PreviewFile,
    after: PreviewFile,
}

impl Store {
    pub async fn git_discard_batch_preview(
        &self,
        checkout: String,
        paths: Vec<String>,
    ) -> Result<GitDiscardBatchPreview> {
        let paths = selected_paths(paths)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let (files, snapshot) = prepare(&scope, &review, paths)?;
            review.verify()?;
            Ok(GitDiscardBatchPreview {
                snapshot,
                files: files
                    .entries
                    .into_iter()
                    .map(|entry| PreviewEntry {
                        path: entry.path,
                        before: PreviewFile::from_bytes(entry.before.as_deref()),
                        after: PreviewFile::from_bytes(entry.after.as_deref()),
                    })
                    .collect(),
            })
        })
        .await?
    }

    pub async fn git_discard_batch_changes(
        &self,
        checkout: String,
        paths: Vec<String>,
        snapshot: String,
    ) -> Result<()> {
        let paths = selected_paths(paths)?;
        ensure!(
            snapshot.len() == 64 && snapshot.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Review the selected files before discarding their changes."
        );
        let scope = self.git_review_scope(checkout.clone()).await?;
        let (project, owner) = (scope.project.clone(), scope.owner.clone());
        let store = self.clone();
        let runtime = Handle::current();
        let write = move || {
            let review = LiveReview::open(&scope)?;
            let git_dir = Path::new(&review.paths.git_dir);
            let _directories = lock_directories(git_dir)?;
            let _index_lock =
                lock_git_file(&git_dir.join("index.lock")).context("lock the worktree index")?;
            review.verify()?;
            let (files, observed) = prepare(&scope, &review, paths)?;
            ensure!(
                observed == snapshot,
                "The selected files or their Git comparison changed. Review the batch again."
            );
            runtime.block_on(store.execute(move |db| {
                let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                files.verify_no_drafts(&transaction, &scope.id)?;
                review.verify()?;
                files.publish(&scope.root)
            }))
        };
        match owner {
            Some(owner) => {
                with_managed_worktree_writer(self, project, checkout, owner, move |_| write()).await
            }
            None => tokio::task::spawn_blocking(write).await?,
        }
    }
}

fn selected_paths(mut paths: Vec<String>) -> Result<Vec<String>> {
    ensure!(
        (2..=MAX_BATCH_PATHS).contains(&paths.len()),
        "Select 2 to 256 tracked files to discard in one review."
    );
    let mut aliases = BTreeSet::new();
    for path in &paths {
        verify_discard_path(path)?;
        ensure!(
            aliases.insert(path.to_uppercase()),
            "The selection includes the same Windows file more than once."
        );
    }
    paths.sort();
    Ok(paths)
}

fn prepare(
    scope: &ReviewScope,
    review: &LiveReview,
    paths: Vec<String>,
) -> Result<(ReviewedFiles, String)> {
    let statuses = review.status()?;
    let mut files = ReviewedFiles::default();
    let mut fingerprints = Vec::new();
    let mut total = 0;
    for path in paths {
        let status = statuses
            .iter()
            .find(|entry| {
                entry.path == path && matches!(entry.working, Some('M' | 'D')) && !entry.unmerged()
            })
            .with_context(|| {
                format!("{path} must be a tracked file with an unstaged modification or deletion.")
            })?;
        let parent = files.lock_parent(&scope.root, &path)?;
        let held = open_target(&scope.root.join(&path))?;
        ensure!(
            held.is_some() == (status.working == Some('M')),
            "The working file {path} changed during discard review."
        );
        let before = held
            .as_ref()
            .map(|file| read_bytes(file.try_clone()?))
            .transpose()?;
        let after = read_checkout_bytes(review, &path)?;
        ensure!(
            before.as_deref() != Some(after.as_slice()),
            "The selected Git changes are already absent in {path}."
        );
        total += before.as_ref().map_or(0, Vec::len) + after.len();
        ensure!(
            total <= MAX_BATCH_BYTES,
            "The selected files exceed the combined discard preview limit."
        );
        fingerprints.push((
            path.clone(),
            parent,
            held.as_ref().map(file_identity).transpose()?,
            before.as_deref().map(digest),
            digest(&after),
        ));
        files.entries.push(ReviewedFile {
            path,
            before,
            after: Some(after),
            held,
        });
    }
    let snapshot = digest(&serde_json::to_vec(&(
        "git-discard-batch-v1",
        &scope.id,
        &review.revision,
        &files.missing_folders,
        fingerprints,
    ))?);
    Ok((files, snapshot))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::params;

    use super::*;
    use crate::{
        repository::{git_path, git_query, register},
        store::DraftKey,
    };

    fn git(root: &Path, arguments: &[&str]) -> Result<String> {
        git_query(&git_path()?, root, arguments)
    }

    #[tokio::test]
    async fn discards_the_exact_tracked_batch_after_rechecking_index_files_and_drafts() -> Result<()>
    {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("repo");
        fs::create_dir_all(root.join("notes"))?;
        git(&root, &["init", "--quiet"])?;
        git(&root, &["config", "core.autocrlf", "false"])?;
        let edited = "Álpha.txt";
        let deleted = "notes/deleted.md";
        let other = "unselected.txt";
        let untracked = "untracked.txt";
        fs::write(root.join(edited), "committed\n")?;
        fs::write(root.join(deleted), "restore this\n")?;
        fs::write(root.join(other), "committed unrelated\n")?;
        git(&root, &["add", "--all"])?;
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "Seed",
            ],
        )?;
        fs::write(root.join(edited), "staged\n")?;
        git(&root, &["add", "--", edited])?;
        fs::write(root.join(edited), "working edit\n")?;
        fs::remove_file(root.join(deleted))?;
        fs::remove_dir(root.join("notes"))?;
        fs::write(root.join(other), "working unrelated\n")?;
        fs::write(root.join(untracked), "not tracked\n")?;

        let store = Store::open(fixture.path().join("store.sqlite3"))?;
        let checkout = register(&store, root.clone()).await?.checkout_id;
        let selection = vec![deleted.into(), edited.into()];
        assert!(
            store
                .git_discard_batch_preview(checkout.clone(), vec![edited.into(), untracked.into()])
                .await
                .is_err()
        );
        assert!(
            store
                .git_discard_batch_preview(
                    checkout.clone(),
                    vec![edited.into(), edited.to_uppercase()]
                )
                .await
                .is_err()
        );
        let first = store
            .git_discard_batch_preview(checkout.clone(), selection.clone())
            .await?;
        let rendered = serde_json::to_value(&first.files)?;
        let files = rendered.as_array().unwrap();
        assert!(files.iter().any(|file| file["path"] == deleted
            && file["before"]["status"] == "missing"
            && file["after"]["content"] == "restore this\n"));
        assert!(files.iter().any(|file| file["path"] == edited
            && file["before"]["content"] == "working edit\n"
            && file["after"]["content"] == "staged\n"));
        let index = fs::read(root.join(".git/index"))?;

        let owner = checkout.clone();
        store
            .execute(move |db| {
                DraftKey::CheckoutFile {
                    checkout_id: owner,
                    relative_path: edited.to_uppercase(),
                }
                .write(db, b"protected draft")
            })
            .await?;
        let blocked = store
            .git_discard_batch_changes(checkout.clone(), selection.clone(), first.snapshot.clone())
            .await
            .unwrap_err();
        assert!(format!("{blocked:#}").contains("protected editor drafts"));
        assert_eq!(fs::read_to_string(root.join(edited))?, "working edit\n");
        assert!(!root.join(deleted).exists());
        let owner = checkout.clone();
        store
            .execute(move |db| {
                db.execute(
                    "DELETE FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1",
                    params![owner],
                )?;
                Ok(())
            })
            .await?;

        fs::write(root.join(edited), "new edit after review\n")?;
        assert!(
            store
                .git_discard_batch_changes(
                    checkout.clone(),
                    selection.clone(),
                    first.snapshot.clone()
                )
                .await
                .is_err()
        );
        assert!(!root.join(deleted).exists());
        fs::write(root.join(edited), "working edit\n")?;
        assert_eq!(fs::read(root.join(".git/index"))?, index);
        git(&root, &["add", "--", other])?;
        let current_index = fs::read(root.join(".git/index"))?;
        assert_ne!(index, current_index);
        assert!(
            store
                .git_discard_batch_changes(checkout.clone(), selection.clone(), first.snapshot)
                .await
                .is_err()
        );
        assert!(!root.join(deleted).exists());

        let current = store
            .git_discard_batch_preview(checkout.clone(), selection.clone())
            .await?;
        store
            .git_discard_batch_changes(
                checkout,
                selection.into_iter().rev().collect(),
                current.snapshot,
            )
            .await?;
        assert_eq!(fs::read_to_string(root.join(edited))?, "staged\n");
        assert_eq!(fs::read_to_string(root.join(deleted))?, "restore this\n");
        assert_eq!(fs::read_to_string(root.join(other))?, "working unrelated\n");
        assert_eq!(fs::read_to_string(root.join(untracked))?, "not tracked\n");
        assert_eq!(fs::read(root.join(".git/index"))?, current_index);
        Ok(())
    }
}
