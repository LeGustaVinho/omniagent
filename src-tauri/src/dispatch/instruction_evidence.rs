use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{
    conversations::MessageRevision,
    protection,
    providers::{ProviderEvent, ProviderEventData, steering::SteerOutcome},
};

use super::Delivery;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionDispatchMode {
    Standalone,
    Continuation,
    Steering,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionDispatchStatus {
    Prepared,
    Submitted,
    Accepted,
    Rejected,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionReview {
    Undelivered,
    Discarded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum InstructionReceipt {
    TurnStarted {
        event_sequence: String,
    },
    SteeringResponse {
        event_sequence: String,
        outcome: SteerOutcome,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionDispatch {
    pub instruction_id: String,
    pub revision_id: String,
    pub text: String,
    pub mode: InstructionDispatchMode,
    pub status: InstructionDispatchStatus,
    pub provider_session_id: Option<String>,
    pub provider_turn_id: Option<String>,
    pub receipt: Option<InstructionReceipt>,
    pub steering_frame: Option<String>,
    pub review: Option<InstructionReview>,
}

pub(super) fn load(
    db: &Connection,
    run_id: &str,
    submitted: &[MessageRevision],
    delivery: Delivery,
) -> Result<Vec<InstructionDispatch>> {
    let mut statement = db.prepare(
        "WITH deliveries AS (
             SELECT i.job_id, i.revision_id, i.route, i.steering_state, i.prepared_event_seq,
                    i.settled_event_seq, i.provider_session_id, i.provider_turn_id, i.review_resolution
             FROM instruction_delivery_attempts i
             WHERE i.run_id = ?1 AND (i.route <> 'steering' OR i.prepared_event_seq IS NOT NULL)
             UNION ALL
             SELECT i.job_id, i.revision_id, 'standalone', NULL, NULL, NULL, NULL, NULL, NULL
             FROM queued_conversation_instructions i JOIN job_attempts j ON j.job_id = i.job_id
             WHERE j.run_id = ?1 AND j.kind = 'initial' AND NOT EXISTS (
                 SELECT 1 FROM instruction_delivery_attempts WHERE run_id = ?1 AND job_id = i.job_id
             )
         )
         SELECT i.job_id, i.revision_id, i.route, i.steering_state,
                prepared.payload, settled.payload, settled.seq, settled.source,
                coalesce(i.provider_session_id, s.provider_session_id),
                coalesce(i.provider_turn_id, a.provider_turn_id), i.review_resolution,
                CASE WHEN i.route = 'steering' THEN m.content END
         FROM deliveries i
         JOIN jobs j ON j.id = i.job_id
         JOIN message_revisions m ON m.id = i.revision_id
         JOIN agent_attempts a ON a.run_id = ?1
         JOIN agent_sessions s ON s.id = a.agent_session_id
         LEFT JOIN agent_events prepared ON prepared.seq = i.prepared_event_seq
             AND prepared.run_id = a.run_id AND prepared.source = 'application'
         LEFT JOIN agent_events settled ON settled.seq = i.settled_event_seq AND settled.run_id = a.run_id
         ORDER BY i.route = 'steering', i.prepared_event_seq, j.sequence",
    )?;
    let mut rows = statement.query([run_id])?;
    let mut instructions = Vec::new();
    let mut turn_receipt = None;
    while let Some(row) = rows.next()? {
        let revision_id: String = row.get(1)?;
        let mode = from_name(row.get(2)?)?;
        let mut instruction = InstructionDispatch {
            instruction_id: row.get(0)?,
            revision_id,
            mode,
            text: String::new(),
            status: match delivery {
                Delivery::Prepared => InstructionDispatchStatus::Prepared,
                Delivery::Submitted => InstructionDispatchStatus::Submitted,
                Delivery::Unconfirmed => InstructionDispatchStatus::Unconfirmed,
            },
            provider_session_id: row.get(8)?,
            provider_turn_id: row.get(9)?,
            receipt: None,
            steering_frame: None,
            review: row
                .get::<_, Option<String>>(10)?
                .map(from_name)
                .transpose()?,
        };

        if mode == InstructionDispatchMode::Steering {
            let prepared: Vec<u8> = row.get(4)?;
            let ProviderEventData::SteeringPrepared {
                message_id,
                target,
                frame,
            } = decode(&prepared)?.data
            else {
                bail!("prepared instruction has no steering frame");
            };
            ensure!(
                matches_steer(&instruction, &message_id, &target),
                "steering frame has a different receiving turn"
            );
            instruction.steering_frame = Some(String::from_utf8(frame)?);
            let content: Vec<u8> = row.get(11)?;
            instruction.text = String::from_utf8(protection::unprotect(&content)?)?;
            instruction.status = from_name(row.get(3)?)?;
            if row.get_ref(7)?.as_str().ok() == Some("provider") {
                let settled: Vec<u8> = row.get(5)?;
                let ProviderEventData::SteeringSettled {
                    message_id,
                    target,
                    outcome,
                } = decode(&settled)?.data
                else {
                    bail!("instruction settlement has no native steering response");
                };
                ensure!(
                    matches_steer(&instruction, &message_id, &target),
                    "steering response has a different receiving turn"
                );
                instruction.receipt = Some(InstructionReceipt::SteeringResponse {
                    event_sequence: row.get::<_, i64>(6)?.to_string(),
                    outcome,
                });
            }
        } else {
            let Some(message) = submitted
                .iter()
                .find(|message| message.revision_id == instruction.revision_id)
            else {
                continue;
            };
            instruction.text.clone_from(&message.text);
            if let Some(turn) = &instruction.provider_turn_id {
                if turn_receipt.is_none() {
                    turn_receipt = Some(find_turn_receipt(db, run_id, turn)?);
                }
                instruction.receipt = turn_receipt.clone().flatten();
            }
        }
        instructions.push(instruction);
    }
    Ok(instructions)
}

fn matches_steer(
    instruction: &InstructionDispatch,
    revision: &str,
    target: &crate::providers::steering::SteerTarget,
) -> bool {
    revision == instruction.revision_id
        && instruction.provider_session_id.as_ref() == Some(&target.session_id)
        && instruction.provider_turn_id.as_ref() == Some(&target.turn_id)
}

fn find_turn_receipt(db: &Connection, run: &str, turn: &str) -> Result<Option<InstructionReceipt>> {
    let mut query = db.prepare(
        "SELECT seq, payload FROM agent_events WHERE run_id = ?1 AND source = 'provider' ORDER BY seq",
    )?;
    let mut rows = query.query([run])?;
    while let Some(row) = rows.next()? {
        let payload: Vec<u8> = row.get(1)?;
        let event = decode(&payload)?;
        if matches!(event.data, ProviderEventData::TurnStarted)
            && event.location.parent_item_id.is_none()
            && event.location.turn_id.as_deref() == Some(turn)
        {
            return Ok(Some(InstructionReceipt::TurnStarted {
                event_sequence: row.get::<_, i64>(0)?.to_string(),
            }));
        }
    }
    Ok(None)
}

fn decode(payload: &[u8]) -> Result<ProviderEvent> {
    serde_json::from_slice(&protection::unprotect(payload)?)
        .context("read protected delivery event")
}

fn from_name<T: serde::de::DeserializeOwned>(name: String) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(name))?)
}
