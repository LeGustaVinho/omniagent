use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

use crate::{providers::configuration::plain_metadata, store::Store};

use super::super::{
    checkout_items::{checkout_directory, mutations::recycle_checked, validate_name},
    documents::{file_identity, open_for_rename, rename, replace_reviewed_checkout_bytes},
};
use super::{
    GitFileSide, GitReviewRevision, LiveReview, ReviewScope, content, install_index_if,
    lock_directories, lock_git_file, stage_existing_file, validate_oid, validate_path,
    with_managed_worktree_writer,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConflictStage {
    mode: String,
    object_id: String,
    file: GitFileSide,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitConflictReview {
    pub path: String,
    pub revision: GitReviewRevision,
    base: Option<ConflictStage>,
    local: Option<ConflictStage>,
    incoming: Option<ConflictStage>,
    deleted_side: Option<ConflictSide>,
    pub working: GitFileSide,
    pub snapshot: Option<String>,
    pub resolution_blocked: Option<String>,
    pub proposal_blocked: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictSide {
    Local,
    Incoming,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum GitConflictProposal {
    Local { object_id: String },
    Incoming { object_id: String },
    Deletion { side: ConflictSide },
    Combined { content: String },
}

impl Store {
    pub async fn git_conflict_review(
        &self,
        checkout: String,
        path: String,
    ) -> Result<GitConflictReview> {
        validate_conflict_path(&path)?;
        let scope = self.git_review_scope(checkout.clone()).await?;
        let mut result = tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let working = content::read_worktree(&scope.root, &path)?;
            let result = read_conflict(&scope, &review, &path, working)?;
            review.verify()?;
            Ok::<_, anyhow::Error>(result)
        })
        .await??;
        let dirty = self
            .execute({
                let path = result.path.clone();
                move |db| protected_draft_exists(db, &checkout, &path)
            })
            .await?;
        if dirty {
            let reason =
                "Save or discard the protected draft for this file before resolving the conflict.";
            result.resolution_blocked = Some(reason.into());
            result.proposal_blocked = Some(reason.into());
        }
        Ok(result)
    }

    pub async fn git_apply_conflict_resolution(
        &self,
        checkout: String,
        path: String,
        expected: String,
        proposal: GitConflictProposal,
    ) -> Result<()> {
        validate_conflict_path(&path)?;
        validate_snapshot(&expected)?;
        let scope = self.git_review_scope(checkout).await?;
        let store = self.clone();
        let runtime = Handle::current();
        with_conflict_writer(self, scope, move |scope| {
            let review = LiveReview::open(&scope)?;
            let _index_lock = lock_review_index(&review)?;
            let working = content::read_worktree(&scope.root, &path)?;
            let observed = read_conflict(&scope, &review, &path, working)?;
            require_snapshot(&observed, &expected)?;
            let edited = proposal_content(&observed, proposal)?;
            runtime.block_on(store.execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                ensure_no_protected_draft(&tx, &scope.id, &path)?;
                let destination = scope.root.join(&path);
                match (observed.working, edited) {
                    (GitFileSide::Text { content }, Some(edited)) => {
                        replace_reviewed_checkout_bytes(
                            &scope.root,
                            &destination,
                            &format!("{:x}", Sha256::digest(content.as_bytes())),
                            edited.as_bytes(),
                            content::MAX_DIFF_SIDE_BYTES,
                            || review.verify(),
                        )
                    }
                    (GitFileSide::Missing, Some(edited)) => {
                        create_working_result(&review, &destination, edited.as_bytes())
                    }
                    (GitFileSide::Text { content }, None) => {
                        recycle_working_result(&scope, &review, &path, &content)
                    }
                    (GitFileSide::Missing, None) => {
                        require_missing_result(&destination)?;
                        review.verify()
                    }
                    (GitFileSide::Unavailable { reason }, _) => anyhow::bail!("{reason}"),
                }
            }))
        })
        .await
    }

    pub async fn git_mark_conflict_resolved(
        &self,
        checkout: String,
        path: String,
        expected: String,
    ) -> Result<()> {
        validate_conflict_path(&path)?;
        validate_snapshot(&expected)?;
        let scope = self.git_review_scope(checkout).await?;
        let store = self.clone();
        let runtime = Handle::current();
        with_conflict_writer(self, scope, move |scope| {
            let prepared = prepare_resolution(scope, path, &expected)?;
            runtime.block_on(store.execute(move |db| prepared.install(db)))
        })
        .await
    }
}

