use std::{io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{protection, providers::configuration::open_locked_local_file, store::Store};

use super::{
    GitStartingState, ManagedWorktree, WorktreeStart, base_commit,
    isolated_git::IsolatedGit,
    worktrees::{load_worktree, verify_managed},
    writers::{WriterSource, retain_pending_changes},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntrackedFile {
    pub path: String,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitChanges {
    pub staged_patch_base64: String,
    pub unstaged_patch_base64: String,
    pub untracked: Vec<UntrackedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateRevision {
    pub revision: String,
    pub start: WorktreeStart,
    pub candidate: GitStartingState,
    pub committed_patch_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AttemptWorktreeEvidence {
    Saved { artifact: Box<CandidateRevision> },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveWorktree {
    pub worktree: ManagedWorktree,
    pub head: String,
    pub status: Vec<String>,
}

impl Store {
    pub async fn attempt_worktree_evidence(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<AttemptWorktreeEvidence>> {
        self.execute(move |db| {
            let evidence: Option<Option<Vec<u8>>> = db
                .query_row(
                    "SELECT c.evidence FROM job_attempts a JOIN jobs j ON j.id = a.job_id
                     LEFT JOIN job_worktree_candidates c ON c.run_id = a.run_id
                     WHERE j.project_id = ?1 AND a.run_id = ?2",
                    params![project, run],
                    |row| row.get(0),
                )
                .optional()?;
            evidence
                .map(|evidence| match evidence {
                    Some(payload) => Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?),
                    None => Ok(AttemptWorktreeEvidence::Unavailable {
                        reason: "No candidate was saved for this attempt.".into(),
                    }),
                })
                .transpose()
        })
        .await
    }

    pub async fn live_managed_worktree(
        &self,
        project: String,
        owner_task: String,
    ) -> Result<Option<LiveWorktree>> {
        let identity = self
            .execute(move |db| load_worktree(db, &project, &owner_task))
            .await?;
        let Some((worktree, git_dir, common)) = identity else {
            return Ok(None);
        };
        tokio::task::spawn_blocking(move || {
            let paths = verify_managed(&worktree, &git_dir, &common)?;
            let head = base_commit(&worktree.root)?.context("the managed branch has no commit")?;
            let index = Path::new(&git_dir).join("index");
            let index_digest = hash_file(&index)?;
            let view = IsolatedGit::open(&paths, &worktree.root, &head)?;
            view.copy_index(&paths)?;
            let status = read_status(&view)?;
            check_capture_identity(&worktree, &git_dir, &common, &head, &index, &index_digest)?;
            Ok(Some(LiveWorktree {
                worktree,
                head,
                status,
            }))
        })
        .await?
    }
}

pub(crate) async fn save_worktree_candidate(store: &Store, run_id: String) -> Result<()> {
    capture_worktree_candidate(store, run_id, false).await
}

pub(crate) async fn save_reconciled_native_candidate(store: &Store, run_id: String) -> Result<()> {
    capture_worktree_candidate(store, run_id, true).await
}

async fn capture_worktree_candidate(store: &Store, run_id: String, reconcile: bool) -> Result<()> {
    let identity = store
        .execute({
            let run = run_id.clone();
            move |db| {
                db.query_row(
                    "SELECT s.state, c.git_dir, p.git_common_dir
                     FROM job_worktree_starts s JOIN job_attempts a ON a.run_id = s.run_id
                     JOIN jobs j ON j.id = a.job_id JOIN checkouts c ON c.id = s.checkout_id
                     JOIN projects p ON p.id = j.project_id
                     JOIN job_writer_reservations w ON w.run_id = a.run_id AND w.checkout_id = c.id
                     WHERE s.run_id = ?1 AND j.mode = 'approved_write' AND j.checkout_id = c.id
                       AND (NOT ?2 OR EXISTS (
                            SELECT 1 FROM native_goal_executors x WHERE x.run_id = s.run_id AND x.stop_confirmed
                       ))",
                    params![run, reconcile],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(Into::into)
            }
        })
        .await?;
    let (start, git_dir, common) =
        identity.context("the attempt lost its starting state or worktree reservation")?;
    let evidence = tokio::task::spawn_blocking(move || {
        let captured = || -> Result<CandidateRevision> {
            let start: WorktreeStart = serde_json::from_slice(&protection::unprotect(&start)?)?;
            ensure!(
                start.git_common_dir == Path::new(&common),
                "the registered repository changed since launch"
            );
            let captured = capture_git(
                &start.worktree,
                &git_dir,
                &common,
                Some(&start.starting.head),
            )?;
            let revision = digest(&serde_json::to_vec(&captured)?);
            Ok(CandidateRevision {
                revision,
                start,
                candidate: captured.state,
                committed_patch_base64: captured.committed_patch_base64,
            })
        };
        match captured() {
            Ok(artifact) => AttemptWorktreeEvidence::Saved {
                artifact: Box::new(artifact),
            },
            Err(error) => AttemptWorktreeEvidence::Unavailable {
                reason: crate::diagnostics::preview(&format!("{error:#}")),
            },
        }
    })
    .await?;
    let confirmed = matches!(evidence, AttemptWorktreeEvidence::Saved { .. });
    let pending = match &evidence {
        AttemptWorktreeEvidence::Saved { artifact } => artifact
            .candidate
            .holds_scope_since(&artifact.start.starting),
        AttemptWorktreeEvidence::Unavailable { .. } => false,
    };
    let payload = protection::protect(&serde_json::to_vec(&evidence)?)?;
    store
        .execute(move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let recorded = tx.execute(
                "INSERT INTO job_worktree_candidates (run_id, evidence, capture_confirmed)
                 SELECT ?1, ?2, ?3 FROM job_writer_reservations WHERE run_id = ?1
                 ON CONFLICT(run_id) DO UPDATE SET evidence = excluded.evidence, capture_confirmed = 1
                 WHERE ?4 AND excluded.capture_confirmed AND NOT job_worktree_candidates.capture_confirmed
                   AND NOT EXISTS (SELECT 1 FROM history_deleted_attempt_diffs WHERE run_id = ?1)",
                params![run_id, payload, confirmed, reconcile],
            )?;
            ensure!(
                recorded == 1
                    || tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM job_worktree_candidates WHERE run_id = ?1)",
                        [&run_id],
                        |row| row.get::<_, bool>(0),
                    )?,
                "the worktree writer reservation ended before candidate capture"
            );
            if recorded == 1 && pending {
                let (project, checkout, scope) = tx.query_row(
                    "SELECT j.project_id, w.checkout_id, coalesce(s.scope_task_id, j.task_id)
                     FROM job_attempts a JOIN jobs j ON j.id = a.job_id
                     JOIN job_writer_reservations w ON w.run_id = a.run_id
                     LEFT JOIN job_worktree_scopes s ON s.job_id = j.id WHERE a.run_id = ?1",
                    [&run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )?;
                retain_pending_changes(
                    &tx,
                    &project,
                    &checkout,
                    &scope,
                    WriterSource::Run(&run_id),
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

#[derive(PartialEq, Eq, Serialize)]
pub(super) struct GitCapture {
    pub(super) state: GitStartingState,
    committed_patch_base64: String,
}

pub(super) fn capture_git(
    worktree: &ManagedWorktree,
    git_dir: &str,
    common: &str,
    starting_head: Option<&str>,
) -> Result<GitCapture> {
    let first = read_git(worktree, git_dir, common, starting_head)?;
    let second = read_git(worktree, git_dir, common, starting_head)?;
    ensure!(
        first == second,
        "the worktree changed while capturing Git evidence"
    );
    Ok(first)
}

#[derive(Debug)]
pub(crate) enum WorktreeCheckpointMismatch {
    Identity,
    Content,
}

impl std::fmt::Display for WorktreeCheckpointMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Identity => "The worktree identity changed since the checkpoint was saved",
            Self::Content => "The worktree content changed since the checkpoint was saved",
        })
    }
}

impl std::error::Error for WorktreeCheckpointMismatch {}

pub(super) fn recheck_candidate(
    worktree: &ManagedWorktree,
    git_dir: &str,
    common: &str,
    payload: &[u8],
) -> Result<GitStartingState> {
    let AttemptWorktreeEvidence::Saved { artifact } =
        serde_json::from_slice(&protection::unprotect(payload)?)?
    else {
        anyhow::bail!("The saved checkpoint has no worktree candidate");
    };
    ensure!(
        artifact.start.worktree == *worktree && artifact.start.git_common_dir == Path::new(common),
        WorktreeCheckpointMismatch::Identity
    );
    let current = capture_git(
        worktree,
        git_dir,
        common,
        Some(&artifact.start.starting.head),
    )?;
    ensure!(
        digest(&serde_json::to_vec(&current)?) == artifact.revision,
        WorktreeCheckpointMismatch::Content
    );
    Ok(current.state)
}

fn read_git(
    worktree: &ManagedWorktree,
    git_dir: &str,
    common: &str,
    starting_head: Option<&str>,
) -> Result<GitCapture> {
    let paths = verify_managed(worktree, git_dir, common)?;
    let head = base_commit(&worktree.root)?.context("the managed branch has no commit")?;
    let index = Path::new(git_dir).join("index");
    let index_digest = hash_file(&index)?;
    let view = IsolatedGit::open(&paths, &worktree.root, &head)?;
    view.copy_index(&paths)?;
    let status = read_status(&view)?;
    let staged = patch(&view, &["--cached"])?;
    let unstaged = patch(&view, &[])?;
    let committed = match starting_head {
        Some(base) if base != head => patch(&view, &[base, &head])?,
        _ => Vec::new(),
    };
    let untracked = untracked_files(&view, &worktree.root, starting_head.is_some())?;
    check_capture_identity(worktree, git_dir, common, &head, &index, &index_digest)?;
    Ok(GitCapture {
        state: GitStartingState {
            head,
            status,
            index_digest,
            staged_diff_digest: digest(&staged),
            unstaged_diff_digest: digest(&unstaged),
            changes: Some(GitChanges {
                staged_patch_base64: STANDARD.encode(staged),
                unstaged_patch_base64: STANDARD.encode(unstaged),
                untracked,
            }),
        },
        committed_patch_base64: STANDARD.encode(committed),
    })
}

fn check_capture_identity(
    worktree: &ManagedWorktree,
    git_dir: &str,
    common: &str,
    head: &str,
    index: &Path,
    index_digest: &str,
) -> Result<()> {
    verify_managed(worktree, git_dir, common)?;
    ensure!(
        base_commit(&worktree.root)?.as_deref() == Some(head) && hash_file(index)? == index_digest,
        "the worktree HEAD or index changed while capturing Git evidence"
    );
    Ok(())
}

pub(super) fn read_status(view: &IsolatedGit) -> Result<Vec<String>> {
    let raw = view.read(&[
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=dirty",
    ])?;
    Ok(String::from_utf8(raw)?
        .split_terminator('\0')
        .map(str::to_owned)
        .collect())
}

fn patch(view: &IsolatedGit, comparison: &[&str]) -> Result<Vec<u8>> {
    let mut args = vec![
        "diff",
        "--binary",
        "--full-index",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
    ];
    args.extend_from_slice(comparison);
    args.push("--");
    view.read(&args)
}

fn untracked_files(view: &IsolatedGit, root: &Path, contents: bool) -> Result<Vec<UntrackedFile>> {
    let names =
        String::from_utf8(view.read(&["ls-files", "--others", "--exclude-standard", "-z"])?)?;
    names
        .split_terminator('\0')
        .map(|name| {
            let path = root.join(name);
            ensure!(
                !name.contains(['\\', ':'])
                    && name.split('/').all(|part| {
                        !part.is_empty()
                            && part != "."
                            && part != ".."
                            && !part.eq_ignore_ascii_case(".git")
                    }),
                "an untracked entry is not a local file inside the checkout"
            );
            let (sha256, content_base64) = if contents {
                let mut bytes = Vec::new();
                open_locked_local_file(&path)?.read_to_end(&mut bytes)?;
                (digest(&bytes), Some(STANDARD.encode(bytes)))
            } else {
                (hash_file(&path)?, None)
            };
            Ok(UntrackedFile {
                path: name.into(),
                sha256,
                content_base64,
            })
        })
        .collect()
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    std::io::copy(&mut open_locked_local_file(path)?, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
