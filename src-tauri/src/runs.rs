use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Starting,
    Running,
    WaitingForInput,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    fn can_advance(self, next: Self) -> bool {
        use RunStatus::*;
        matches!(
            (self, next),
            (Starting, Running | Failed | Cancelled | Interrupted)
                | (
                    Running,
                    WaitingForInput | Succeeded | Failed | Cancelled | Interrupted
                )
                | (
                    WaitingForInput,
                    Running | Succeeded | Failed | Cancelled | Interrupted
                )
        )
    }

    fn as_sql(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingForInput => "waiting_for_input",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_sql(value: &str) -> Result<Self> {
        match value {
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "waiting_for_input" => Ok(Self::WaitingForInput),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => bail!("unrecognized run status"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: String,
    pub project_id: String,
    pub checkout_id: String,
    pub turn_id: String,
    pub status: RunStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEventData {
    Status {
        status: RunStatus,
    },
    Output {
        stream: OutputStream,
        preview: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    // A decimal string keeps the full SQLite sequence across JSON and JavaScript.
    pub seq: String,
    pub run_id: String,
    pub turn_id: String,
    pub data: RunEventData,
}

impl Store {
    pub async fn start_run(&self, checkout_id: String) -> Result<(RunRecord, RunEvent)> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let project_id = tx
                .query_row(
                    "SELECT project_id FROM checkouts WHERE id = ?1",
                    [&checkout_id],
                    |row| row.get(0),
                )
                .context("execution requires a registered checkout")?;
            let run = RunRecord {
                id: Uuid::new_v4().to_string(),
                project_id,
                checkout_id,
                turn_id: Uuid::new_v4().to_string(),
                status: RunStatus::Starting,
            };
            tx.execute(
                "INSERT INTO runs(id, checkout_id, status) VALUES (?1, ?2, ?3)",
                params![run.id, run.checkout_id, run.status.as_sql()],
            )?;
            tx.execute(
                "INSERT INTO turns(id, run_id, ordinal) VALUES (?1, ?2, 1)",
                params![run.turn_id, run.id],
            )?;
            let event = append_event(&tx, &run, RunEventData::Status { status: run.status })?;
            tx.commit()?;
            Ok((run, event))
        })
        .await
    }

    pub async fn record_run_event(&self, run_id: String, data: RunEventData) -> Result<RunEvent> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let run = load_run(&tx, &run_id)?;
            ensure!(!run.status.is_terminal(), "run already ended");
            if let RunEventData::Status { status } = data {
                ensure!(run.status.can_advance(status), "invalid run transition");
                tx.execute(
                    "UPDATE runs SET status = ?1 WHERE id = ?2",
                    params![status.as_sql(), run.id],
                )?;
                if status.is_terminal() {
                    tx.execute(
                        "UPDATE turns SET closed_at = unixepoch() WHERE id = ?1",
                        [&run.turn_id],
                    )?;
                }
            }
            let event = append_event(&tx, &run, data)?;
            tx.commit()?;
            Ok(event)
        })
        .await
    }
}

const RUN_RECORD_QUERY: &str = "SELECT r.id, c.project_id, c.id, t.id, r.status
     FROM runs r
     JOIN checkouts c ON c.id = r.checkout_id
     JOIN turns t ON t.run_id = r.id AND t.ordinal = (
         SELECT max(ordinal) FROM turns WHERE run_id = r.id
     )";

fn load_run(db: &Connection, id: &str) -> Result<RunRecord> {
    let query = format!("{RUN_RECORD_QUERY} WHERE r.id = ?1");
    Ok(db.query_row(&query, [id], read_run)?)
}

fn read_run(row: &Row<'_>) -> rusqlite::Result<RunRecord> {
    let status: String = row.get(4)?;
    let status = RunStatus::from_sql(&status).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            error.into_boxed_dyn_error(),
        )
    })?;
    Ok(RunRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        checkout_id: row.get(2)?,
        turn_id: row.get(3)?,
        status,
    })
}

fn append_event(db: &Connection, run: &RunRecord, data: RunEventData) -> Result<RunEvent> {
    let seq: i64 = db.query_row(
        "INSERT INTO run_events(turn_id, payload) VALUES (?1, ?2) RETURNING seq",
        params![run.turn_id, serde_json::to_string(&data)?],
        |row| row.get(0),
    )?;
    Ok(RunEvent {
        seq: seq.to_string(),
        run_id: run.id.clone(),
        turn_id: run.turn_id.clone(),
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) async fn sample_store() -> Result<(tempfile::TempDir, Store)> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("state.sqlite"))?;
        store
            .execute(|db| {
                db.execute(
                    "INSERT INTO projects VALUES ('project', 'common', 'root')",
                    [],
                )?;
                db.execute(
                    "INSERT INTO checkouts VALUES ('checkout', 'project', 'git', 'root')",
                    [],
                )?;
                Ok(())
            })
            .await?;
        Ok((directory, store))
    }

    #[tokio::test]
    async fn run_state_turn_and_sequence_commit_together() -> Result<()> {
        let (_dir, store) = sample_store().await?;
        let (run, first) = store.start_run("checkout".into()).await?;
        assert_eq!(first.seq, "1");
        let advance =
            |status| store.record_run_event(run.id.clone(), RunEventData::Status { status });
        assert!(advance(RunStatus::Succeeded).await.is_err());
        assert_eq!(advance(RunStatus::Running).await?.seq, "2");
        assert_eq!(advance(RunStatus::Succeeded).await?.seq, "3");
        assert!(advance(RunStatus::Failed).await.is_err());

        store
            .execute(move |db| {
                assert_eq!(load_run(db, &run.id)?.status, RunStatus::Succeeded);
                let closed: i64 = db.query_row(
                    "SELECT closed_at FROM turns WHERE id = ?1",
                    [&run.turn_id],
                    |row| row.get(0),
                )?;
                assert!(closed > 0);
                let count: i64 =
                    db.query_row("SELECT count(*) FROM run_events", [], |row| row.get(0))?;
                assert_eq!(count, 3);
                Ok(())
            })
            .await
    }
}
