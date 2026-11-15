use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, TransactionBehavior};
use tokio::sync::watch;

use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobNoticeKind {
    JobApproval,
    Permission,
    Question,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobNotice {
    sequence: i64,
    pub kind: JobNoticeKind,
    pub project_id: String,
    pub job_id: String,
    pub run_id: Option<String>,
    pub request_event_seq: Option<i64>,
}

/// Committed notifications for the native desktop presenter.
pub struct JobNotices {
    store: Store,
    wake: watch::Receiver<()>,
}

impl Store {
    pub fn job_notices(&self) -> JobNotices {
        JobNotices {
            store: self.clone(),
            wake: self.job_signal.subscribe(),
        }
    }
}

impl JobNotices {
    pub async fn next(&mut self) -> Result<JobNotice> {
        loop {
            self.wake.borrow_and_update();
            if let Some(notice) = self.store.execute(|db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                // Drop requests that were answered, superseded, or cancelled before delivery.
                tx.execute(
                    "DELETE FROM job_notices WHERE sequence NOT IN (SELECT sequence FROM deliverable_job_notices)",
                    [],
                )?;
                let row = tx.query_row(
                    "SELECT sequence, kind, project_id, job_id, run_id, request_event_seq
                     FROM deliverable_job_notices ORDER BY sequence LIMIT 1",
                    [],
                    |row| Ok((
                        row.get(0)?, row.get::<_, String>(1)?, row.get(2)?,
                        row.get(3)?, row.get(4)?, row.get(5)?,
                    )),
                ).optional()?;
                tx.commit()?;
                row.map(|(sequence, kind, project_id, job_id, run_id, request_event_seq)| {
                    let kind = match kind.as_str() {
                        "job_approval" => JobNoticeKind::JobApproval,
                        "permission" => JobNoticeKind::Permission,
                        "question" => JobNoticeKind::Question,
                        "succeeded" => JobNoticeKind::Succeeded,
                        "failed" => JobNoticeKind::Failed,
                        "interrupted" => JobNoticeKind::Interrupted,
                        _ => anyhow::bail!("unknown job notice"),
                    };
                    Ok(JobNotice { sequence, kind, project_id, job_id, run_id, request_event_seq })
                }).transpose()
            }).await? {
                return Ok(notice);
            }
            self.wake
                .changed()
                .await
                .context("job notice signal closed")?;
        }
    }

    pub async fn acknowledge(&self, notice: &JobNotice) -> Result<()> {
        let sequence = notice.sequence;
        self.store
            .execute(move |db| {
                db.execute("DELETE FROM job_notices WHERE sequence = ?1", [sequence])?;
                Ok(())
            })
            .await
    }
}
