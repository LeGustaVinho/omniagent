use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;
use uuid::Uuid;

use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct RunRecord {
    pub id: String,
    pub project_id: String,
    pub checkout_id: String,
    pub turn_id: String,
    pub status: RunStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum RunEventData {
    Status {
        status: RunStatus,
    },
    Output {
        stream: OutputStream,
        preview: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct RunEvent {
    // A decimal string keeps the full SQLite sequence across JSON and JavaScript.
    pub seq: String,
    pub run_id: String,
    pub turn_id: String,
    pub data: RunEventData,
}

const REPLAY_PAGE: i64 = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct RunSnapshot {
    pub cursor: String,
    pub runs: Vec<RunRecord>,
    pub recent_events: Vec<RunEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum SubscriptionUpdate {
    Event { event: RunEvent },
    Failed { message: String },
}

pub struct RunSubscription {
    store: Store,
    checkout_id: String,
    cursor: i64,
    wake: watch::Receiver<()>,
}

impl RunSubscription {
    pub async fn next_batch(&mut self) -> Result<Vec<RunEvent>> {
        loop {
            // Observe notifications before the read. A commit after the read can
            // then wake a receiver without relying on the frontend's timing.
            self.wake.borrow_and_update();
            let checkout_id = self.checkout_id.clone();
            let cursor = self.cursor;
            let events = self.store.execute(move |db| {
                let query = format!(
                    "{EVENT_QUERY} WHERE r.checkout_id = ?1 AND e.seq > ?2 ORDER BY e.seq LIMIT ?3"
                );
                let mut statement = db.prepare(&query)?;
                let events = statement.query_map(params![checkout_id, cursor, REPLAY_PAGE], read_event)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(events)
            }).await?;
            if let Some(last) = events.last() {
                self.cursor = last.seq.parse()?;
                return Ok(events);
            }
            self.wake.changed().await.context("event source stopped")?;
        }
    }
}

impl Store {
    pub async fn start_run(&self, checkout_id: String) -> Result<(RunRecord, RunEvent)> {
        let wake = self.event_signal.clone();
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
            wake.send_replace(());
            Ok((run, event))
        })
        .await
    }

    pub async fn record_run_event(&self, run_id: String, data: RunEventData) -> Result<RunEvent> {
        let wake = self.event_signal.clone();
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
            wake.send_replace(());
            Ok(event)
        })
        .await
    }

    pub async fn run_snapshot(&self, checkout_id: String) -> Result<RunSnapshot> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let cursor: i64 =
                tx.query_row("SELECT coalesce(max(seq), 0) FROM run_events", [], |row| {
                    row.get(0)
                })?;
            let runs = {
                let query = format!(
                    "{RUN_RECORD_QUERY} WHERE r.checkout_id = ?1 ORDER BY r.created_at DESC, r.id"
                );
                let mut statement = tx.prepare(&query)?;
                statement
                    .query_map([&checkout_id], read_run)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let recent_events = {
                let query =
                    format!("{EVENT_QUERY} WHERE r.checkout_id = ?1 ORDER BY e.seq DESC LIMIT ?2");
                let mut statement = tx.prepare(&query)?;
                let mut events = statement
                    .query_map(params![checkout_id, REPLAY_PAGE], read_event)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                events.reverse();
                events
            };
            tx.commit()?;
            Ok(RunSnapshot {
                cursor: cursor.to_string(),
                runs,
                recent_events,
            })
        })
        .await
    }

    pub fn subscribe_events(
        &self,
        checkout_id: String,
        after_seq: &str,
    ) -> Result<RunSubscription> {
        let cursor = after_seq.parse::<i64>().context("invalid event cursor")?;
        ensure!(cursor >= 0, "invalid event cursor");
        Ok(RunSubscription {
            store: self.clone(),
            checkout_id,
            cursor,
            wake: self.event_signal.subscribe(),
        })
    }
}

const RUN_RECORD_QUERY: &str = "SELECT r.id, c.project_id, c.id, t.id, r.status
     FROM runs r
     JOIN checkouts c ON c.id = r.checkout_id
     JOIN turns t ON t.run_id = r.id AND t.ordinal = (
         SELECT max(ordinal) FROM turns WHERE run_id = r.id
     )";

const EVENT_QUERY: &str = "SELECT e.seq, r.id, t.id, e.payload
    FROM run_events e JOIN turns t ON t.id = e.turn_id JOIN runs r ON r.id = t.run_id";

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

fn read_event(row: &Row<'_>) -> rusqlite::Result<RunEvent> {
    let payload: String = row.get(3)?;
    let data = serde_json::from_str(&payload).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(RunEvent {
        seq: row.get::<_, i64>(0)?.to_string(),
        run_id: row.get(1)?,
        turn_id: row.get(2)?,
        data,
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

    #[tokio::test]
    async fn snapshot_gap_and_slow_subscribers_replay_committed_events_in_order() -> Result<()> {
        use std::time::Duration;
        use tokio::time::timeout;

        let (_dir, store) = sample_store().await?;
        let (run, _) = store.start_run("checkout".into()).await?;
        let snapshot = store.run_snapshot("checkout".into()).await?;
        assert_eq!(snapshot.runs, [run.clone()]);
        assert_eq!(snapshot.recent_events[0].seq, snapshot.cursor);

        store
            .record_run_event(
                run.id.clone(),
                RunEventData::Status {
                    status: RunStatus::Running,
                },
            )
            .await?;
        let mut stream = store.subscribe_events("checkout".into(), &snapshot.cursor)?;
        for i in 0..(REPLAY_PAGE + 3) {
            store
                .record_run_event(
                    run.id.clone(),
                    RunEventData::Output {
                        stream: OutputStream::Stdout,
                        preview: i.to_string(),
                    },
                )
                .await?;
        }
        let page = timeout(Duration::from_secs(5), stream.next_batch()).await??;
        assert_eq!(page.len(), REPLAY_PAGE as usize);
        let rest = timeout(Duration::from_secs(5), stream.next_batch()).await??;
        assert_eq!(rest.len(), 4);
        let sequences = page
            .into_iter()
            .chain(rest)
            .map(|event| event.seq.parse::<i64>())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(sequences, (2..=(REPLAY_PAGE + 5)).collect::<Vec<_>>());

        let record = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            store
                .record_run_event(
                    run.id,
                    RunEventData::Status {
                        status: RunStatus::Succeeded,
                    },
                )
                .await
        };
        let (live, terminal) =
            tokio::join!(timeout(Duration::from_secs(5), stream.next_batch()), record);
        assert_eq!(live??, [terminal?]);
        Ok(())
    }
}
