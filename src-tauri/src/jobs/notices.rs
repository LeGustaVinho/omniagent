use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::sync::watch;

use crate::store::Store;

const DESKTOP_NOTIFICATIONS_KEY: &str = "desktop_notifications_enabled";

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
    pub async fn desktop_notifications_enabled(&self) -> Result<bool> {
        self.execute(|db| read_desktop_notifications_enabled(db))
            .await
    }

    pub async fn set_desktop_notifications_enabled(&self, enabled: bool) -> Result<bool> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Drop pending notices when muting and any that accumulated while muted.
            if !enabled || !read_desktop_notifications_enabled(&tx)? {
                tx.execute("DELETE FROM job_notices", [])?;
            }
            tx.execute(
                "INSERT INTO workspace_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![DESKTOP_NOTIFICATIONS_KEY, enabled.to_string()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(enabled)
    }

    pub fn job_notices(&self) -> JobNotices {
        JobNotices {
            store: self.clone(),
            wake: self.job_signal.subscribe(),
        }
    }
}

fn read_desktop_notifications_enabled(db: &Connection) -> Result<bool> {
    let value = db
        .query_row(
            "SELECT value FROM workspace_settings WHERE key = ?1",
            [DESKTOP_NOTIFICATIONS_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(value
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(true))
}

impl JobNotices {
    pub async fn next(&mut self) -> Result<JobNotice> {
        loop {
            self.wake.borrow_and_update();
            if let Some(notice) = self.store.execute(|db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if !read_desktop_notifications_enabled(&tx)? {
                    tx.execute("DELETE FROM job_notices", [])?;
                    tx.commit()?;
                    return Ok(None);
                }
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

    pub async fn is_deliverable(&self, notice: &JobNotice) -> Result<bool> {
        let sequence = notice.sequence;
        self.store
            .execute(move |db| {
                Ok(read_desktop_notifications_enabled(db)?
                    && db
                        .prepare("SELECT 1 FROM deliverable_job_notices WHERE sequence = ?1")?
                        .exists([sequence])?)
            })
            .await
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
