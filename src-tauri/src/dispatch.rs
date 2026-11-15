use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    conversations::{MessageRevision, SessionScope},
    protection,
    providers::{ProviderEvent, capabilities::CapabilityAssessment},
    store::Store,
};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    pub version: u16,
    pub context_id: String,
    pub scope: SessionScope,
    pub base_commit: Option<String>,
    pub submitted_messages: Vec<MessageRevision>,
    pub native_history: NativeHistory,
    #[serde(default)]
    pub capabilities: Option<CapabilityAssessment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum NativeHistory {
    Fresh,
    ProviderCursor {
        session_id: String,
        context_id: String,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Prepared,
    Submitted,
    Unconfirmed,
}

impl Delivery {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Submitted => "submitted",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

pub struct DispatchEvidence {
    pub id: String,
    pub turn_id: String,
    pub snapshot: ContextSnapshot,
    pub frames: Vec<Vec<u8>>,
    pub delivery: Delivery,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Application,
    Provider,
    Supervisor,
    Unknown,
}

impl EventSource {
    pub(crate) fn as_sql(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Provider => "provider",
            Self::Supervisor => "supervisor",
            Self::Unknown => "unknown",
        }
    }
}

pub struct RecordedAgentEvent {
    pub seq: i64,
    pub source: EventSource,
    pub event: ProviderEvent,
}

impl Store {
    pub(crate) async fn prepare_dispatch(
        &self,
        run_id: String,
        snapshot: ContextSnapshot,
        frames: Vec<Vec<u8>>,
    ) -> Result<String> {
        ensure!(
            !frames.is_empty() && frames.iter().all(|frame| !frame.is_empty()),
            "dispatch requires exact protocol frames"
        );
        let snapshot_cipher = protection::protect(&serde_json::to_vec(&snapshot)?)?;
        let frame_cipher = protection::protect(&serde_json::to_vec(&frames)?)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (context, turn, session): (String, String, String) = tx.query_row(
                "SELECT a.context_id, a.turn_id, a.agent_session_id
                 FROM agent_attempts a JOIN active_agent_attempts l ON l.run_id = a.run_id WHERE a.run_id = ?1",
                [&run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).context("dispatch requires an active attempt")?;
            ensure!(context == snapshot.context_id, "snapshot does not cover the admitted context");
            let snapshot_id = Uuid::new_v4().to_string();
            let dispatch_id = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO context_snapshots(id, run_id, context_id, payload) VALUES (?1, ?2, ?3, ?4)",
                params![snapshot_id, run_id, context, snapshot_cipher])?;
            tx.execute("INSERT INTO dispatches(id, turn_id, snapshot_id, ordinal, frames, delivery)
                        VALUES (?1, ?2, ?3, 1, ?4, 'prepared')", params![dispatch_id, turn, snapshot_id, frame_cipher])?;
            tx.execute("UPDATE agent_sessions SET cursor_context_id = NULL WHERE id = ?1", [&session])?;
            tx.commit()?;
            Ok(dispatch_id)
        }).await
    }

    pub(crate) async fn settle_dispatch(
        &self,
        dispatch_id: String,
        delivery: Delivery,
    ) -> Result<()> {
        ensure!(
            delivery != Delivery::Prepared,
            "must resolve delivery to a non-prepared state"
        );
        self.execute(move |db| {
            let updated = db.execute(
                "UPDATE dispatches SET delivery = ?1 WHERE id = ?2 AND delivery = 'prepared'",
                params![delivery.as_sql(), dispatch_id],
            )?;
            ensure!(updated == 1, "dispatch was already settled or is missing");
            Ok(())
        })
        .await
    }

    pub async fn agent_dispatch(
        &self,
        conversation_id: String,
        run_id: String,
    ) -> Result<Option<DispatchEvidence>> {
        self.execute(move |db| {
            let record = db
                .query_row(
                    "SELECT d.id, d.turn_id, s.payload, d.frames, d.delivery FROM dispatches d
                 JOIN context_snapshots s ON s.id = d.snapshot_id
                 JOIN agent_attempts a ON a.run_id = s.run_id
                 WHERE s.run_id = ?1 AND a.conversation_id = ?2 AND d.ordinal = 1",
                    params![run_id, conversation_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                            row.get::<_, Vec<u8>>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?;
            record
                .map(|(id, turn_id, snapshot, frames, delivery)| {
                    Ok(DispatchEvidence {
                        id,
                        turn_id,
                        snapshot: serde_json::from_slice(&protection::unprotect(&snapshot)?)?,
                        frames: serde_json::from_slice(&protection::unprotect(&frames)?)?,
                        delivery: serde_json::from_value(serde_json::Value::String(delivery))?,
                    })
                })
                .transpose()
        })
        .await
    }

    pub async fn agent_observations(
        &self,
        conversation_id: String,
        run_id: String,
        after: i64,
    ) -> Result<Vec<RecordedAgentEvent>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT e.seq, e.source, e.payload FROM agent_events e JOIN agent_attempts a ON a.run_id = e.run_id
                 WHERE a.run_id = ?1 AND a.conversation_id = ?2 AND e.seq > ?3 ORDER BY e.seq LIMIT 128",
            )?;
            let rows = query.query_map(params![run_id, conversation_id, after], |row| {
                Ok((row.get(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?))
            })?;
            rows.map(|row| {
                let (seq, source, payload) = row?;
                Ok(RecordedAgentEvent {
                    seq, source:serde_json::from_value(serde_json::Value::String(source))?,
                    event:serde_json::from_slice(&protection::unprotect(&payload)?)?,
                })
            }).collect()
        }).await
    }
}