fn validate_conflict_path(path: &str) -> Result<()> {
    validate_path(path)?;
    path.split('/').try_for_each(validate_name)
}

fn recycle_working_result(
    scope: &ReviewScope,
    review: &LiveReview,
    path: &str,
    expected: &str,
) -> Result<()> {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    let parent = checkout_directory(&scope.root, folder)?;
    let _parents = lock_directories(&parent)?;
    let destination = parent.join(name);
    let file = open_for_rename(&destination)?;
    let identity = file_identity(&file)?;
    ensure!(
        matches!(content::read_locked_worktree(&file)?, GitFileSide::Text { content } if content == expected),
        "the working file changed before removal; review the conflict again"
    );
    let recovery = tempfile::Builder::new()
        .prefix(".omniagent-conflict-recovery-")
        .tempdir_in(&parent)?;
    let recovery_guards = lock_directories(recovery.path())?;
    let displaced = recovery.path().join(name);
    review.verify()?;
    rename(&file, &displaced).context("move the reviewed file into recovery")?;
    let recovery = recovery.keep();
    drop(file);

    let recycled = recycle_checked(&displaced, || {
        review.verify()?;
        let recovery_file = open_for_rename(&displaced)?;
        ensure!(
            file_identity(&recovery_file)? == identity
                && matches!(content::read_locked_worktree(&recovery_file)?, GitFileSide::Text { content } if content == expected),
            "the recovery file changed"
        );
        Ok(())
    });
    if let Err(error) = recycled {
        let restored = (|| -> Result<()> {
            let file = open_for_rename(&displaced)?;
            ensure!(
                file_identity(&file)? == identity,
                "the recovery file changed"
            );
            rename(&file, &destination)?;
            Ok(())
        })();
        drop(recovery_guards);
        let _ = fs::remove_dir(&recovery);
        if restored.is_ok() {
            return Err(
                error.context("The Recycle Bin operation failed and the working file was restored")
            );
        }
        anyhow::bail!(
            "Check the recovery path {} or the Recycle Bin. The reviewed file could not be restored automatically: {error:#}",
            displaced.display()
        );
    }
    drop(recovery_guards);
    let _ = fs::remove_dir(&recovery);
    require_missing_result(&destination).context("The reviewed file was moved to the Recycle Bin")
}

fn require_missing_result(path: &Path) -> Result<()> {
    ensure!(
        plain_metadata(path)?.is_none(),
        "a working file appeared; review the conflict again"
    );
    Ok(())
}

fn create_working_result(review: &LiveReview, destination: &Path, edited: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .context("the file has no parent directory")?;
    let _directories = lock_directories(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(edited)?;
    file.as_file().sync_all()?;
    review.verify()?;
    file.persist_noclobber(destination).context(
        "the working result appeared or could not be created; review the conflict again",
    )?;
    Ok(())
}

async fn with_conflict_writer(
    store: &Store,
    scope: ReviewScope,
    write: impl FnOnce(ReviewScope) -> Result<()> + Send + 'static,
) -> Result<()> {
    let (project, checkout, owner) = (scope.project.clone(), scope.id.clone(), scope.owner.clone());
    match owner {
        Some(owner) => {
            with_managed_worktree_writer(store, project, checkout, owner, move |_| write(scope))
                .await
        }
        None => tokio::task::spawn_blocking(move || write(scope)).await?,
    }
}

fn proposal_content(
    observed: &GitConflictReview,
    proposal: GitConflictProposal,
) -> Result<Option<String>> {
    match proposal {
        GitConflictProposal::Local { object_id } => {
            chosen_side(&observed.local, &object_id).map(Some)
        }
        GitConflictProposal::Incoming { object_id } => {
            chosen_side(&observed.incoming, &object_id).map(Some)
        }
        GitConflictProposal::Deletion { side } => {
            ensure!(
                observed.deleted_side == Some(side),
                "the selected conflict side has no recorded deletion"
            );
            Ok(None)
        }
        GitConflictProposal::Combined { content } => {
            ensure!(
                content.len() as u64 <= content::MAX_DIFF_SIDE_BYTES
                    && !content.contains(['\r', '\0']),
                "the combined result must use UTF-8 text with LF line endings and fit in the text preview limit"
            );
            Ok(Some(content))
        }
    }
}

fn chosen_side(stage: &Option<ConflictStage>, expected_object_id: &str) -> Result<String> {
    let stage = stage
        .as_ref()
        .context("the selected conflict version is absent")?;
    ensure!(
        stage.object_id == expected_object_id,
        "the selected conflict version changed; review it again"
    );
    match &stage.file {
        GitFileSide::Text { content } => Ok(content.clone()),
        _ => anyhow::bail!("the selected conflict version is not an available text file"),
    }
}

fn validate_snapshot(expected: &str) -> Result<()> {
    ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "review the conflict again before applying a resolution"
    );
    Ok(())
}

