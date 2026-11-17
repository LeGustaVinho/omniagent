use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::store::Store;

use super::{WorkflowRun, control};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitConfiguration {
    pub until: WaitUntil,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WaitUntil {
    Delay { seconds: u64 },
    Scheduled { at: String },
}

impl WaitConfiguration {
    pub(super) fn is_valid(&self) -> bool {
        self.deadline(now_millis()).is_ok()
    }

    fn deadline(&self, now: i64) -> Result<i64> {
        match &self.until {
            WaitUntil::Delay { seconds } => after(now, *seconds),
            WaitUntil::Scheduled { at } => {
                let instant = OffsetDateTime::parse(at, &Rfc3339)
                    .context("Select a timestamp with a UTC offset")?;
                let nanos = instant.unix_timestamp_nanos();
                i64::try_from(-(-nanos).div_euclid(1_000_000)).context("Invalid wait deadline")
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct Deadline {
    pub(super) at: Option<i64>,
    state: String,
}

impl Deadline {
    pub(super) fn due(&self, now: i64) -> bool {
        self.state == "waiting" && self.at.is_some_and(|at| at <= now)
    }

    pub(super) fn cancelled(&self) -> bool {
        self.state == "cancelled"
    }
}

pub(super) fn now_millis() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

pub(super) fn after(now: i64, seconds: u64) -> Result<i64> {
    let millis = i64::try_from(seconds)?.checked_mul(1000);
    millis
        .and_then(|millis| now.checked_add(millis))
        .context("The duration exceeds the supported time range")
}

pub(super) fn read(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
) -> Result<Option<Deadline>> {
    Ok(db
        .query_row(
            "SELECT deadline_at, state FROM workflow_wait_deadlines
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3 AND unit_key = ?4",
            params![
                run.id,
                node,
                run.plan_revision.as_deref().unwrap_or(""),
                unit.unwrap_or("")
            ],
            |row| {
                Ok(Deadline {
                    at: row.get(0)?,
                    state: row.get(1)?,
                })
            },
        )
        .optional()?)
}

pub(super) fn register(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    purpose: &str,
    deadline: Option<i64>,
) -> Result<Deadline> {
    db.execute(
        "INSERT INTO workflow_wait_deadlines (pipeline_run_id, node_instance_id, plan_key, unit_key, purpose, deadline_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT DO NOTHING",
        params![run.id, node, run.plan_revision.as_deref().unwrap_or(""), unit.unwrap_or(""), purpose, deadline],
    )?;
    read(db, run, node, unit)?.context("The pinned workflow deadline is unavailable")
}

fn transition(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    state: &str,
) -> Result<bool> {
    Ok(db.execute(
        "UPDATE workflow_wait_deadlines SET state = ?5
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3 AND unit_key = ?4 AND state = 'waiting'",
        params![run.id, node, run.plan_revision.as_deref().unwrap_or(""), unit.unwrap_or(""), state],
    )? == 1)
}

pub(super) fn finish(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
) -> Result<()> {
    ensure!(
        transition(db, run, node, unit, "resolved")?,
        "The workflow wait is no longer pending"
    );
    Ok(())
}

pub(super) fn cancel(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
) -> Result<()> {
    transition(db, run, node, unit, "cancelled")?;
    Ok(())
}

pub(super) fn wait(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    config: &WaitConfiguration,
) -> Result<bool> {
    let now = now_millis();
    let deadline = register(db, run, node, unit, "wait", Some(config.deadline(now)?))?;
    if !deadline.due(now) {
        return Ok(false);
    }
    finish(db, run, node, unit)?;
    Ok(true)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimedWorkflowWait {
    node_id: String,
    unit_id: Option<String>,
    deadline_at: i64,
}

pub(super) fn active_waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<TimedWorkflowWait>> {
    Ok(db.prepare(
        "SELECT node_instance_id, nullif(unit_key, ''), deadline_at FROM workflow_wait_deadlines
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND purpose = 'wait' AND state = 'waiting'
         ORDER BY deadline_at, node_instance_id, unit_key",
    )?.query_map(params![run.id, run.plan_revision.as_deref().unwrap_or("")], |row| {
        Ok(TimedWorkflowWait { node_id: row.get(0)?, unit_id: row.get(1)?, deadline_at: row.get(2)? })
    })?.collect::<rusqlite::Result<_>>()?)
}

impl Store {
    pub(crate) async fn refresh_due_workflow_waits(&self) -> Result<usize> {
        self.execute(|db| {
            let due = db.prepare(
                "SELECT r.project_id, w.pipeline_run_id FROM workflow_wait_deadlines w
                 JOIN job_pipeline_runs r ON r.id = w.pipeline_run_id
                 WHERE w.state = 'waiting' AND w.deadline_at <= ?1 AND w.plan_key = coalesce(r.plan_revision, '')
                   AND (w.unit_key = '' OR EXISTS (
                     SELECT 1 FROM workflow_delivery_units u WHERE u.unit_id = w.unit_key
                     AND u.pipeline_run_id = w.pipeline_run_id AND u.plan_revision = r.plan_revision))
                 GROUP BY r.project_id, w.pipeline_run_id ORDER BY min(w.deadline_at), w.pipeline_run_id",
            )?.query_map([now_millis()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
             .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut first_error = None;
            for (project, run) in &due {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                match control::refresh(&tx, project, run).and_then(|()| Ok(tx.commit()?)) {
                    Ok(()) => (),
                    Err(error) => { first_error.get_or_insert(error); }
                }
            }
            first_error.map_or(Ok(due.len()), Err)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_use_utc_and_respect_the_configured_instant() -> Result<()> {
        let scheduled = |at: &str| WaitConfiguration {
            until: WaitUntil::Scheduled { at: at.into() },
        };
        assert_eq!(scheduled("1970-01-01T00:00:00.0001Z").deadline(1000)?, 1);
        assert_eq!(scheduled("1969-12-31T21:00:01-03:00").deadline(0)?, 1000);
        assert!(!scheduled("1970-01-01T00:00:01").is_valid());
        assert_eq!(after(101, 2)?, 2101);
        assert!(after(0, u64::MAX).is_err());
        Ok(())
    }
}
