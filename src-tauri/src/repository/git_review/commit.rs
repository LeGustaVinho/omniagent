use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};

use crate::{providers::configuration::plain_metadata, store::Store};

use super::super::isolated_git::check;
use super::{
    GitReviewRevision, LiveReview, ReviewScope, git_command, git_path, lock_directories,
    lock_git_file, validate_oid,
};

impl Store {
    pub async fn commit_git_index(
        &self,
        checkout: String,
        message: String,
        expected: GitReviewRevision,
    ) -> Result<String> {
        let message = message.trim().to_owned();
        ensure!(
            !message.is_empty() && message.len() <= 8 * 1024 && !message.contains('\0'),
            "enter a Git commit message of up to 8 KiB"
        );
        let scope = self.git_review_scope(checkout).await?;
        ensure!(
            scope.owner.is_none(),
            "use the Task delivery to commit a managed worktree"
        );
        tokio::task::spawn_blocking(move || commit_index(&scope, &message, &expected)).await?
    }
}

fn commit_index(
    scope: &ReviewScope,
    message: &str,
    expected: &GitReviewRevision,
) -> Result<String> {
    let review = LiveReview::open(scope)?;
    ensure!(
        &review.revision == expected,
        "the Git branch, HEAD or index changed; refresh the checkout and review it again"
    );
    let branch = expected
        .branch
        .as_deref()
        .filter(|branch| branch.starts_with("refs/heads/"))
        .context("check out a local branch before committing")?;
    require_files_references(&scope.root)?;
    let git_dir = Path::new(&review.paths.git_dir);
    let _directories = lock_directories(git_dir)?;
    let _index = lock_git_file(&git_dir.join("index.lock")).context("lock the worktree index")?;
    let _head = lock_git_file(&git_dir.join("HEAD.lock")).context("lock the worktree branch")?;
    review.verify()?;
    let target = git_command(&git_path()?, &scope.root)
        .args(["symbolic-ref", "--quiet", "--no-recurse", branch])
        .output()?;
    ensure!(
        target.status.code() == Some(1) && target.stderr.is_empty(),
        "check out a direct local branch before committing"
    );
    for operation in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        ensure!(
            plain_metadata(&git_dir.join(operation))?.is_none(),
            "finish the Git operation already in progress before committing"
        );
    }
    ensure!(
        review.status()?.iter().any(|entry| entry.staged.is_some()),
        "there are no staged changes to commit"
    );
    let tree = text_oid(review.view.read(&["write-tree"])?)?;
    if let Some(parent) = expected.head.as_deref() {
        let previous = text_oid(
            review
                .view
                .read(&["rev-parse", &format!("{parent}^{{tree}}")])?,
        )?;
        ensure!(tree != previous, "there are no staged changes to commit");
    }

    let author = identity(&scope.root, "GIT_AUTHOR_IDENT")?;
    let committer = identity(&scope.root, "GIT_COMMITTER_IDENT")?;
    let mut command = review.view.command();
    command
        .env("GIT_AUTHOR_NAME", &author.0)
        .env("GIT_AUTHOR_EMAIL", &author.1)
        .env("GIT_COMMITTER_NAME", &committer.0)
        .env("GIT_COMMITTER_EMAIL", &committer.1)
        .args(["commit-tree", "--no-gpg-sign", &tree, "-m", message]);
    if let Some(parent) = expected.head.as_deref() {
        command.args(["-p", parent]);
    }
    let sha = text_oid(check(
        command.output()?,
        "create the reviewed index commit",
    )?)?;

    // Git implicitly locks and writes the current checkout's HEAD reflog when
    // advancing its branch. Use a detached app-owned context for the common
    // ref update while holding the real checkout's Git HEAD and index locks.
    let metadata = tempfile::tempdir()?;
    fs::write(metadata.path().join("HEAD"), format!("{sha}\n"))?;
    let common = review.paths.git_common_dir.replace('\\', "/");
    fs::write(metadata.path().join("commondir"), format!("{common}\n"))?;
    let absent = "0".repeat(sha.len());
    review.verify()?;
    check(
        git_command(&git_path()?, &scope.root)
            .env("GIT_DIR", metadata.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_COMMITTER_NAME", &committer.0)
            .env("GIT_COMMITTER_EMAIL", &committer.1)
            .args([
                "-c",
                "core.hooksPath=NUL",
                "update-ref",
                "--no-deref",
                "--create-reflog",
                "-m",
                "OmniAgent staged index commit",
                branch,
                &sha,
                expected.head.as_deref().unwrap_or(&absent),
            ])
            .output()?,
        "advance only the reviewed branch",
    )?;
    Ok(sha)
}

fn identity(root: &Path, variable: &str) -> Result<(String, String)> {
    // Resolve normal user and repository Git identity without running hooks or
    // signing programs. The isolated commit receives the resulting fields.
    let bytes = check(
        git_command(&git_path()?, root)
            .args(["var", variable])
            .output()?,
        "read the configured Git author and committer identity",
    )?;
    let text = String::from_utf8(bytes)?;
    let (identity, _) = text.rsplit_once('>').context("invalid Git identity")?;
    let (name, email) = identity.rsplit_once('<').context("invalid Git identity")?;
    Ok((name.trim_end().to_owned(), email.to_owned()))
}

fn require_files_references(root: &Path) -> Result<()> {
    let storage = git_command(&git_path()?, root)
        .args(["config", "--local", "--get", "extensions.refStorage"])
        .output()?;
    if storage.status.code() == Some(1) && storage.stderr.is_empty() {
        return Ok(()); // The files backend is the unnamed Git default.
    }
    ensure!(
        check(storage, "identify the repository reference backend")?
            .trim_ascii()
            .eq_ignore_ascii_case(b"files"),
        "committing from this workspace requires Git's files reference backend"
    );
    Ok(())
}

fn text_oid(bytes: Vec<u8>) -> Result<String> {
    let text = String::from_utf8(bytes)?;
    let oid = text.trim();
    validate_oid(oid)?;
    Ok(oid.into())
}