fn require_snapshot(observed: &GitConflictReview, expected: &str) -> Result<()> {
    ensure!(
        observed.snapshot.as_deref() == Some(expected),
        "the index or working result changed; review the current conflict again"
    );
    Ok(())
}

fn read_conflict(
    scope: &ReviewScope,
    review: &LiveReview,
    path: &str,
    working: GitFileSide,
) -> Result<GitConflictReview> {
    let [base, local, incoming] = read_stages(review, path)?;
    let regular = |stage: &ConflictStage| matches!(stage.mode.as_str(), "100644" | "100755");
    let deleted_side = match (&base, &local, &incoming) {
        (Some(base), None, Some(incoming)) if regular(base) && regular(incoming) => {
            Some(ConflictSide::Local)
        }
        (Some(base), Some(local), None) if regular(base) && regular(local) => {
            Some(ConflictSide::Incoming)
        }
        _ => None,
    };
    let (fingerprint, resolution_blocked, proposal_blocked) = match &working {
        GitFileSide::Text { content } => {
            let fingerprint = format!("{:x}", Sha256::digest(content.as_bytes()));
            let blocked = has_conflict_markers(review, path, content)?.then(|| {
                "Remove the remaining conflict markers from the working result before staging."
                    .into()
            });
            (Some(fingerprint), blocked, None)
        }
        GitFileSide::Missing => (
            Some("missing".into()),
            deleted_side
                .is_none()
                .then(|| "Create the working result and review it before staging.".into()),
            None,
        ),
        GitFileSide::Unavailable { reason } => (None, Some(reason.clone()), Some(reason.clone())),
    };
    let snapshot = fingerprint
        .map(|fingerprint| {
            let metadata = |stage: &Option<ConflictStage>| {
                stage
                    .as_ref()
                    .map(|stage| (stage.mode.clone(), stage.object_id.clone()))
            };
            let identity = serde_json::to_vec(&(
                "git-conflict-resolution-v1",
                &scope.id,
                path,
                &review.revision,
                [metadata(&base), metadata(&local), metadata(&incoming)],
                fingerprint,
            ))?;
            Ok::<_, anyhow::Error>(format!("{:x}", Sha256::digest(identity)))
        })
        .transpose()?;
    Ok(GitConflictReview {
        path: path.into(),
        revision: review.revision.clone(),
        base,
        local,
        incoming,
        deleted_side,
        working,
        snapshot,
        resolution_blocked,
        proposal_blocked,
    })
}

fn read_stages(review: &LiveReview, path: &str) -> Result<[Option<ConflictStage>; 3]> {
    let output = review.view.read(&[
        "--literal-pathspecs",
        "ls-files",
        "--unmerged",
        "-z",
        "--",
        path,
    ])?;
    let mut stages = [None, None, None];
    for row in String::from_utf8(output)?.split_terminator('\0') {
        let (metadata, candidate) = row
            .split_once('\t')
            .context("Git returned an invalid conflict stage")?;
        if candidate != path {
            continue;
        }
        let fields: Vec<_> = metadata.split_ascii_whitespace().collect();
        let [mode, object_id, stage] = fields[..] else {
            anyhow::bail!("Git returned invalid conflict metadata");
        };
        validate_oid(object_id)?;
        let index = match stage {
            "1" => 0,
            "2" => 1,
            "3" => 2,
            _ => anyhow::bail!("Git returned an unexpected conflict stage"),
        };
        ensure!(
            stages[index].is_none(),
            "Git returned a duplicate conflict stage"
        );
        stages[index] = Some(ConflictStage {
            mode: mode.into(),
            object_id: object_id.into(),
            file: content::read_blob(&review.view, mode, object_id)?,
        });
    }
    ensure!(
        stages.iter().any(Option::is_some),
        "the selected path has no unresolved stages"
    );
    Ok(stages)
}

