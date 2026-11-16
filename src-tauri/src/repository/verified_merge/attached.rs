mod git;
mod state;
#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::git::validate_oid;
use crate::{
    providers::configuration::open_locked_local_file,
    repository::{
        CheckoutPaths,
        documents::{file_identity, lock_directories},
        isolated_git::IsolatedGit,
    },
};
use state::{
    FileStamp, FileState, NewDirectory, Progress, directory_identity, exists, lock_index,
    stage_file,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AttachedMergePlan {
    checkout: PathBuf,
    git_dir: PathBuf,
    common: String,
    branch: String,
    previous: String,
    integration: String,
    id: Uuid,
    checkout_identity: String,
    git_dir_identity: String,
    metadata_identity: String,
    backups_identity: String,
    parents: BTreeMap<String, String>,
    new_directories: Vec<NewDirectory>,
    files: Vec<ChangedFile>,
    index: FileState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ChangedFile {
    path: String,
    state: FileState,
}

#[derive(Debug, Clone)]
pub(super) struct AttachedRecovery {
    pub checkout: PathBuf,
    pub recovery_directory: PathBuf,
    pub message: String,
}

impl fmt::Display for AttachedRecovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}; recovery files are at {}",
            self.message,
            self.recovery_directory.display()
        )
    }
}

impl std::error::Error for AttachedRecovery {}

pub(super) struct AttachedMerge<'a> {
    plan: &'a AttachedMergePlan,
    paths: CheckoutPaths,
    _index_guard: File,
    _anchors: Vec<File>,
    output_locks: RefCell<Vec<File>>,
}

impl AttachedMergePlan {
    pub(super) fn prepare(
        checkout: &Path,
        common: &str,
        branch: &str,
        previous: &str,
        integration: &str,
    ) -> Result<Self> {
        validate_oid(previous)?;
        validate_oid(integration)?;
        let checkout = dunce::canonicalize(checkout)?;
        let paths = git::checkout_paths(&checkout, common, branch)?;
        let git_dir = PathBuf::from(&paths.git_dir);
        let checkout_guard = lock_directories(&checkout)?;
        let git_dir_guard = lock_directories(&git_dir)?;
        let checkout_identity = identity(&checkout_guard)?;
        let git_dir_identity = identity(&git_dir_guard)?;
        let _index_guard = lock_index(&git_dir)?;
        git::require_idle(&git_dir)?;
        git::require_tip(&checkout, previous)?;
        git::require_clean(&paths, &checkout, previous)?;
        let old_index = FileStamp::inspect(&git_dir.join("index"))?
            .context("the attached destination has no index")?;
        let view = IsolatedGit::open(&paths, &checkout, previous)?;
        let deltas = git::deltas(&view, &checkout, previous, integration)?;
        git::block_filters(&view, previous, &deltas)?;
        git::block_filters(&view, integration, &deltas)?;
        let after = tempfile::tempdir()?;
        let indexes = tempfile::tempdir()?;
        let target_index = indexes.path().join("after");
        git::materialize(
            &view,
            after.path(),
            &indexes.path().join("checkout"),
            integration,
        )?;
        git::prepare_index(&view, &target_index, integration)?;

        let id = Uuid::new_v4();
        let metadata = metadata_path(&git_dir, id);
        let backups = backups_path(
            &checkout,
            &metadata,
            &checkout_identity,
            &git_dir_identity,
            id,
        )?;
        let (mut parents, mut missing) = (BTreeMap::new(), BTreeMap::new());
        let mut validated = Vec::new();
        let mut aliases = BTreeMap::new();
        for delta in deltas {
            let live = checkout.join(&delta.path);
            for path in live.ancestors().take_while(|path| *path != checkout) {
                let name = crate::repository::documents::relative_name(&checkout, path)?;
                ensure!(
                    aliases
                        .insert(name.to_uppercase(), name.clone())
                        .is_none_or(|prior| prior == name),
                    "the integration includes colliding Windows paths"
                );
            }
            collect_parents(&checkout, &live, &mut parents, &mut missing)?;
            let original = FileStamp::inspect(&live)?;
            if let Some(expected) = &delta.before {
                let checkout_file = original
                    .as_ref()
                    .with_context(|| format!("{} was removed from the destination", delta.path))?;
                ensure!(
                    git::worktree_blob(&view, previous, &delta.path, checkout_file.pin(&live)?)?
                        == *expected,
                    "{} differs from its pinned checkout bytes",
                    delta.path
                );
            } else {
                ensure!(
                    original.is_none(),
                    "an ignored or untracked path collides with {}",
                    delta.path
                );
            }
            validated.push((delta, original));
        }

        // Finish the disk staging before returning a plan for the caller to commit durably.
        // No destination pathname is changed before the plan and start marker exist.
        let metadata_guard = create_vault(&metadata)?;
        let backups_guard = if backups == metadata {
            None
        } else {
            Some(create_vault(&backups)?)
        };
        let metadata_identity = identity(&metadata_guard)?;
        let backups_identity = backups_guard.as_ref().map_or_else(
            || Ok(metadata_identity.clone()),
            |handles| identity(handles),
        )?;
        let mut new_directories = Vec::new();
        for path in missing.into_keys() {
            let location = staged_directory(&backups, new_directories.len());
            fs::create_dir(&location)?;
            new_directories.push(NewDirectory {
                path,
                identity: directory_identity(&location)?,
            });
        }
        let mut files = Vec::new();
        for (delta, before) in validated {
            let after = if delta.after {
                let target = after.path().join(&delta.path);
                let rendered = FileStamp::inspect(&target)?
                    .context("the approved candidate could not be rendered")?;
                if before
                    .as_ref()
                    .is_some_and(|old| old.same_contents(&rendered))
                {
                    continue;
                }
                Some(stage_file(&target, &staged_file(&backups, files.len()))?)
            } else {
                None
            };
            files.push(ChangedFile {
                path: delta.path,
                state: FileState { before, after },
            });
        }
        let new_index = stage_file(&target_index, &metadata.join("index.next"))?;
        git::require_tip(&checkout, previous)?;
        Ok(Self {
            checkout,
            git_dir,
            common: common.into(),
            branch: branch.into(),
            previous: previous.into(),
            integration: integration.into(),
            id,
            checkout_identity,
            git_dir_identity,
            metadata_identity,
            backups_identity,
            parents,
            new_directories,
            files,
            index: FileState {
                before: Some(old_index),
                after: Some(new_index),
            },
        })
    }

