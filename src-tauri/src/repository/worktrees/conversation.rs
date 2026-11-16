use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Seek},
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::providers::configuration::open_locked_local_file;

use super::{
    Provision, Source, base_commit, check, check_branch, git_path, inspect, materialize_base,
    normalize_root, persist_checkout, prepare_named_provision, validate_source,
};
use crate::repository::{
    ProjectCheckout,
    documents::{lock_directories, open_for_rename, relative_name, rename},
    isolated_git::IsolatedGit,
    writers::{WriterConflict, writer_conflict},
};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ConversationWorktreePreview {
    pub project_id: String,
    pub repository_root: String,
    pub source_checkout_id: String,
    pub checkout_root: PathBuf,
    pub base_commit: Option<String>,
    pub dirty_paths: Vec<String>,
    pub selected_paths: Vec<String>,
    pub target_checkout_root: Option<PathBuf>,
    pub target_branch: Option<String>,
    source_git_dir: String,
    common: String,
    selected: Vec<SelectedFile>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SelectedFile {
    path: String,
    before_blob: Option<String>,
    digest: Option<String>,
}

pub(crate) fn inspect_fork_checkout(
    db: &Connection,
    checkout_id: &str,
    preview_id: &str,
    separate: bool,
    selected_paths: Vec<String>,
    default_root: &Path,
) -> Result<ConversationWorktreePreview> {
    let (project_id, source) = registered_source(db, checkout_id)?;
    validate_source(&source)?;
    let paths = inspect(&source.root)?;
    let head = base_commit(&source.root)?;
    let mut result = ConversationWorktreePreview {
        project_id,
        repository_root: paths.repository_root.clone(),
        source_checkout_id: checkout_id.into(),
        checkout_root: source.root.clone(),
        base_commit: head.clone(),
        dirty_paths: Vec::new(),
        selected_paths,
        target_checkout_root: None,
        target_branch: None,
        source_git_dir: source.git_dir.clone(),
        common: source.common.clone(),
        selected: Vec::new(),
    };
    if !separate {
        ensure!(
            result.selected_paths.is_empty(),
            "dirty paths are only selected for a separate worktree"
        );
        return Ok(result);
    }
    require_idle_writer(db, checkout_id)?;
    let base = head
        .as_deref()
        .context("the source checkout has no base commit")?;
    let view = IsolatedGit::open(&paths, &source.root, base)?;
    let dirty = changed_paths(&view, &paths, base)?;
    let mut used = BTreeSet::new();
    for path in &result.selected_paths {
        safe_path(&source.root, path)?;
        ensure!(
            dirty.contains(path) && used.insert(path.to_uppercase()),
            "select each available dirty path once"
        );
        let before_blob = old_blob(&view, base, path)?;
        let digest = file_digest(&source.root.join(path))?;
        ensure!(
            digest.is_some() || before_blob.is_some(),
            "a selected path has no working copy change"
        );
        result.selected.push(SelectedFile {
            path: path.clone(),
            before_blob,
            digest,
        });
    }
    ensure!(
        base_commit(&source.root)? == head,
        "the source HEAD changed during checkout review"
    );
    let worktree_root = normalize_root(source.override_root.as_deref().unwrap_or(default_root))?;
    ensure!(
        !worktree_root.starts_with(&source.root)
            && !worktree_root.starts_with(&paths.repository_root),
        "place the separate checkout outside the source repository working trees"
    );
    let planned = prepare_named_provision(
        &source,
        &git_path()?,
        default_root,
        &result.project_id,
        preview_id,
        "conversation",
    )?;
    ensure!(
        Some(&planned.base_commit) == head.as_ref(),
        "the source HEAD changed during checkout review"
    );
    result.dirty_paths = dirty.into_iter().collect();
    result.target_checkout_root = Some(planned.root);
    result.target_branch = Some(planned.branch);
    Ok(result)
}

pub(crate) fn materialize_fork_checkout(
    db: &Connection,
    preview: &ConversationWorktreePreview,
    preview_id: &str,
) -> Result<ProjectCheckout> {
    let (project_id, source) = registered_source(db, &preview.source_checkout_id)?;
    ensure!(
        project_id == preview.project_id
            && source.root == preview.checkout_root
            && source.git_dir == preview.source_git_dir
            && source.common == preview.common,
        "the source checkout registration changed after review"
    );
    validate_source(&source)?;
    let base = base_commit(&source.root)?;
    ensure!(
        base == preview.base_commit,
        "the source HEAD changed after checkout review"
    );
    let Some(root) = preview.target_checkout_root.as_ref() else {
        let paths = inspect(&source.root)?;
        return Ok(ProjectCheckout {
            project_id,
            repository_root: paths.repository_root,
            checkout_id: preview.source_checkout_id.clone(),
            checkout_root: paths.checkout_root,
        });
    };
    require_idle_writer(db, &preview.source_checkout_id)?;
    let base = base.context("the source checkout has no base commit")?;
    let provision = Provision {
        root: root.clone(),
        branch: preview
            .target_branch
            .clone()
            .context("the fork branch is unpinned")?,
        base_commit: base.clone(),
    };
    let mut source_files = Vec::new();
    for selected in &preview.selected {
        let path = source.root.join(&selected.path);
        safe_path(&source.root, &selected.path)?;
        let file = opened_file(&path)?;
        ensure!(
            file.as_ref()
                .map(|file| file.try_clone().and_then(digest))
                .transpose()?
                == selected.digest,
            "the selected source file changed before the fork: {}",
            selected.path
        );
        source_files.push(file);
    }
    let _parent_guard = lock_directories(
        root.parent()
            .context("the planned worktree has no parent")?,
    )?;
    let git = git_path()?;
    let (checkout, _) = materialize_base(&git, &source, &provision)?;
    let _checkout_guard = lock_directories(root)?;
    let view = IsolatedGit::open(&checkout, &source.root, &base)?;
    let dirty = changed_paths(&view, &checkout, &base)?;
    ensure!(
        view.read(&["diff", "--cached", "--name-only", "--"])?
            .is_empty(),
        "the unfinished fork has a changed Git index"
    );
    ensure!(
        dirty
            .iter()
            .all(|path| preview.selected_paths.contains(path)),
        "the unfinished fork contains unreviewed paths"
    );
    if !preview.selected.is_empty() {
        let vault = root.with_file_name(format!(".omniagent-fork-{preview_id}"));
        if !path_exists(&vault)? {
            fs::create_dir(&vault)?;
        }
        let _vault_guard = lock_directories(&vault)?;
        for (ordinal, (selected, file)) in preview.selected.iter().zip(source_files).enumerate() {
            install_selected(&view, root, &vault, selected, file, ordinal, &base)?;
        }
    }
    ensure!(
        base_commit(&source.root)?.as_deref() == Some(base.as_str()),
        "the source HEAD moved during provisioning"
    );
    ensure!(
        base_commit(root)?.as_deref() == Some(base.as_str()),
        "the fork moved away from its pinned base"
    );
    check_branch(root, &git, &provision.branch)?;
    let inventory = crate::customizations::inspect(root, root)?;
    let saved = persist_checkout(db, checkout, &inventory)?;
    ensure!(
        saved.project_id == project_id,
        "the fork checkout belongs to a different project"
    );
    Ok(saved)
}

fn registered_source(db: &Connection, checkout: &str) -> Result<(String, Source)> {
    db.query_row(
        "SELECT p.id, c.root, c.git_dir, p.git_common_dir, r.root FROM checkouts c JOIN projects p ON p.id = c.project_id
         LEFT JOIN project_worktree_roots r ON r.project_id = p.id WHERE c.id = ?1", [checkout],
        |row| Ok((row.get(0)?, Source { root: PathBuf::from(row.get::<_, String>(1)?), git_dir: row.get(2)?, common: row.get(3)?,
            override_root: row.get::<_, Option<String>>(4)?.map(PathBuf::from) })),
    ).optional()?.context("the conversation checkout is unavailable")
}

fn require_idle_writer(db: &Connection, checkout: &str) -> Result<()> {
    ensure!(
        !matches!(
            writer_conflict(db, checkout, "")?,
            Some(WriterConflict::Run { .. } | WriterConflict::App(_))
        ),
        "finish the existing worktree write before creating a conversation fork"
    );
    Ok(())
}

fn changed_paths(
    view: &IsolatedGit,
    paths: &super::CheckoutPaths,
    base: &str,
) -> Result<BTreeSet<String>> {
    view.copy_index(paths)?;
    let tracked = view.read(&[
        "diff",
        "--no-renames",
        "--name-only",
        "--no-ext-diff",
        "--no-textconv",
        "-z",
        base,
        "--",
    ])?;
    let untracked = view.read(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok(std::str::from_utf8(&tracked)?
        .split_terminator('\0')
        .chain(std::str::from_utf8(&untracked)?.split_terminator('\0'))
        .map(str::to_owned)
        .collect())
}

fn old_blob(view: &IsolatedGit, base: &str, path: &str) -> Result<Option<String>> {
    let tree = view.read(&[
        "ls-tree",
        "--full-tree",
        "-z",
        base,
        "--",
        &format!(":(top,literal){path}"),
    ])?;
    if tree.is_empty() {
        return Ok(None);
    }
    let tree = std::str::from_utf8(&tree)?;
    let (metadata, name) = tree
        .trim_end_matches('\0')
        .split_once('\t')
        .context("the source path is not a file")?;
    let mut metadata = metadata.split(' ');
    ensure!(
        name == path
            && matches!(metadata.next(), Some("100644" | "100755"))
            && metadata.next() == Some("blob"),
        "the source path is a link, submodule, or directory: {path}"
    );
    Ok(Some(
        metadata
            .next()
            .context("the source path has no base blob")?
            .into(),
    ))
}

fn safe_path(root: &Path, path: &str) -> Result<()> {
    ensure!(
        relative_name(root, &root.join(path))? == path
            && !path.contains(['\\', ':'])
            && !path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git") || part.ends_with(['.', ' '])),
        "the selected path is outside the safe checkout files: {path}"
    );
    Ok(())
}

fn opened_file(path: &Path) -> Result<Option<File>> {
    let parent = path.parent().context("the selected file has no parent")?;
    let _anchor = match lock_directories(parent) {
        Ok(anchor) => anchor,
        Err(err)
            if err
                .downcast_ref::<io::Error>()
                .is_some_and(|err| err.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    match open_locked_local_file(path) {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn file_digest(path: &Path) -> Result<Option<String>> {
    opened_file(path)?
        .map(digest)
        .transpose()
        .map_err(Into::into)
}

fn digest(mut file: File) -> io::Result<String> {
    file.rewind()?;
    let mut digest = Sha256::new();
    io::copy(&mut file, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn ensure_parents(root: &Path, parent: &Path) -> Result<()> {
    let mut path = root.to_owned();
    for part in parent.strip_prefix(root)? {
        let anchor = lock_directories(&path)?;
        path.push(part);
        if !path_exists(&path)? {
            fs::create_dir(&path)?;
        }
        drop(anchor);
    }
    Ok(())
}

fn install_selected(
    view: &IsolatedGit,
    root: &Path,
    vault: &Path,
    change: &SelectedFile,
    source: Option<File>,
    ordinal: usize,
    base: &str,
) -> Result<()> {
    let live = root.join(&change.path);
    let current = file_digest(&live)?;
    let backup = vault.join(format!("{ordinal}.before"));
    if current == change.digest {
        if current.is_some() || change.before_blob.is_none() {
            return Ok(());
        }
        if path_exists(&backup)? {
            verify_base_file(view, base, change, &open_for_rename(&backup)?)?;
            return Ok(());
        }
    }
    let parent = live
        .parent()
        .context("the selected file has no destination parent")?;
    ensure_parents(root, parent)?;
    let _anchor = lock_directories(parent)?;
    if current.is_some() {
        let winning = open_for_rename(&live)?;
        verify_base_file(view, base, change, &winning)?;
        rename(&winning, &backup)?;
    } else if change.before_blob.is_some() {
        verify_base_file(view, base, change, &open_for_rename(&backup)?)?;
    }
    if let Some(mut source) = source {
        source.rewind()?;
        let staged = vault.join(format!("{ordinal}.next"));
        if !path_exists(&staged)? {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)?;
            io::copy(&mut source, &mut file)?;
            file.sync_all()?;
        }
        let winning = open_for_rename(&staged)?;
        ensure!(
            Some(digest(winning.try_clone()?)?) == change.digest,
            "the staged fork file changed: {}",
            change.path
        );
        rename(&winning, &live)?;
    }
    ensure!(
        file_digest(&live)? == change.digest,
        "the fork's selected file could not be verified: {}",
        change.path
    );
    Ok(())
}

fn verify_base_file(
    view: &IsolatedGit,
    base: &str,
    change: &SelectedFile,
    file: &File,
) -> Result<()> {
    let mut source = file.try_clone()?;
    source.rewind()?;
    let hash = check(
        view.command()
            .env("GIT_ATTR_SOURCE", base)
            .args(["hash-object", "--stdin", "--path", &change.path])
            .stdin(Stdio::from(source))
            .output()?,
        "verify the fork file preimage",
    )?;
    let hash = std::str::from_utf8(&hash)?.trim_end();
    ensure!(
        Some(hash) == change.before_blob.as_deref(),
        "the unfinished fork contains an unreviewed change at {}",
        change.path
    );
    Ok(())
}
