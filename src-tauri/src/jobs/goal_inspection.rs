use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    protection,
    providers::{
        Provider, ProviderEvent, ProviderEventData,
        goal::{NativeGoalControlOutcome, NativeGoalObservation},
    },
    store::Store,
};

use super::{
    GoalCheckpoint, GoalContinuationOwner, GoalControlState, GoalDefinition, GoalRevision,
    goals::{load_revision, orchestrator_goal},
    question_waits::has_open_questions,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectedGoal {
    revision_id: String,
    definition: GoalDefinition,
    continuation_owner: GoalContinuationOwner,
    control_state: GoalControlState,
    blocker: Option<&'static str>,
    checkpoint: Option<GoalCheckpoint>,
    attempt: Option<GoalAttempt>,
    evidence: Option<GoalAttemptEvidence>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalAttempt {
    run_id: String,
    conversation_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalAttemptEvidence {
    provider: Provider,
    usage: Option<GoalUsage>,
    native_start: Option<NativeGoalStart>,
    native_turns: Vec<NativeGoalTurn>,
    native_observation: Option<GoalObservation>,
    provider_result: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalUsage {
    event_sequence: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeGoalStart {
    event_sequence: String,
    frame: String,
    outcome: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeGoalTurn {
    turn_id: String,
    started_event: String,
    finished_event: Option<String>,
    outcome: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalObservation {
    event_sequence: String,
    observation: NativeGoalObservation,
}

impl Store {
    pub async fn project_task_goals(
        &self,
        project: String,
        task: String,
    ) -> Result<Vec<InspectedGoal>> {
        self.execute(move |db| {
            let mut revisions = db.prepare(
                "SELECT r.id FROM goal_revisions r JOIN goals g ON g.id = r.goal_id
                 JOIN jobs j ON j.id = r.initial_job_id AND j.project_id = g.project_id
                 WHERE g.project_id = ?1 AND j.task_id = ?2
                 ORDER BY r.created_ms DESC, r.id",
            )?;
            let rows =
                revisions.query_map(params![project, task], |row| row.get::<_, String>(0))?;
            rows.map(|row| inspect_goal(db, load_revision(db, row?)?, None))
                .collect()
        })
        .await
    }
}

pub(super) fn inspect_run_goal(db: &Connection, run: &str) -> Result<Option<InspectedGoal>> {
    orchestrator_goal(db, run)?
        .map(|goal| inspect_goal(db, goal, Some(run)))
        .transpose()
}

fn inspect_goal(db: &Connection, goal: GoalRevision, run: Option<&str>) -> Result<InspectedGoal> {
    let attempt = db
        .query_row(
            "SELECT a.run_id, a.conversation_id
         FROM goal_continuations c JOIN job_attempts j ON j.job_id = c.job_id
         JOIN agent_attempts a ON a.run_id = j.run_id
         WHERE c.revision_id = ?1
         ORDER BY c.checkpoint_ordinal DESC, j.ordinal DESC LIMIT 1",
            [&goal.id],
            |row| {
                Ok(GoalAttempt {
                    run_id: row.get(0)?,
                    conversation_id: row.get(1)?,
                })
            },
        )
        .optional()?;
    let waiting = attempt
        .as_ref()
        .map(|attempt| has_open_questions(db, &attempt.run_id))
        .transpose()?
        .unwrap_or_default();
    let blocker = goal.control_state.blocker().or_else(|| {
        if waiting {
            return Some("Waiting for input");
        }
        match goal
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.next_action.as_str())
        {
            Some("blocked") => Some("The checkpoint reported a blocker"),
            Some("limit_reached") => Some("The goal reached a configured limit"),
            _ => None,
        }
    });
    let inspected_run = run.or_else(|| attempt.as_ref().map(|attempt| attempt.run_id.as_str()));
    let evidence = inspected_run
        .map(|run| attempt_evidence(db, run))
        .transpose()?;
    Ok(InspectedGoal {
        revision_id: goal.id,
        definition: goal.definition,
        continuation_owner: goal.continuation_owner,
        control_state: goal.control_state,
        checkpoint: goal.checkpoint,
        blocker,
        attempt,
        evidence,
    })
}

fn attempt_evidence(db: &Connection, run: &str) -> Result<GoalAttemptEvidence> {
    let provider: String = db.query_row(
        "SELECT json_quote(json_extract(s.scope, '$.provider'))
         FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE a.run_id = ?1",
        [run],
        |row| row.get(0),
    )?;
    let provider = serde_json::from_str(&provider)?;
    let start = db
        .query_row(
            "SELECT e.seq, e.payload, c.outcome FROM native_goal_controls c
         JOIN agent_events e ON e.seq = c.prepared_event_seq AND e.run_id = c.run_id
         WHERE c.run_id = ?1 AND c.action = 'start' AND e.source = 'application'
         ORDER BY e.seq LIMIT 1",
            [run],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let native_start = start
        .map(|(sequence, payload, outcome)| -> Result<_> {
            let ProviderEventData::NativeGoalControlPrepared { frame, .. } =
                decode_event(&payload)?.data
            else {
                anyhow::bail!("Goal input receipt does not contain a prepared native command");
            };
            Ok(NativeGoalStart {
                event_sequence: sequence.to_string(),
                frame: String::from_utf8(frame)?,
                outcome,
            })
        })
        .transpose()?;
    let observed = db
        .query_row(
            "SELECT e.seq, e.payload FROM native_goal_observations n
         JOIN agent_events e ON e.seq = n.event_seq AND e.run_id = n.run_id
         WHERE n.run_id = ?1 AND e.source = 'provider'",
            [run],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    let native_observation = observed
        .map(|(sequence, payload)| -> Result<_> {
            let observation = match decode_event(&payload)?.data {
                ProviderEventData::GoalObserved { observation }
                | ProviderEventData::NativeGoalControlSettled {
                    result: NativeGoalControlOutcome::Confirmed { observation },
                    ..
                } => observation,
                _ => anyhow::bail!("Goal observation does not contain a provider receipt"),
            };
            Ok(GoalObservation {
                event_sequence: sequence.to_string(),
                observation,
            })
        })
        .transpose()?;
    let mut turns = db.prepare(
        "SELECT t.provider_turn_id, t.started_event_seq, t.finished_event_seq, t.outcome
         FROM native_goal_turns t JOIN agent_events e ON e.seq = t.started_event_seq AND e.source = 'provider'
         WHERE t.run_id = ?1 ORDER BY t.started_event_seq",
    )?;
    let native_turns = turns
        .query_map([run], |row| {
            Ok(NativeGoalTurn {
                turn_id: row.get(0)?,
                started_event: row.get::<_, i64>(1)?.to_string(),
                finished_event: row.get::<_, Option<i64>>(2)?.map(|seq| seq.to_string()),
                outcome: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let provider_result = db
        .query_row(
            "SELECT provider_result FROM native_goal_executors WHERE run_id = ?1",
            [run],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(GoalAttemptEvidence {
        provider,
        usage: latest_usage(db, run)?,
        native_start,
        native_turns,
        native_observation,
        provider_result,
    })
}

fn latest_usage(db: &Connection, run: &str) -> Result<Option<GoalUsage>> {
    let mut query = db.prepare(
        "SELECT seq, payload FROM agent_events WHERE run_id = ?1 AND source = 'provider' ORDER BY seq DESC",
    )?;
    let mut rows = query.query([run])?;
    while let Some(row) = rows.next()? {
        let event = decode_event(&row.get::<_, Vec<u8>>(1)?)?;
        if event.location.parent_item_id.is_none()
            && let ProviderEventData::Usage {
                input,
                output,
                cached_input,
            } = event.data
        {
            return Ok(Some(GoalUsage {
                event_sequence: row.get::<_, i64>(0)?.to_string(),
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: cached_input,
            }));
        }
    }
    Ok(None)
}

fn decode_event(bytes: &[u8]) -> Result<ProviderEvent> {
    Ok(serde_json::from_slice(&protection::unprotect(bytes)?)?)
}