    pub(super) fn checkout(&self) -> &Path {
        &self.checkout
    }
    pub(super) fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub(super) fn publication_started(&self) -> Result<bool> {
        let metadata = self.metadata();
        if exists(&metadata.join("started"))? || !exists(&metadata.join("index.next"))? {
            return Ok(true);
        }
        // An interrupted publish may have renamed a file before its directory's marker
        // became durable. A missing staged file or a displaced preimage is also progress.
        let backups = self.backups();
        for position in 0..self.new_directories.len() {
            if !exists(&staged_directory(&backups, position))? {
                return Ok(true);
            }
        }
        for (position, file) in self.files.iter().enumerate() {
            if exists(&file_backup(&backups, position))?
                || (file.state.after.is_some() && !exists(&staged_file(&backups, position))?)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn recovery_error(&self, message: impl Into<String>) -> AttachedRecovery {
        AttachedRecovery {
            checkout: self.checkout.clone(),
            recovery_directory: self.backups(),
            message: message.into(),
        }
    }

    pub(super) fn unstarted_matches_current(&self) -> Result<bool> {
        let attached = self.reserve()?;
        ensure!(
            !self.publication_started()?,
            "the destination update already started"
        );
        Ok(attached.validate_preimages(false).is_ok())
    }

    pub(super) fn reserve(&self) -> Result<AttachedMerge<'_>> {
        let result = (|| {
            let paths = git::checkout_paths(&self.checkout, &self.common, &self.branch)?;
            ensure!(
                Path::new(&paths.git_dir) == self.git_dir,
                "the destination index location changed"
            );
            let mut anchors = pin_directory(&self.checkout, &self.checkout_identity)?;
            anchors.extend(pin_directory(&self.git_dir, &self.git_dir_identity)?);
            anchors.extend(pin_directory(&self.metadata(), &self.metadata_identity)?);
            anchors.extend(pin_directory(&self.backups(), &self.backups_identity)?);
            let guard = lock_index(&self.git_dir)?;
            git::require_idle(&self.git_dir)?;
            for (path, expected) in &self.parents {
                anchors.extend(pin_directory(&self.checkout.join(path), expected)?);
            }
            Ok(AttachedMerge {
                plan: self,
                paths,
                _index_guard: guard,
                _anchors: anchors,
                output_locks: RefCell::default(),
            })
        })();
        if self.publication_started()? {
            result.map_err(|error: anyhow::Error| {
                self.recovery_error(format!(
                    "could not rejoin the destination update: {error:#}"
                ))
                .into()
            })
        } else {
            result
        }
    }

    pub(super) fn cleanup_after_receipt(&self) -> Result<()> {
        let metadata = self.metadata();
        let backups = self.backups();
        if exists(&metadata)? {
            let _guard = pin_directory(&metadata, &self.metadata_identity)?;
            let index_backup = metadata.join("index.before");
            cleanup_file(&index_backup, self.index.before.as_ref())?;
        }
        if exists(&backups)? {
            let _guard = pin_directory(&backups, &self.backups_identity)?;
            for (position, file) in self.files.iter().enumerate() {
                cleanup_file(&file_backup(&backups, position), file.state.before.as_ref())?;
            }
        }
        if backups != metadata && exists(&backups)? {
            fs::remove_dir(&backups)?;
        }
        if exists(&metadata)? {
            let marker = metadata.join("started");
            if exists(&marker)? {
                fs::remove_file(marker)?;
            }
            fs::remove_dir(metadata)?;
        }
        Ok(())
    }

    fn metadata(&self) -> PathBuf {
        metadata_path(&self.git_dir, self.id)
    }

    fn backups(&self) -> PathBuf {
        // Construction and deserialization both carry the same canonical checkout identity.
        if same_volume(&self.checkout_identity, &self.git_dir_identity) {
            self.metadata()
        } else {
            self.checkout
                .with_file_name(format!(".omniagent-merge-{}", self.id))
        }
    }

    fn index_files(&self) -> [PathBuf; 3] {
        let metadata = self.metadata();
        [
            self.git_dir.join("index"),
            metadata.join("index.next"),
            metadata.join("index.before"),
        ]
    }
}

impl AttachedMerge<'_> {
    fn validate_preimages(&self, started: bool) -> Result<()> {
        let plan = self.plan;
        let backups = plan.backups();
        for (position, item) in plan.new_directories.iter().enumerate() {
            git::validate_relative(&plan.checkout, &item.path)?;
            ensure!(
                item.inspect(
                    &plan.checkout.join(&item.path),
                    &staged_directory(&backups, position)
                )? == Progress::Pending
                    || started,
                "a directory changed before the destination update started"
            );
        }
        for (position, file) in plan.files.iter().enumerate() {
            git::validate_relative(&plan.checkout, &file.path)?;
            ensure!(
                file.state.inspect(
                    &plan.checkout.join(&file.path),
                    &staged_file(&backups, position),
                    &file_backup(&backups, position)
                )? == Progress::Pending
                    || started,
                "a file changed before the destination update started"
            );
        }
        ensure!(
            self.index_progress(started)? == Progress::Pending || started,
            "the index changed before the destination update started"
        );
        Ok(())
    }

