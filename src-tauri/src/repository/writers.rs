use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::{protection, store::Store};

use super::{
    AttemptWorktreeEvidence, ManagedWorktree, candidates::capture_git, worktrees::load_worktree,
};

pub(crate) enum WriterConflict {
    Run { id: String, needs_review: bool },
    App(String),
    Pending { scope: String, run: Option<String> },
}

#[derive(Clone, Copy)]
pub(super) enum WriterSource<'a> {
    Run(&'a str),
    App(&'a str),
}

pub(crate) fn writer_owner(
    db: &Connection,
    project: &str,
    scope: &str,
    checkout: &str,
) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT m.task_id FROM managed_task_worktrees m
         WHERE m.project_id = ?1 AND m.checkout_id = ?3 AND (m.task_id = ?2 OR EXISTS (
             SELECT 1 FROM shared_worktree_scopes s WHERE s.project_id = m.project_id
             AND s.scope_task_id = ?2 AND s.owner_task_id = m.task_id AND s.checkout_id = m.checkout_id
         ))",
        params![project, scope, checkout],
        |row| row.get(0),
    ).optional()?)
}

// Admission and reservation use this check inside the same transaction.
pub(crate) fn writer_conflict(
    db: &Connection,
    checkout: &str,
    scope: &str,
) -> Result<Option<WriterConflict>> {
    if let Some(run) = db
        .query_row(
            "SELECT w.run_id, NOT EXISTS (
                 SELECT 1 FROM active_agent_attempts a WHERE a.run_id = w.run_id
             ) FROM job_writer_reservations w WHERE w.checkout_id = ?1",
            [checkout],
            |row| {
                Ok(WriterConflict::Run {
                    id: row.get(0)?,
                    needs_review: row.get(1)?,
                })
            },
        )
        .optional()?
    {
        return Ok(Some(run));
    }
    if let Some(operation) = db
        .query_row(
            "SELECT operation_id FROM app_worktree_writers WHERE checkout_id = ?1",
            [checkout],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(Some(WriterConflict::App(operation)));
    }
    Ok(db
        .query_row(
            "SELECT scope_task_id, producing_run_id FROM pending_worktree_changes
         WHERE checkout_id = ?1 AND scope_task_id <> ?2",
            params![checkout, scope],
            |row| {
                Ok(WriterConflict::Pending {
                    scope: row.get(0)?,
                    run: row.get(1)?,
                })
            },
        )
        .optional()?)
}

pub(super) fn retain_pending_changes(
    db: &Connection,
    project: &str,
    checkout: &str,
    scope: &str,
    source: WriterSource<'_>,
) -> Result<()> {
    let (run, operation) = match source {
        WriterSource::Run(run) => (Some(run), None),
        WriterSource::App(operation) => (None, Some(operation)),
    };
    let changed = db.execute(
        "INSERT INTO pending_worktree_changes VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(checkout_id) DO UPDATE SET scope_task_id = excluded.scope_task_id
         WHERE pending_worktree_changes.scope_task_id = excluded.scope_task_id",
        params![checkout, project, scope, run, operation],
    )?;
    ensure!(
        changed == 1,
        "pending changes belong to a different work scope"
    );
    Ok(())
}

impl Store {
    /// Assign a scope to a worktree already provisioned for its owner.
    pub async fn assign_shared_worktree(
        &self,
        project: String,
        scope: String,
        checkout: String,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let owner: String = tx.query_row(
                "SELECT task_id FROM managed_task_worktrees WHERE project_id = ?1 AND checkout_id = ?2",
                params![project, checkout], |row| row.get(0),
            ).optional()?.context("shared worktree is outside the project")?;
            let has_own: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM worktree_provisions WHERE project_id = ?1 AND task_id = ?2)
                     OR EXISTS (SELECT 1 FROM managed_task_worktrees WHERE project_id = ?1 AND task_id = ?2)",
                params![project, scope], |row| row.get(0),
            )?;
            ensure!(!has_own, "the scope already owns or is provisioning a worktree");
            let inserted = tx.execute(
                "INSERT INTO shared_worktree_scopes VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id, scope_task_id) DO NOTHING",
                params![project, scope, owner, checkout],
            )?;
            ensure!(inserted == 1 || writer_owner(&tx, &project, &scope, &checkout)?.is_some(),
                "the scope already uses a different shared worktree");
            tx.commit()?;
            Ok(())
        }).await
    }
}

