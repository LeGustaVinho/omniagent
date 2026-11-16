use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use super::{
    CheckoutLayout, git_command, git_path, inspect,
    verification::{MaterializedCandidate, materialize::ScratchGit},
};
use crate::providers::configuration::plain_metadata;

pub(crate) fn materialize_comparison(
    source: &CheckoutLayout,
    commit: &str,
    storage: &Path,
) -> Result<(MaterializedCandidate, PathBuf, String)> {
    ensure!(
        (commit.len() == 40 || commit.len() == 64)
            && commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Use the full Git commit SHA as the shared comparison context"
    );
    let registered = inspect(&source.root)?;
    fs::create_dir_all(storage)?;
    let storage = dunce::canonicalize(storage)?;
    ensure!(
        !storage.starts_with(&source.root) && !storage.starts_with(&source.primary_root),
        "The comparison checkout must be outside the source repository"
    );
    let (checkout, tree) =
        ScratchGit::new_in(Path::new(&registered.git_common_dir), commit, &storage)?
            .materialize_commit(commit)?;
    let root = dunce::canonicalize(checkout.path())?;
    verify_comparison(&root, commit, &tree)?;
    Ok((checkout, root, tree))
}

pub(crate) fn verify_comparison(root: &Path, commit: &str, tree: &str) -> Result<CheckoutLayout> {
    ensure!(
        dunce::canonicalize(root)? == root
            && plain_metadata(&root.join(".git"))?.is_some_and(|entry| entry.is_dir()),
        "The isolated comparison checkout changed identity"
    );
    ensure!(
        !root.join(".git/objects/info/alternates").exists(),
        "The comparison checkout borrows mutable Git objects"
    );
    let git = git_path()?;
    let query = |arguments: &[&str]| -> Result<String> {
        let output = git_command(&git, root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join(".git/empty-config"))
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.symlinks=false",
            ])
            .args(arguments)
            .output()?;
        ensure!(
            output.status.success(),
            "Git could not verify the isolated comparison checkout"
        );
        Ok(String::from_utf8(output.stdout)?)
    };
    let revisions = query(&["rev-parse", "--verify", "HEAD"])?;
    let current_tree = query(&["rev-parse", "--verify", "HEAD^{tree}"])?;
    ensure!(
        revisions.trim() == commit && current_tree.trim() == tree,
        "The comparison checkout moved away from its pinned Git commit"
    );
    ensure!(
        query(&[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored"
        ])?
        .is_empty(),
        "The comparison checkout has changes outside its pinned Git tree"
    );
    Ok(CheckoutLayout {
        root: root.into(),
        cwd: root.into(),
        primary_root: root.into(),
    })
}
