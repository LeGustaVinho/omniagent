use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    dispatch::EventSource,
    protection,
    providers::{
        ProviderEvent, ProviderEventData, RequestId,
        goal::{NativeGoalAction, NativeGoalControlOutcome, NativeGoalState},
    },
};

use super::{is_native_goal_run, record_native_goal};

fn action_key(action: NativeGoalAction) -> &'static str {
    match action {
        NativeGoalAction::SeedContext => "seed_context",
        NativeGoalAction::Start => "start",
        NativeGoalAction::Status => "status",
        NativeGoalAction::Pause => "pause",
        NativeGoalAction::Stop => "stop",
        NativeGoalAction::Resume => "resume",
        NativeGoalAction::Cancel => "cancel",
    }
}

fn matching_session(db: &Connection, run: &str, event: &ProviderEvent) -> Result<bool> {
    if event.location.parent_item_id.is_some() {
        return Ok(false);
    }
    let Some(session_id) = event.location.session_id.as_deref() else {
        return Ok(false);
    };
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
                       WHERE a.run_id = ?1 AND s.provider_session_id = ?2)",
        params![run, session_id],
        |row| row.get(0),
    ).map_err(Into::into)
}

fn recorded_control(
    db: &Connection,
    run: &str,
    request: &RequestId,
    action: NativeGoalAction,
) -> Result<Option<Option<String>>> {
    Ok(db.query_row(
        "SELECT outcome FROM native_goal_controls WHERE run_id = ?1 AND request_key = ?2 AND action = ?3",
        params![run, serde_json::to_string(request)?, action_key(action)],
        |row| row.get(0),
    ).optional()?)
}