/// Run an app-owned edit against a managed checkout under the scheduler's writer admission.
/// Filesystem operations run to settlement even if the caller closes its UI.
pub async fn with_managed_worktree_writer<T: Send + 'static>(
    store: &Store,
    project: String,
    checkout: String,
    scope: String,
    write: impl FnOnce(&ManagedWorktree) -> Result<T> + Send + 'static,
) -> Result<T> {
    let store = store.clone();
    tokio::spawn(async move {
        let operation = Uuid::new_v4().to_string();
        let identity = store
            .execute({
                let (project, scope, checkout, operation) = (
                    project.clone(),
                    scope.clone(),
                    checkout.clone(),
                    operation.clone(),
                );
                move |db| {
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let owner = writer_owner(&tx, &project, &scope, &checkout)?
                        .context("this work scope is not assigned to the checkout")?;
                    let identity = load_worktree(&tx, &project, &owner)?
                        .context("managed worktree is unavailable")?;
                    match writer_conflict(&tx, &checkout, &scope)? {
                        Some(WriterConflict::Run { id, .. }) => {
                            anyhow::bail!("worktree reserved by run {id}")
                        }
                        Some(WriterConflict::App(id)) => {
                            anyhow::bail!("worktree reserved by app operation {id}")
                        }
                        Some(WriterConflict::Pending { scope, .. }) => {
                            anyhow::bail!("worktree has pending changes for scope {scope}")
                        }
                        None => {}
                    }
                    tx.execute(
                        "INSERT INTO app_worktree_writers VALUES (?1, ?2, ?3, ?4)",
                        params![checkout, operation, project, scope],
                    )?;
                    tx.commit()?;
                    Ok(identity)
                }
            })
            .await?;
        let (worktree, git_dir, common) = identity;
        let observation = tokio::task::spawn_blocking(move || {
            let before = match capture_git(&worktree, &git_dir, &common, None) {
                Ok(before) => before,
                Err(error) => return Ok((Err(error), false)),
            };
            let result = write(&worktree);
            let after = capture_git(&worktree, &git_dir, &common, None)?;
            Ok::<_, anyhow::Error>((result, after.state.holds_scope_since(&before.state)))
        })
        .await??;
        let (result, pending) = observation;
        store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let removed = tx.execute(
                    "DELETE FROM app_worktree_writers WHERE checkout_id = ?1 AND operation_id = ?2",
                    params![checkout, operation],
                )?;
                ensure!(
                    removed == 1,
                    "the app worktree reservation ended before the write settled"
                );
                if pending {
                    retain_pending_changes(
                        &tx,
                        &project,
                        &checkout,
                        &scope,
                        WriterSource::App(&operation),
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        store.job_signal.send_replace(());
        result
    })
    .await?
}

pub(crate) fn migrate_writer_evidence(db: &Connection) -> Result<()> {
    let mut query = db.prepare(
        "WITH latest AS (
             SELECT a.run_id, j.project_id, j.checkout_id, j.task_id, e.evidence,
                    row_number() OVER (PARTITION BY j.checkout_id ORDER BY (
                        SELECT min(events.seq) FROM turns JOIN run_events events ON events.turn_id = turns.id
                        WHERE turns.run_id = a.run_id
                    ) DESC) AS position
             FROM job_attempts a JOIN jobs j ON j.id = a.job_id
             LEFT JOIN job_worktree_candidates e ON e.run_id = a.run_id
             WHERE j.writer_checkout_id IS NOT NULL AND a.process_launch_requested = 1
         ) SELECT run_id, project_id, checkout_id, task_id, evidence FROM latest WHERE position = 1",
    )?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let run: String = row.get(0)?;
        let evidence: Option<Vec<u8>> = row.get(4)?;
        let saved = evidence
            .and_then(|payload| protection::unprotect(&payload).ok())
            .and_then(|payload| serde_json::from_slice::<AttemptWorktreeEvidence>(&payload).ok());
        let pending = match saved {
            Some(AttemptWorktreeEvidence::Saved { artifact }) => {
                db.execute(
                    "UPDATE job_worktree_candidates SET capture_confirmed = 1 WHERE run_id = ?1",
                    [&run],
                )?;
                artifact
                    .candidate
                    .holds_scope_since(&artifact.start.starting)
            }
            _ => true,
        };
        if pending {
            retain_pending_changes(
                db,
                row.get_ref(1)?.as_str()?,
                row.get_ref(2)?.as_str()?,
                row.get_ref(3)?.as_str()?,
                WriterSource::Run(&run),
            )?;
        }
    }
    Ok(())
}
