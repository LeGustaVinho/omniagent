use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

use crate::store::Store;

use super::{
    GitFileSide, GitReviewRevision, LiveReview, ReviewScope, content, install_index,
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
    pub working: GitFileSide,
    pub snapshot: Option<String>,
    pub resolution_blocked: Option<String>,
}

impl Store {
    pub async fn git_conflict_review(
        &self,
        checkout: String,
        path: String,
    ) -> Result<GitConflictReview> {
        validate_path(&path)?;
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
            result.resolution_blocked = Some(
                "Save or discard the protected draft for this file before staging a resolution."
                    .into(),
            );
        }
        Ok(result)
    }

    pub async fn git_mark_conflict_resolved(
        &self,
        checkout: String,
        path: String,
        expected: String,
    ) -> Result<()> {
        validate_path(&path)?;
        ensure!(
            expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "review this conflict again before staging"
        );
        let scope = self.git_review_scope(checkout.clone()).await?;
        let (project, owner) = (scope.project.clone(), scope.owner.clone());
        let store = self.clone();
        let runtime = Handle::current();
        let write = move || {
            let prepared = prepare_resolution(scope, path, &expected)?;
            runtime.block_on(store.execute(move |db| prepared.install(db)))
        };
        match owner {
            Some(owner) => {
                with_managed_worktree_writer(self, project, checkout, owner, move |_| write()).await
            }
            None => tokio::task::spawn_blocking(write).await?,
        }
    }
}

fn read_conflict(
    scope: &ReviewScope,
    review: &LiveReview,
    path: &str,
    working: GitFileSide,
) -> Result<GitConflictReview> {
    let [base, local, incoming] = read_stages(review, path)?;
    let (snapshot, resolution_blocked) = match &working {
        GitFileSide::Text { content } => {
            let fingerprint = format!("{:x}", Sha256::digest(content.as_bytes()));
            let metadata = |stage: &Option<ConflictStage>| stage.as_ref().map(|stage| (stage.mode.clone(), stage.object_id.clone()));
            let identity = serde_json::to_vec(&(
                "git-conflict-resolution-v1", &scope.id, path, &review.revision,
                [metadata(&base), metadata(&local), metadata(&incoming)], fingerprint,
            ))?;
            let blocked = has_conflict_markers(review, path, content)?
                .then(|| "Remove the remaining conflict markers from the working result before staging.".into());
            (Some(format!("{:x}", Sha256::digest(identity))), blocked)
        }
        GitFileSide::Missing => (None, Some("The working result is missing. Restore or create the intended file before staging it here.".into())),
        GitFileSide::Unavailable { reason } => (None, Some(reason.clone())),
    };
    Ok(GitConflictReview {
        path: path.into(),
        revision: review.revision.clone(),
        base,
        local,
        incoming,
        working,
        snapshot,
        resolution_blocked,
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
    index: PathBuf,
    _parents: Vec<File>,
    _file: File,
    _git_dirs: Vec<File>,
    _index_lock: File,
}

fn prepare_resolution(
    scope: ReviewScope,
    path: String,
    expected: &str,
) -> Result<PreparedResolution> {
    let review = LiveReview::open(&scope)?;
    let git_dir = Path::new(&review.paths.git_dir);
    let _git_dirs = lock_directories(git_dir)?;
    let index = git_dir.join("index");
    let _index_lock =
        lock_git_file(&git_dir.join("index.lock")).context("lock the worktree index")?;
    review.verify()?;
    let (_parents, _file) = content::lock_staging_file(&scope.root, &path)?
        .context("the working result is missing; review the conflict again")?;
    let observed = read_conflict(
        &scope,
        &review,
        &path,
        content::read_locked_worktree(&_file)?,
    )?;
    ensure!(
        observed.snapshot.as_deref() == Some(expected),
        "the index or working result changed; review the current conflict again"
    );
    if let Some(reason) = observed.resolution_blocked {
        anyhow::bail!("{reason}");
    }
    Ok(PreparedResolution {
        scope,
        path,
        review,
        index,
        _parents,
        _file,
        _git_dirs,
        _index_lock,
    })
}

impl PreparedResolution {
    fn install(self, db: &mut Connection) -> Result<()> {
        // Hold the index and working file handles while the database checks for a new
        // protected draft. The transaction keeps another Store from racing that check.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            !protected_draft_exists(&tx, &self.scope.id, &self.path)?,
            "save or discard the protected draft for this file before staging a resolution"
        );
        stage_existing_file(&self.review, &self.path)?;
        ensure!(
            self.review
                .view
                .read(&[
                    "--literal-pathspecs",
                    "ls-files",
                    "--unmerged",
                    "-z",
                    "--",
                    &self.path
                ])?
                .is_empty(),
            "Git could not stage the file as a resolution"
        );
        install_index(&self.review, &self.index)
    }
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