fn has_conflict_markers(review: &LiveReview, path: &str, text: &str) -> Result<bool> {
    let attribute = review.view.read(&[
        "--literal-pathspecs",
        "check-attr",
        "-z",
        "conflict-marker-size",
        "--",
        path,
    ])?;
    let width = String::from_utf8(attribute)?
        .split_terminator('\0')
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width > 0)
        .unwrap_or(7);
    Ok(text.split(['\r', '\n']).any(|line| {
        let bytes = line.as_bytes();
        let Some(marker @ (b'<' | b'>' | b'|')) = bytes.first() else {
            return false;
        };
        let count = bytes.iter().take_while(|byte| *byte == marker).count();
        (count == width || count >= 7)
            && bytes
                .get(count)
                .is_none_or(|byte| byte.is_ascii_whitespace())
    }))
}

struct PreparedResolution {
    scope: ReviewScope,
    path: String,
    review: LiveReview,
    index: LockedReviewIndex,
    working: Option<(Vec<File>, File)>,
}

struct LockedReviewIndex {
    path: PathBuf,
    _directories: Vec<File>,
    _lock: File,
}

fn lock_review_index(review: &LiveReview) -> Result<LockedReviewIndex> {
    let git_dir = Path::new(&review.paths.git_dir);
    let _directories = lock_directories(git_dir)?;
    let _lock = lock_git_file(&git_dir.join("index.lock")).context("lock the worktree index")?;
    review.verify()?;
    Ok(LockedReviewIndex {
        path: git_dir.join("index"),
        _directories,
        _lock,
    })
}

fn prepare_resolution(
    scope: ReviewScope,
    path: String,
    expected: &str,
) -> Result<PreparedResolution> {
    let review = LiveReview::open(&scope)?;
    let index = lock_review_index(&review)?;
    let working = content::lock_staging_file(&scope.root, &path)?;
    let reviewed_file = match &working {
        Some((_, file)) => content::read_locked_worktree(file)?,
        None => GitFileSide::Missing,
    };
    let observed = read_conflict(&scope, &review, &path, reviewed_file)?;
    require_snapshot(&observed, expected)?;
    if let Some(reason) = observed.resolution_blocked {
        anyhow::bail!("{reason}");
    }
    Ok(PreparedResolution {
        scope,
        path,
        review,
        index,
        working,
    })
}

impl PreparedResolution {
    fn install(self, db: &mut Connection) -> Result<()> {
        // Hold the index and working file handles while the database checks for a new
        // protected draft. The transaction keeps another Store from racing that check.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_no_protected_draft(&tx, &self.scope.id, &self.path)?;
        let listing = if self.working.is_some() {
            stage_existing_file(&self.review, &self.path)?;
            "--unmerged"
        } else {
            require_missing_result(&self.scope.root.join(&self.path))?;
            self.review.view.read(&[
                "-c",
                "core.splitIndex=false",
                "update-index",
                "--force-remove",
                "--",
                &self.path,
            ])?;
            "--stage"
        };
        ensure!(
            self.review
                .view
                .read(&[
                    "--literal-pathspecs",
                    "ls-files",
                    listing,
                    "-z",
                    "--",
                    &self.path
                ])?
                .is_empty(),
            "Git could not stage the file as a resolution"
        );
        install_index_if(&self.review, &self.index.path, || {
            if self.working.is_none() {
                require_missing_result(&self.scope.root.join(&self.path))?;
            }
            Ok(())
        })
    }
}

fn ensure_no_protected_draft(db: &Connection, checkout: &str, path: &str) -> Result<()> {
    ensure!(
        !protected_draft_exists(db, checkout, path)?,
        "save or discard the protected draft for this file before resolving the conflict"
    );
    Ok(())
}

fn protected_draft_exists(db: &Connection, checkout: &str, path: &str) -> Result<bool> {
    let mut query =
        db.prepare("SELECT document FROM drafts WHERE kind = 'checkout_file' AND owner_id = ?1")?;
    let rows = query.query_map(params![checkout], |row| row.get::<_, String>(0))?;
    let compared = path.to_lowercase();
    for draft in rows {
        if draft?.to_lowercase() == compared {
            return Ok(true);
        }
    }
    Ok(false)
}
