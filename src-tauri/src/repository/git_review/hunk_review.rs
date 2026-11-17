use std::collections::HashSet;

use anyhow::{Result, ensure};
use rusqlite::params;
use serde::Deserialize;

use crate::store::Store;

use super::{GitDiffSource, GitReviewDiff};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHunkReviewRequest {
    path: String,
    source: GitDiffSource,
    snapshot: String,
    hunk_id: String,
    reviewed: bool,
}

pub(super) async fn load(store: &Store, checkout: String, diff: &mut GitReviewDiff) -> Result<()> {
    let Some(scope) = &diff.comment_scope else {
        return Ok(());
    };
    if diff.hunks.is_empty() {
        return Ok(());
    }
    let snapshot = scope.snapshot.clone();
    let reviewed = store.execute(move |db| {
        let mut query = db.prepare(
            "SELECT hunk_id FROM git_reviewed_hunks WHERE checkout_id = ?1 AND comparison_snapshot = ?2",
        )?;
        Ok(query.query_map(params![checkout, snapshot], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?)
    }).await?;
    for hunk in &mut diff.hunks {
        hunk.reviewed = reviewed.contains(&hunk.id);
    }
    Ok(())
}

impl Store {
    pub async fn set_git_hunk_reviewed(
        &self,
        checkout: String,
        request: GitHunkReviewRequest,
    ) -> Result<()> {
        let GitHunkReviewRequest {
            path,
            source,
            snapshot,
            hunk_id,
            reviewed,
        } = request;
        let diff = self
            .git_checkout_diff(checkout.clone(), path, source)
            .await?;
        ensure!(
            diff.comment_scope
                .is_some_and(|scope| scope.snapshot == snapshot)
                && diff.hunks.iter().any(|hunk| hunk.id == hunk_id),
            "The Git comparison changed. Review the current hunk again."
        );
        self.execute(move |db| {
            let query = if reviewed {
                "INSERT INTO git_reviewed_hunks(checkout_id, comparison_snapshot, hunk_id) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING"
            } else {
                "DELETE FROM git_reviewed_hunks WHERE checkout_id = ?1 AND comparison_snapshot = ?2 AND hunk_id = ?3"
            };
            db.execute(query, params![checkout, snapshot, hunk_id])?;
            Ok(())
        }).await
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use crate::repository::{git_path, git_query, register};

    use super::*;

    fn git(root: &Path, args: &[&str]) -> Result<String> {
        git_query(&git_path()?, root, args)
    }

    fn review_request(diff: &GitReviewDiff, index: usize, reviewed: bool) -> GitHunkReviewRequest {
        GitHunkReviewRequest {
            path: diff.path.clone(),
            source: diff.source,
            snapshot: diff.comment_scope.as_ref().unwrap().snapshot.clone(),
            hunk_id: diff.hunks[index].id.clone(),
            reviewed,
        }
    }

    #[tokio::test]
    async fn keeps_review_state_for_the_exact_checkout_revision_and_hunk_only() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("repo");
        fs::create_dir(&root)?;
        git(&root, &["init", "--quiet"])?;
        git(&root, &["config", "core.autocrlf", "false"])?;
        let path = "scope.txt";
        let base: String = (1..=30)
            .map(|number| format!("line {number:02}\n"))
            .collect();
        let changed = base
            .replace("line 02", "first edit")
            .replace("line 27", "second edit");
        fs::write(root.join(path), &base)?;
        git(&root, &["add", "--", path])?;
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "Seed",
            ],
        )?;
        fs::write(root.join(path), &changed)?;
        let linked = fixture.path().join("another-worktree");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked.to_str().unwrap(),
            ],
        )?;
        fs::write(linked.join(path), &changed)?;

        let database = fixture.path().join("workbench.sqlite3");
        let store = Store::open(database.clone())?;
        let checkout = register(&store, root.clone()).await?.checkout_id;
        let other = register(&store, linked).await?.checkout_id;
        let source = GitDiffSource::Working;
        let before = store
            .git_checkout_diff(checkout.clone(), path.into(), source)
            .await?;
        assert_eq!(before.hunks.len(), 2);
        let index_bytes = fs::read(root.join(".git/index"))?;
        for _ in 0..2 {
            store
                .set_git_hunk_reviewed(checkout.clone(), review_request(&before, 0, true))
                .await?;
        }
        assert_eq!(fs::read(root.join(".git/index"))?, index_bytes);
        assert_eq!(fs::read_to_string(root.join(path))?, changed);
        let mut forged = review_request(&before, 0, true);
        forged.hunk_id = "unknown hunk".into();
        assert!(
            store
                .set_git_hunk_reviewed(checkout.clone(), forged)
                .await
                .is_err()
        );
        assert!(
            store
                .set_git_hunk_reviewed(other.clone(), review_request(&before, 0, true))
                .await
                .is_err()
        );
        assert!(
            store
                .git_checkout_diff(other, path.into(), source)
                .await?
                .hunks
                .iter()
                .all(|hunk| !hunk.reviewed)
        );
        drop(store);

        let store = Store::open(database)?;
        let stored = store
            .git_checkout_diff(checkout.clone(), path.into(), source)
            .await?;
        assert_eq!(
            stored
                .hunks
                .iter()
                .map(|hunk| hunk.reviewed)
                .collect::<Vec<_>>(),
            [true, false]
        );
        fs::write(
            root.join(path),
            changed.replace("second edit", "new second edit"),
        )?;
        assert!(
            store
                .set_git_hunk_reviewed(checkout.clone(), review_request(&stored, 0, false))
                .await
                .is_err()
        );
        assert!(
            store
                .git_checkout_diff(checkout.clone(), path.into(), source)
                .await?
                .hunks
                .iter()
                .all(|hunk| !hunk.reviewed)
        );

        fs::write(root.join(path), &changed)?;
        store
            .set_git_hunk_reviewed(checkout.clone(), review_request(&stored, 0, false))
            .await?;
        assert!(
            store
                .git_checkout_diff(checkout.clone(), path.into(), source)
                .await?
                .hunks
                .iter()
                .all(|hunk| !hunk.reviewed)
        );
        assert_eq!(fs::read(root.join(".git/index"))?, index_bytes);

        git(&root, &["add", "--", path])?;
        let staged = store
            .git_checkout_diff(checkout.clone(), path.into(), GitDiffSource::Staged)
            .await?;
        assert!(staged.hunks.iter().all(|hunk| !hunk.reviewed));
        store
            .set_git_hunk_reviewed(checkout.clone(), review_request(&staged, 1, true))
            .await?;
        let staged = store
            .git_checkout_diff(checkout, path.into(), GitDiffSource::Staged)
            .await?;
        assert_eq!(
            staged
                .hunks
                .iter()
                .map(|hunk| hunk.reviewed)
                .collect::<Vec<_>>(),
            [false, true]
        );
        Ok(())
    }
}