pub(crate) fn record_native_goal_control(
    db: &Connection,
    run: &str,
    seq: i64,
    source: EventSource,
    event: &ProviderEvent,
) -> Result<()> {
    match &event.data {
        ProviderEventData::NativeGoalContextSeeded {
            request_id,
            accepted: true,
        } => {
            ensure!(
                source == EventSource::Provider && matching_session(db, run, event)?,
                "Native goal context receipt belongs to another session"
            );
            let frames: Vec<u8> = db.query_row(
                "SELECT d.frames FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                 WHERE s.run_id = ?1 AND d.ordinal = 1 AND d.delivery = 'submitted'",
                [run],
                |row| row.get(0),
            )?;
            let frames: Vec<Vec<u8>> = serde_json::from_slice(&protection::unprotect(&frames)?)?;
            let [frame] = frames.as_slice() else {
                anyhow::bail!("Goal context receipt has no unique dispatch");
            };
            let frame: serde_json::Value = serde_json::from_slice(frame)?;
            let id = serde_json::to_value(request_id)?;
            ensure!(
                frame["id"] == id
                    && frame["method"] == "thread/inject_items"
                    && frame["params"]["threadId"].as_str() == event.location.session_id.as_deref(),
                "Goal context receipt does not match the dispatched context"
            );
            db.execute(
                "INSERT INTO native_goal_context_receipts VALUES (?1, ?2, ?3)",
                params![run, event.location.session_id, seq],
            )?;
        }
        ProviderEventData::NativeGoalControlPrepared {
            request_id,
            action,
            frame,
        } => {
            ensure!(
                source == EventSource::Application
                    && is_native_goal_run(db, run)?
                    && matching_session(db, run, event)?
                    && event.location.turn_id.is_none()
                    && !frame.is_empty(),
                "Native goal control does not match an authorized executor"
            );
            db.execute(
                "INSERT INTO native_goal_controls(run_id, request_key, action, prepared_event_seq)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    run,
                    serde_json::to_string(request_id)?,
                    action_key(*action),
                    seq
                ],
            )?;
        }
        ProviderEventData::NativeGoalControlSettled {
            request_id,
            action,
            result,
        } => {
            ensure!(
                matching_receipt(db, run, source, event, *action, result)?,
                "Native goal control receipt does not match the executor"
            );
            let previous = recorded_control(db, run, request_id, *action)?;
            ensure!(
                previous.is_some(),
                "Native goal control receipt has no prepared request"
            );
            if matches!(previous, Some(Some(_))) {
                return Ok(());
            }
            let outcome = match result {
                NativeGoalControlOutcome::Confirmed { observation } => {
                    record_native_goal(db, run, seq, observation)?;
                    "confirmed"
                }
                NativeGoalControlOutcome::Rejected => "rejected",
                NativeGoalControlOutcome::Unconfirmed => "unconfirmed",
            };
            db.execute(
                "UPDATE native_goal_controls SET settled_event_seq = ?4, outcome = ?5
                 WHERE run_id = ?1 AND request_key = ?2 AND action = ?3 AND outcome IS NULL",
                params![
                    run,
                    serde_json::to_string(request_id)?,
                    action_key(*action),
                    seq,
                    outcome
                ],
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn matching_receipt(
    db: &Connection,
    run: &str,
    source: EventSource,
    event: &ProviderEvent,
    action: NativeGoalAction,
    result: &NativeGoalControlOutcome,
) -> Result<bool> {
    let authentic_source = match result {
        NativeGoalControlOutcome::Confirmed { observation } => {
            source == EventSource::Provider
                && event.location.session_id.as_ref() == Some(&observation.provider_session_id)
                && observation.provider_turn_id.is_none()
                && match action {
                    NativeGoalAction::SeedContext => false,
                    NativeGoalAction::Start | NativeGoalAction::Resume => {
                        observation.state == NativeGoalState::Active
                    }
                    NativeGoalAction::Status => observation.state != NativeGoalState::Unknown,
                    NativeGoalAction::Pause => observation.state == NativeGoalState::Paused,
                    NativeGoalAction::Stop => matches!(
                        observation.state,
                        NativeGoalState::Paused
                            | NativeGoalState::Cancelled
                            | NativeGoalState::Cleared
                    ),
                    NativeGoalAction::Cancel => matches!(
                        observation.state,
                        NativeGoalState::Cancelled | NativeGoalState::Cleared
                    ),
                }
        }
        NativeGoalControlOutcome::Rejected => source == EventSource::Provider,
        NativeGoalControlOutcome::Unconfirmed => source != EventSource::Unknown,
    };
    Ok(authentic_source && event.location.turn_id.is_none() && matching_session(db, run, event)?)
}

pub(crate) fn late_native_goal_evidence(
    db: &Connection,
    run: &str,
    source: EventSource,
    event: &ProviderEvent,
) -> Result<bool> {
    match &event.data {
        ProviderEventData::GoalObserved { observation } if source == EventSource::Provider => {
            if event.location.session_id.as_ref() != Some(&observation.provider_session_id)
                || event.location.turn_id != observation.provider_turn_id
                || !matching_session(db, run, event)?
            {
                return Ok(false);
            }
            Ok(db.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM native_goal_observations WHERE run_id = ?1
                   UNION SELECT 1 FROM native_goal_controls WHERE run_id = ?1
                   UNION SELECT 1 FROM native_goal_turns WHERE run_id = ?1
                 )",
                [run],
                |row| row.get(0),
            )?)
        }
        ProviderEventData::NativeGoalControlSettled {
            request_id,
            action,
            result,
        } => Ok(matching_receipt(db, run, source, event, *action, result)?
            && recorded_control(db, run, request_id, *action)?.is_some()),
        _ => Ok(false),
    }
}

pub(super) fn confirmed_stop_control(
    db: &Connection,
    run: &str,
    revision: &str,
    action: &str,
) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT c.action = ?3 AND coalesce(c.outcome = 'confirmed', 0)
         FROM native_goal_controls c JOIN goal_revision_controls g ON g.stop_run_id = c.run_id
         WHERE c.run_id = ?1 AND g.revision_id = ?2 AND c.action <> 'status'
           AND c.prepared_event_seq > g.stop_event_floor
         ORDER BY c.prepared_event_seq DESC LIMIT 1",
            params![run, revision, action],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}