    pub(super) fn apply(&mut self) -> Result<()> {
        let applied = (|| {
            let plan = self.plan;
            let backups = plan.backups();
            let started = plan.publication_started()?;
            self.validate_preimages(started)?;
            if !started {
                git::require_clean(&self.paths, &plan.checkout, &plan.previous)?;
                let marker = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(plan.metadata().join("started"))?;
                marker.sync_all()?;
            }
            for (position, item) in plan.new_directories.iter().enumerate() {
                let live = plan.checkout.join(&item.path);
                item.install(&live, &staged_directory(&backups, position))?;
                self._anchors.extend(pin_directory(&live, &item.identity)?);
            }
            for (position, file) in plan.files.iter().enumerate() {
                file.state.apply(
                    &plan.checkout.join(&file.path),
                    &staged_file(&backups, position),
                    &file_backup(&backups, position),
                )?;
            }
            if self.index_progress(true)? != Progress::Applied {
                let [index, prepared, displaced] = plan.index_files();
                plan.index.apply(&index, &prepared, &displaced)?;
            }
            Ok(())
        })();
        if self.plan.publication_started()? {
            applied.map_err(|error: anyhow::Error| {
                self.plan
                    .recovery_error(format!("destination update requires recovery: {error:#}"))
                    .into()
            })
        } else {
            applied
        }
    }

    pub(super) fn verify_updated(&self) -> Result<()> {
        let verified = (|| {
            let plan = self.plan;
            let paths = git::checkout_paths(&plan.checkout, &plan.common, &plan.branch)?;
            ensure!(
                paths.git_dir == self.paths.git_dir,
                "the destination Git index moved"
            );
            ensure!(
                plan.publication_started()?,
                "the destination update has no start marker"
            );
            let backups = plan.backups();
            let mut locks = Vec::new();
            for (position, item) in plan.new_directories.iter().enumerate() {
                let live = plan.checkout.join(&item.path);
                ensure!(
                    item.inspect(&live, &staged_directory(&backups, position))?
                        == Progress::Applied,
                    "a destination directory did not match the prepared integration"
                );
                locks.extend(pin_directory(&live, &item.identity)?);
            }
            for (position, file) in plan.files.iter().enumerate() {
                locks.extend(file.state.pin_applied(
                    &plan.checkout.join(&file.path),
                    &staged_file(&backups, position),
                    &file_backup(&backups, position),
                )?);
            }
            locks.push(self.pin_settled_index()?);
            git::require_clean(&self.paths, &plan.checkout, &plan.integration)?;
            self.output_locks.replace(locks);
            Ok(())
        })();
        verified.map_err(|error: anyhow::Error| {
            self.plan
                .recovery_error(format!("could not verify the updated checkout: {error:#}"))
                .into()
        })
    }

