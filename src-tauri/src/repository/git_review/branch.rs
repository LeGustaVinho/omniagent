use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::{providers::configuration::plain_metadata, store::Store};

use super::{ReviewScope, inspect, lock_directories, passive, validate_oid};

impl Store {
    pub async fn create_git_branch(
        &self,
        checkout: String,
        branch: String,
        expected_head: String,
    ) -> Result<()> {
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || create_branch(&scope, &branch, &expected_head)).await?
    }
}

fn create_branch(scope: &ReviewScope, branch: &str, head: &str) -> Result<()> {
    validate_oid(head)?;
    ensure!(
        !branch.is_empty() && !branch.starts_with('-'),
        "Choose a valid local branch name."
    );
    let _checkout = lock_directories(&scope.root)?;
    let _metadata = lock_directories(Path::new(&scope.git_dir))?;
    let _common = lock_directories(Path::new(&scope.common))?;
    let paths = inspect(&scope.root)?;
    ensure!(
        paths.git_dir == scope.git_dir && paths.git_common_dir == scope.common,
        "Checkout identity changed before branch creation."
    );

    let validated = passive(&scope.root, &["check-ref-format", "--branch", branch])
        .context("Choose a valid local branch name.")?;
    // Git can expand the previous-checkout syntax. Create only the literal confirmed name.
    ensure!(
        validated.strip_suffix(b"\n") == Some(branch.as_bytes()),
        "Choose a literal local branch name."
    );
    let kind = passive(&scope.root, &["cat-file", "-t", head])
        .context("The selected commit is unavailable in this checkout.")?;
    ensure!(
        kind == b"commit\n",
        "Select a Git commit as the starting revision."
    );

    let reference = format!("refs/heads/{branch}");
    let common = Path::new(&scope.common);
    let _ = plain_metadata(&common.join(&reference))?;
    let _ = plain_metadata(&common.join("logs").join(&reference))?;
    let absent = "0".repeat(head.len());
    // An all-zero old object ID makes creation atomic and refuses every existing ref.
    passive(
        &scope.root,
        &[
            "update-ref",
            "--no-deref",
            "--create-reflog",
            "-m",
            "OmniAgent: create local branch",
            &reference,
            head,
            &absent,
        ],
    )
    .context("Could not create the local branch.")?;
    Ok(())
}
