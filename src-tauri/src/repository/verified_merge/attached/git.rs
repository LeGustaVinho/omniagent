use std::{
    fs::File,
    io::Seek,
    path::Path,
    process::{Output, Stdio},
};

use anyhow::{Context, Result, ensure};

use crate::repository::{
    CheckoutPaths,
    documents::relative_name,
    git_command, git_path, inspect,
    isolated_git::{IsolatedGit, check},
};

pub(super) struct Delta {
    pub path: String,
    pub before: Option<String>,
    pub after: bool,
}

pub(super) fn checkout_paths(root: &Path, common: &str, branch: &str) -> Result<CheckoutPaths> {
    let paths = inspect(root)?;
    ensure!(
        Path::new(&paths.checkout_root) == root && paths.git_common_dir == common,
        "the attached destination checkout or repository identity changed"
    );
    let reference = check(
        git_command(&git_path()?, root)
            .args(["symbolic-ref", "--quiet", "--no-recurse", "HEAD"])
            .output()?,
        "read the attached destination HEAD",
    )?;
    ensure!(
        reference.trim_ascii_end() == format!("refs/heads/{branch}").as_bytes(),
        "the approved branch is no longer checked out at the destination"
    );
    Ok(paths)
}

pub(super) fn require_idle(git_dir: &Path) -> Result<()> {
    for name in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-apply",
        "rebase-merge",
        "sequencer",
        "BISECT_START",
        "info/sparse-checkout",
    ] {
        ensure!(
            !super::state::exists(&git_dir.join(name))?,
            "the attached destination has a Git operation or sparse checkout in progress ({name})"
        );
    }
    Ok(())
}

pub(super) fn require_clean(paths: &CheckoutPaths, root: &Path, head: &str) -> Result<()> {
    let view = IsolatedGit::open(paths, root, head)?;
    copy_plain_index(&view, paths)?;
    let status = view.read(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ])?;
    ensure!(
        status.is_empty(),
        "the attached destination has staged, modified, or untracked files ({:?})",
        String::from_utf8_lossy(status.split(|byte| *byte == 0).next().unwrap_or_default())
    );
    Ok(())
}

pub(super) fn require_exact_index(paths: &CheckoutPaths, root: &Path, head: &str) -> Result<()> {
    let view = IsolatedGit::open(paths, root, head)?;
    copy_plain_index(&view, paths)?;
    let staged = view.read(&[
        "diff-index",
        "--cached",
        "--ita-visible-in-index",
        "-z",
        head,
        "--",
    ])?;
    ensure!(
        staged.is_empty(),
        "the destination index no longer matches the approved integration tree"
    );
    Ok(())
}

fn copy_plain_index(view: &IsolatedGit, paths: &CheckoutPaths) -> Result<()> {
    view.copy_index(paths)?;
    let flags = view.read(&["ls-files", "-v", "-z"])?;
    ensure!(
        flags
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .all(|entry| entry.starts_with(b"H ")),
        "the attached destination index contains sparse, assumed, or unmerged entries"
    );
    Ok(())
}

pub(super) fn require_tip(root: &Path, head: &str) -> Result<()> {
    let actual = check(
        git_command(&git_path()?, root)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()?,
        "read the attached destination tip",
    )?;
    ensure!(
        actual.trim_ascii_end() == head.as_bytes(),
        "the attached destination changed before its files were prepared"
    );
    Ok(())
}

pub(super) fn deltas(view: &IsolatedGit, root: &Path, old: &str, new: &str) -> Result<Vec<Delta>> {
    let diff = view.read(&[
        "diff-tree",
        "--raw",
        "--no-abbrev",
        "--no-commit-id",
        "--no-renames",
        "-r",
        "-z",
        old,
        new,
    ])?;
    let diff = std::str::from_utf8(&diff)?;
    let mut fields = diff.split_terminator('\0');
    let mut paths = Vec::new();
    while let Some(metadata) = fields.next() {
        let path = fields
            .next()
            .context("Git omitted a changed destination path")?;
        let mut metadata = metadata
            .strip_prefix(':')
            .context("invalid Git tree delta")?
            .split(' ');
        let preimage = metadata
            .next()
            .context("Git omitted the previous file mode")?;
        let target = metadata
            .next()
            .context("Git omitted the target file mode")?;
        let old_blob = metadata.next().context("Git omitted the previous blob")?;
        let regular_mode = |mode| matches!(mode, "000000" | "100644" | "100755");
        ensure!(
            regular_mode(preimage) && regular_mode(target),
            "the attached update contains a link or submodule at {path}"
        );
        validate_relative(root, path)?;
        paths.push(Delta {
            path: path.into(),
            before: (preimage != "000000").then(|| old_blob.into()),
            after: target != "000000",
        });
    }
    Ok(paths)
}

pub(super) fn worktree_blob(
    view: &IsolatedGit,
    commit: &str,
    path: &str,
    mut file: File,
) -> Result<String> {
    file.rewind()?;
    let digest = check(
        view.command()
            .env("GIT_ATTR_SOURCE", commit)
            .args(["hash-object", "--stdin", "--path", path])
            .stdin(Stdio::from(file))
            .output()?,
        "verify the existing file against its pinned Git blob",
    )?;
    Ok(std::str::from_utf8(&digest)?.trim_end().into())
}

pub(super) fn validate_relative(root: &Path, path: &str) -> Result<()> {
    ensure!(
        relative_name(root, &root.join(path))? == path
            && !path.contains(['\\', ':'])
            && !path
                .split('/')
                .any(|name| name.eq_ignore_ascii_case(".git") || name.ends_with(['.', ' '])),
        "the integration path is unsafe in the destination: {path}"
    );
    Ok(())
}

pub(super) fn block_filters(view: &IsolatedGit, commit: &str, deltas: &[Delta]) -> Result<()> {
    for chunk in deltas.chunks(32) {
        let output = check(
            view.command()
                .arg("check-attr")
                .arg(format!("--source={commit}"))
                .args(["-z", "filter", "--"])
                .args(chunk.iter().map(|entry| &entry.path))
                .output()?,
            "read filter attributes for the integration paths",
        )?;
        let data = std::str::from_utf8(&output)?
            .split_terminator('\0')
            .collect::<Vec<_>>();
        ensure!(
            data.len() == chunk.len() * 3,
            "Git returned incomplete checkout attributes"
        );
        for item in data.chunks_exact(3) {
            ensure!(
                matches!(item[2], "unspecified" | "unset"),
                "the attached update requires a checkout filter for {}",
                item[0]
            );
        }
    }
    Ok(())
}

pub(super) fn materialize(
    view: &IsolatedGit,
    worktree: &Path,
    index: &Path,
    commit: &str,
) -> Result<()> {
    let output: Output = view
        .command()
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_INDEX_FILE", index)
        .env("GIT_ATTR_SOURCE", commit)
        .args([
            "-c",
            "core.protectNTFS=true",
            "read-tree",
            "-m",
            "-u",
            "--no-sparse-checkout",
            commit,
        ])
        .output()?;
    check(output, "materialize the pinned tree in scratch")?;
    Ok(())
}

pub(super) fn prepare_index(view: &IsolatedGit, index: &Path, commit: &str) -> Result<()> {
    let output = view
        .command()
        .env("GIT_INDEX_FILE", index)
        .args(["read-tree", "--no-sparse-checkout", commit])
        .output()?;
    check(
        output,
        "prepare the integration index independently of scratch files",
    )?;
    Ok(())
}