    fn index_progress(&self, started: bool) -> Result<Progress> {
        let [index, prepared, displaced] = self.plan.index_files();
        match self.plan.index.inspect(&index, &prepared, &displaced) {
            Ok(progress) => Ok(progress),
            Err(_) if started => self
                .pin_refreshed_index(&index, &prepared, &displaced)
                .map(|_| Progress::Applied),
            Err(error) => Err(error),
        }
    }

    fn pin_settled_index(&self) -> Result<File> {
        let [index, prepared, displaced] = self.plan.index_files();
        match self.plan.index.pin_applied(&index, &prepared, &displaced) {
            Ok(Some(file)) => Ok(file),
            _ => self.pin_refreshed_index(&index, &prepared, &displaced),
        }
    }

    fn pin_refreshed_index(&self, index: &Path, prepared: &Path, displaced: &Path) -> Result<File> {
        // A Git status refresh can rewrite only index stat data after interruption.
        // Accept it only after the journal proves the prepared index was installed.
        ensure!(
            self.plan.index.completed_backup(prepared, displaced)?,
            "the destination index or its protected backups changed outside the merge"
        );
        let file = open_locked_local_file(index)?;
        git::require_exact_index(&self.paths, &self.plan.checkout, &self.plan.integration)?;
        Ok(file)
    }
}

fn collect_parents(
    root: &Path,
    path: &Path,
    known: &mut BTreeMap<String, String>,
    missing: &mut BTreeMap<String, ()>,
) -> Result<()> {
    let mut parents = path
        .ancestors()
        .skip(1)
        .take_while(|path| *path != root)
        .collect::<Vec<_>>();
    parents.reverse();
    for path in parents {
        let name = crate::repository::documents::relative_name(root, path)?;
        if known.contains_key(&name) || missing.contains_key(&name) {
            continue;
        }
        if exists(path)? {
            known.insert(name, directory_identity(path)?);
        } else {
            missing.insert(name, ());
        }
    }
    Ok(())
}

fn same_volume(left: &str, right: &str) -> bool {
    left.split_once(':')
        .zip(right.split_once(':'))
        .is_some_and(|(left, right)| left.0 == right.0)
}

fn metadata_path(git_dir: &Path, id: Uuid) -> PathBuf {
    git_dir.join(format!("omniagent-merge-{id}"))
}

fn backups_path(
    checkout: &Path,
    metadata: &Path,
    checkout_identity: &str,
    git_dir_identity: &str,
    id: Uuid,
) -> Result<PathBuf> {
    if same_volume(checkout_identity, git_dir_identity) {
        Ok(metadata.to_owned())
    } else {
        Ok(checkout
            .parent()
            .context(
                "the destination volume needs space for merge recovery alongside the checkout",
            )?
            .join(format!(".omniagent-merge-{id}")))
    }
}

fn identity(handles: &[File]) -> Result<String> {
    Ok(file_identity(
        handles.last().context("directory handle was unavailable")?,
    )?)
}

fn pin_directory(path: &Path, expected: &str) -> Result<Vec<File>> {
    let handles = lock_directories(path)?;
    ensure!(
        identity(&handles)? == expected,
        "directory identity changed at {}",
        path.display()
    );
    Ok(handles)
}

fn create_vault(path: &Path) -> Result<Vec<File>> {
    let _parent = lock_directories(path.parent().context("recovery directory has no parent")?)?;
    fs::create_dir(path)?;
    lock_directories(path)
}

fn cleanup_file(path: &Path, expected: Option<&FileStamp>) -> Result<()> {
    let current = FileStamp::inspect(path)?;
    if let Some(stamp) = current.as_ref() {
        ensure!(
            Some(stamp) == expected,
            "retained merge data changed at {}",
            path.display()
        );
        // Keep the cleanup reversible when an editor opens the file between verification and deletion.
        let mut handle = crate::repository::documents::open_for_rename(path)?;
        state::delete_exact(&mut handle, stamp)?;
    }
    Ok(())
}

fn staged_directory(directory: &Path, position: usize) -> PathBuf {
    directory.join(format!("directory-{position}"))
}
fn staged_file(directory: &Path, position: usize) -> PathBuf {
    directory.join(format!("file-{position}.next"))
}
fn file_backup(directory: &Path, position: usize) -> PathBuf {
    directory.join(format!("file-{position}.before"))
}
