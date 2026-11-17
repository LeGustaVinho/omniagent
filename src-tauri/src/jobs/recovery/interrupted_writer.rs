use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    protection,
    repository::{WorktreeStart, observe_interrupted_writer},
    runs::RunStatus,
    store::Store,
};

use super::{JobKind, QueueState, SessionMode, load_job};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterReconciliationReceipt {
    pub source_run_id: String,
    pub review_revision: String,
    pub reviewed_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptedWriterReview {
    pub source_run_id: String,
    pub checkout_id: String,
    pub revision: String,
    pub starting_head: String,
    pub observed_head: String,
    pub observed_status: Vec<String>,
    pub index_restored: bool,
    pub worktree_restored: bool,
    pub dispatches: Vec<WriterDispatch>,
    pub launch_requested: bool,
    pub receipt: Option<WriterReconciliationReceipt>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterDispatch {
    id: String,
    ordinal: i64,
    delivery: String,
}

impl Store {
    pub async fn interrupted_writer_review(
        &self,
        project: String,
        job: String,
        run: String,
    ) -> Result<Option<InterruptedWriterReview>> {
        self.execute(move |db| load_review(db, &project, &job, &run))
            .await
    }

    pub async fn reconcile_interrupted_writer(
        &self,
        project: String,
        job: String,
        run: String,
        expected_revision: String,
        external_effects_reconciled: bool,
    ) -> Result<WriterReconciliationReceipt> {
        ensure!(
            external_effects_reconciled,
            "Confirm that the interrupted writer's external effects were reconciled"
        );
        let receipt = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let review = load_review(&tx, &project, &job, &run)?
                    .context("The interrupted writer is no longer available for reconciliation")?;
                if let Some(receipt) = review.receipt {
                    ensure!(
                        receipt.review_revision == expected_revision,
                        "The writer was reconciled under a different review revision"
                    );
                    return Ok(receipt);
                }
                ensure!(
                    review.revision == expected_revision,
                    "The interrupted writer or its checkout changed. Reload the review"
                );
                ensure!(
                    review.worktree_restored,
                    "Restore the managed checkout, index and HEAD to their saved starting state"
                );
                let reviewed_at = tx.query_row(
                    "INSERT INTO interrupted_writer_reconciliations
                  (source_run_id, review_revision, external_effects_reconciled)
                 VALUES (?1, ?2, 1) RETURNING reviewed_at",
                    params![run, expected_revision],
                    |row| row.get(0),
                )?;
                let removed = tx.execute(
                    "DELETE FROM job_writer_reservations WHERE run_id = ?1 AND checkout_id = ?2",
                    params![run, review.checkout_id],
                )?;
                ensure!(
                    removed == 1,
                    "The interrupted writer no longer owns this checkout"
                );
                tx.commit()?;
                Ok(WriterReconciliationReceipt {
                    source_run_id: run,
                    review_revision: expected_revision,
                    reviewed_at,
                })
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(receipt)
    }
}

fn load_review(
    db: &Connection,
    project: &str,
    job_id: &str,
    run: &str,
) -> Result<Option<InterruptedWriterReview>> {
    let job = load_job(db, project, job_id)?.context("Job is unavailable in this project")?;
    let Some(attempt) = job.latest_attempt.as_ref() else {
        return Ok(None);
    };
    let Some(owner) = job.worktree_owner_task_id.as_deref() else {
        return Ok(None);
    };
    if attempt.run_id != run
        || attempt.status != RunStatus::Interrupted
        || job.mode != SessionMode::ApprovedWrite
        || job.kind != JobKind::Agent
        || job.pipeline.is_none()
        || job.downstream_wait.is_some()
        || job.queue_state == QueueState::Cancelled
    {
        return Ok(None);
    }
    let record: Option<(Vec<u8>, bool)> = db.query_row(
        "SELECT start.state, attempt.process_launch_requested
         FROM job_attempts attempt JOIN job_worktree_starts start ON start.run_id = attempt.run_id
         WHERE attempt.run_id = ?1 AND attempt.job_id = ?2 AND start.checkout_id = ?3
           AND NOT EXISTS (SELECT 1 FROM active_agent_attempts a WHERE a.run_id = attempt.run_id)
           AND NOT EXISTS (SELECT 1 FROM job_worktree_candidates c WHERE c.run_id = attempt.run_id AND c.capture_confirmed)
           AND NOT EXISTS (SELECT 1 FROM native_goal_executors n WHERE n.run_id = attempt.run_id)
           AND NOT EXISTS (SELECT 1 FROM goal_revisions g WHERE g.initial_job_id = attempt.job_id)
           AND NOT EXISTS (SELECT 1 FROM goal_writer_attempts g WHERE g.run_id = attempt.run_id)
           AND NOT EXISTS (SELECT 1 FROM unconfirmed_managed_goal_stops g WHERE g.run_id = attempt.run_id)
           AND NOT EXISTS (SELECT 1 FROM structured_question_continuations c WHERE c.continuation_run_id = attempt.run_id)
           AND (EXISTS (SELECT 1 FROM job_writer_reservations w WHERE w.run_id = attempt.run_id AND w.checkout_id = start.checkout_id)
                OR EXISTS (SELECT 1 FROM interrupted_writer_reconciliations r WHERE r.source_run_id = attempt.run_id))",
        params![run, job_id, job.checkout_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((bytes, launch_requested)) = record else {
        return Ok(None);
    };
    let dispatches = db
        .prepare(
            "SELECT d.id, d.ordinal, d.delivery FROM dispatches d
         JOIN context_snapshots s ON s.id = d.snapshot_id WHERE s.run_id = ?1 ORDER BY d.ordinal",
        )?
        .query_map([run], |row| {
            Ok(WriterDispatch {
                id: row.get(0)?,
                ordinal: row.get(1)?,
                delivery: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !launch_requested && dispatches.is_empty() {
        return Ok(None);
    }
    let start: WorktreeStart = decode_start(&bytes)?;
    let (observed, index) = observe_interrupted_writer(db, project, owner, &start)?;
    let index_proof = if start.index_entries_digest.is_some() {
        index.as_deref()
    } else {
        Some(observed.index_digest.as_str())
    };
    let index_restored = start
        .index_entries_digest
        .as_deref()
        .unwrap_or(&start.starting.index_digest)
        == index_proof.unwrap_or_default();
    let worktree_restored = start.restored_by(&observed, index.as_deref());
    let observed_proof = (
        &observed.head,
        &observed.status,
        index_proof,
        &observed.staged_diff_digest,
        &observed.unstaged_diff_digest,
        &observed.changes,
    );
    let revision = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            project,
            job_id,
            run,
            &job.pipeline,
            &start,
            launch_requested,
            &dispatches,
            observed_proof,
        ))?)
    );
    let receipt = db.query_row(
        "SELECT review_revision, reviewed_at FROM interrupted_writer_reconciliations WHERE source_run_id = ?1",
        [run], |row| Ok(WriterReconciliationReceipt {
            source_run_id: run.into(), review_revision: row.get(0)?, reviewed_at: row.get(1)?,
        }),
    ).optional()?;
    Ok(Some(InterruptedWriterReview {
        source_run_id: run.into(),
        checkout_id: job.checkout_id,
        revision,
        starting_head: start.starting.head,
        observed_head: observed.head,
        observed_status: observed.status,
        index_restored,
        worktree_restored,
        dispatches,
        launch_requested,
        receipt,
    }))
}

pub(super) fn has_reconciled_writer(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM interrupted_writer_reconciliations WHERE source_run_id = ?1)",
        [run],
        |row| row.get(0),
    )?)
}

pub(crate) fn load_reconciled_writer_start(
    db: &Connection,
    run: &str,
) -> Result<Option<WorktreeStart>> {
    let bytes: Option<Vec<u8>> = db.query_row(
        "WITH RECURSIVE previous(run_id, job_id) AS (
            SELECT earlier.run_id, retry.job_id FROM job_attempts retry
            JOIN job_attempts earlier ON earlier.run_id = retry.previous_run_id AND earlier.job_id = retry.job_id
            WHERE retry.run_id = ?1 AND retry.kind IN ('retry', 'resume')
            UNION ALL
            SELECT earlier.run_id, lineage.job_id FROM previous lineage
            JOIN job_attempts retry ON retry.run_id = lineage.run_id
            JOIN job_attempts earlier ON earlier.run_id = retry.previous_run_id AND earlier.job_id = lineage.job_id
            WHERE retry.kind IN ('retry', 'resume') AND retry.process_launch_requested = 0
         )
         SELECT s.state FROM previous lineage
         JOIN interrupted_writer_reconciliations r ON r.source_run_id = lineage.run_id
         JOIN job_worktree_starts s ON s.run_id = lineage.run_id LIMIT 1",
        [run], |row| row.get(0),
    ).optional()?;
    bytes.as_deref().map(decode_start).transpose()
}

fn decode_start(bytes: &[u8]) -> Result<WorktreeStart> {
    Ok(serde_json::from_slice(&protection::unprotect(bytes)?)?)
}
