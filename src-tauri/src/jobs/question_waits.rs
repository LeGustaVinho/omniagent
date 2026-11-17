use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use ts_rs::TS;

use crate::{
    protection,
    providers::ProviderEvent,
    runs::{self, RunStatus},
    store::Store,
    workflows::settings_proposals::read_question_timeout_setting,
};

use super::{
    history::structured_job_run, pipeline::load_attempt_job, policy::attempt_requirements,
};

pub(crate) enum QuestionTimer {
    Idle,
    WakeAt(i64),
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct QuestionWaitSummary {
    pub count: u32,
    pub saved: u32,
    pub needs_review: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConversationQuestion {
    pub event: ProviderEvent,
    pub reason: String,
    pub state: String,
    pub answer_recorded: bool,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn project_timeout(db: &Connection, project: &str) -> Result<u32> {
    Ok(read_question_timeout_setting(db, project)?.value)
}

impl Store {
    pub async fn project_question_timeout(&self, project: String) -> Result<u32> {
        self.execute(move |db| project_timeout(db, &project)).await
    }

    pub async fn set_project_question_timeout(&self, project: String, minutes: u32) -> Result<()> {
        self.edit_project_question_timeout(project, None, minutes)
            .await
            .map(|_| ())
    }

    pub(crate) async fn poll_question_timer(&self, run: String, now: i64) -> Result<QuestionTimer> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let timer = poll_timer(&tx, &run, now)?;
            tx.commit()?;
            Ok(timer)
        })
        .await
    }

    pub(crate) async fn preserve_questions_after_turn(
        &self,
        run: String,
        disconnect: bool,
    ) -> Result<bool> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            stage_remaining(
                &tx,
                &run,
                if disconnect {
                    "transport_closed"
                } else {
                    "turn_stopped"
                },
            )?;
            let saving = has_saving_question(&tx, &run)?;
            tx.commit()?;
            Ok(saving)
        })
        .await
    }

    pub(crate) async fn close_questions_for_operator_stop(&self, run: String) -> Result<()> {
        self.execute(move |db| {
            db.execute(
                "UPDATE native_question_waits SET state = 'closed'
                 WHERE state = 'live' AND request_event_seq IN (SELECT request_event_seq FROM agent_questions WHERE run_id = ?1)",
                [run],
            )?;
            Ok(())
        }).await
    }

    pub async fn saved_conversation_questions(
        &self,
        conversation: String,
        run: String,
    ) -> Result<Vec<SavedConversationQuestion>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT q.request_event_seq, e.payload, q.reason, q.state, answer.request_event_seq IS NOT NULL
                 FROM open_agent_questions q
                 JOIN agent_events e ON e.seq = q.request_event_seq
                 LEFT JOIN recorded_question_answers answer ON answer.request_event_seq = q.request_event_seq
                 WHERE q.conversation_id = ?1 AND q.run_id = ?2 AND q.state <> 'live' ORDER BY e.seq",
            )?;
            let rows = query.query_map(params![conversation, run], |row| Ok((
                row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, bool>(4)?,
            )))?;
            rows.map(|row| {
                let (seq, payload, reason, mut state, answer_recorded) = row?;
                if state == "saved" && !saved_checkpoint_is_current(db, &run, seq)? {
                    state = "needs_review".into();
                }
                Ok(SavedConversationQuestion { event: serde_json::from_slice(&protection::unprotect(&payload)?)?, reason, state, answer_recorded })
            }).collect()
        }).await
    }
}

pub(crate) fn open_native_question(db: &Connection, run: &str, seq: i64) -> Result<()> {
    let project = runs::load_run(db, run)?.project_id;
    let opened = now_ms();
    let deadline = opened + i64::from(project_timeout(db, &project)?) * 60_000;
    db.execute(
        "INSERT INTO native_question_waits(request_event_seq, opened_ms, deadline_ms, state)
         VALUES (?1, ?2, ?3, 'live')",
        params![seq, opened, deadline],
    )?;
    refresh_live_question_status(db, run)
}

pub(crate) fn close_live_question(db: &Connection, run: &str, key: &[u8]) -> Result<()> {
    db.execute(
        "UPDATE native_question_waits AS w
         SET state = CASE q.delivery WHEN 'submitted' THEN 'closed' ELSE 'saving' END,
             reason = CASE q.delivery WHEN 'unconfirmed' THEN 'transport_closed' END
         FROM agent_questions AS q WHERE w.state = 'live' AND q.request_event_seq = w.request_event_seq
             AND q.run_id = ?1 AND q.request_key = ?2 AND q.delivery IN ('submitted', 'unconfirmed')",
        params![run, key],
    )?;
    refresh_live_question_status(db, run)
}

pub(crate) fn refresh_live_question_status(db: &Connection, run: &str) -> Result<()> {
    let pending: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_question_waits w JOIN agent_questions q
         ON q.request_event_seq = w.request_event_seq WHERE q.run_id = ?1 AND w.state IN ('live', 'saving')
           AND q.delivery IN ('pending', 'prepared'))",
        [run],
        |row| row.get(0),
    )?;
    let run = runs::load_run(db, run)?;
    match (run.status, pending) {
        (RunStatus::Running, true) => {
            runs::advance_run(db, &run, RunStatus::WaitingForInput).map(|_| ())
        }
        (RunStatus::WaitingForInput, false) => {
            runs::advance_run(db, &run, RunStatus::Running).map(|_| ())
        }
        _ => Ok(()),
    }
}

fn poll_timer(db: &Connection, run: &str, now: i64) -> Result<QuestionTimer> {
    if has_saving_question(db, run)? {
        return Ok(QuestionTimer::Stop);
    }
    let next: Option<i64> = db.query_row(
        "SELECT min(w.deadline_ms) FROM native_question_waits w JOIN agent_questions q
         ON q.request_event_seq = w.request_event_seq
         WHERE q.run_id = ?1 AND w.state = 'live' AND q.delivery = 'pending'",
        [run],
        |row| row.get(0),
    )?;
    let Some(deadline) = next else {
        return Ok(QuestionTimer::Idle);
    };
    if now < deadline {
        return Ok(QuestionTimer::WakeAt(deadline));
    }
    db.execute(
        "UPDATE native_question_waits SET state = 'saving',
            reason = CASE WHEN deadline_ms <= ?2 THEN 'timeout' ELSE 'turn_stopped' END
         WHERE state = 'live' AND request_event_seq IN (
             SELECT request_event_seq FROM agent_questions WHERE run_id = ?1 AND delivery = 'pending'
         )",
        params![run, now],
    )?;
    Ok(QuestionTimer::Stop)
}

pub(crate) fn stage_provider_expiry(
    db: &Connection,
    run: &str,
    key: &[u8],
    parent_item_id: Option<&str>,
) -> Result<()> {
    let request: Option<(i64, Vec<u8>)> = db.query_row(
        "SELECT q.request_event_seq, e.payload FROM agent_questions q
         JOIN native_question_waits w ON w.request_event_seq = q.request_event_seq
         JOIN agent_events e ON e.seq = q.request_event_seq
         WHERE q.run_id = ?1 AND q.request_key = ?2 AND q.delivery = 'pending' AND w.state = 'live'",
        params![run, key], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    if let Some((sequence, payload)) = request {
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        if event.location.parent_item_id.as_deref() == parent_item_id {
            db.execute(
                "UPDATE native_question_waits SET state = 'saving', reason = 'provider_expired' WHERE request_event_seq = ?1",
                [sequence],
            )?;
            stage_remaining(db, run, "turn_stopped")?;
        }
    }
    Ok(())
}

fn stage_remaining(db: &Connection, run: &str, reason: &str) -> Result<()> {
    db.execute(
        "UPDATE native_question_waits SET state = 'saving', reason = ?2
         WHERE state = 'live' AND request_event_seq IN (
             SELECT request_event_seq FROM agent_questions WHERE run_id = ?1 AND delivery IN ('pending', 'prepared')
         )",
        params![run, reason],
    )?;
    Ok(())
}

pub(crate) fn restart_question_waits(db: &Connection, run: &str, context: &str) -> Result<()> {
    stage_remaining(db, run, "runtime_restart")?;
    let local_job_tree: bool = db.query_row(
        "SELECT json_extract(s.scope, '$.runner') = ?2
         FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id WHERE a.run_id = ?1",
        params![run, crate::runner::DirectWindowsRunner::IDENTITY],
        |row| row.get(0),
    )?;
    settle_question_waits(db, run, context, local_job_tree)?;
    Ok(())
}

pub(crate) fn settle_question_waits(
    db: &Connection,
    run: &str,
    context: &str,
    stopped: bool,
) -> Result<bool> {
    let saving = has_saving_question(db, run)?;
    if saving {
        let confirmed_dispatch: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
             WHERE s.run_id = ?1 AND d.delivery = 'submitted')",
            [run],
            |row| row.get(0),
        )?;
        let write_checkpoint_ok = writer_checkpoint_is_current(db, run)?;
        db.execute(
            "UPDATE native_question_waits SET stop_confirmed = ?3, state = CASE WHEN ?2 AND EXISTS (
                 SELECT 1 FROM agent_questions q WHERE q.request_event_seq = native_question_waits.request_event_seq
                   AND q.delivery = 'pending'
             ) THEN 'saved' ELSE 'needs_review' END
             WHERE state = 'saving' AND request_event_seq IN (SELECT request_event_seq FROM agent_questions WHERE run_id = ?1)",
            params![run, stopped && confirmed_dispatch && write_checkpoint_ok, stopped],
        )?;
        db.execute(
            "INSERT INTO structured_question_continuations(request_event_seq, context_id)
             SELECT w.request_event_seq, ?1 FROM native_question_waits w JOIN agent_questions q
             ON q.request_event_seq = w.request_event_seq
             WHERE q.run_id = ?2 AND w.state IN ('saved', 'needs_review')",
            params![context, run],
        )?;
    }
    db.execute(
        "UPDATE native_question_waits SET state = 'closed'
         WHERE state = 'live' AND request_event_seq IN (SELECT request_event_seq FROM agent_questions WHERE run_id = ?1)",
        [run],
    )?;
    Ok(saving)
}

pub(crate) fn recover_question_stop_reservations(db: &Connection) -> Result<()> {
    let settled = {
        let mut query = db.prepare(
            "SELECT q.run_id FROM unconfirmed_question_stops q
             JOIN agent_attempts a ON a.run_id = q.run_id JOIN agent_sessions s ON s.id = a.agent_session_id
             WHERE json_extract(s.scope, '$.runner') = ?1",
        )?;
        query
            .query_map([crate::runner::DirectWindowsRunner::IDENTITY], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // The desktop's non-inheritable job handle drained the local CLI when the old process exited.
    for run in settled {
        let eligible = writer_checkpoint_is_current(db, &run)? && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
             WHERE s.run_id = ?1 AND d.delivery = 'submitted')", [&run], |r| r.get::<_, bool>(0),
        )?;
        db.execute(
            "UPDATE native_question_waits SET stop_confirmed = 1,
                state = CASE WHEN state = 'needs_review' AND ?2 AND EXISTS (
                    SELECT 1 FROM agent_questions q WHERE q.request_event_seq = native_question_waits.request_event_seq
                    AND q.delivery = 'closed'
                ) THEN 'saved' ELSE state END
             WHERE stop_confirmed = 0 AND request_event_seq IN (SELECT request_event_seq FROM agent_questions WHERE run_id = ?1)",
            params![run, eligible],
        )?;
    }
    Ok(())
}

pub(crate) fn archive_saved_question(db: &Connection, seq: i64) -> Result<()> {
    db.execute(
        "UPDATE native_question_waits SET state = 'closed' WHERE request_event_seq = ?1",
        [seq],
    )?;
    Ok(())
}

fn has_saving_question(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_question_waits w JOIN agent_questions q
         ON q.request_event_seq = w.request_event_seq WHERE q.run_id = ?1 AND w.state = 'saving')",
        [run],
        |row| row.get(0),
    )?)
}

pub(crate) fn has_open_questions(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM open_agent_questions WHERE run_id = ?1)",
        [run],
        |row| row.get(0),
    )?)
}

pub(super) fn question_wait_summary(
    db: &Connection,
    run: &str,
) -> Result<Option<QuestionWaitSummary>> {
    let mut statement =
        db.prepare("SELECT request_event_seq, state FROM open_agent_questions WHERE run_id = ?1")?;
    let rows = statement.query_map([run], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut summary = QuestionWaitSummary {
        count: 0,
        saved: 0,
        needs_review: 0,
    };
    for row in rows {
        let (seq, state) = row?;
        summary.count += 1;
        match state.as_str() {
            "saved" if saved_checkpoint_is_current(db, run, seq)? => summary.saved += 1,
            "saved" | "needs_review" => summary.needs_review += 1,
            _ => {}
        }
    }
    Ok((summary.count > 0).then_some(summary))
}

pub(crate) fn saved_checkpoint_is_current(db: &Connection, run: &str, seq: i64) -> Result<bool> {
    let current: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM open_agent_questions q JOIN agent_attempts a ON a.run_id = q.run_id
         WHERE q.request_event_seq = ?1 AND q.run_id = ?2 AND q.state = 'saved'
           AND q.context_id = coalesce(a.result_context_id, q.context_id)
           AND NOT EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = a.run_id))",
        params![seq, run],
        |row| row.get(0),
    )?;
    if !current || !writer_checkpoint_is_current(db, run)? {
        return Ok(false);
    }
    if let Some(origin) = structured_job_run(db, run)? {
        return Ok(attempt_requirements(db, &origin)?.is_ok());
    }
    Ok(true)
}

fn writer_checkpoint_is_current(db: &Connection, run: &str) -> Result<bool> {
    let Some(job) = load_attempt_job(db, run)? else {
        return Ok(true);
    };
    if job.mode != crate::providers::SessionMode::ApprovedWrite {
        return Ok(true);
    }
    let task = job.task_id.as_deref().context("Writer has no task scope")?;
    if crate::repository::writers::writer_owner(db, &job.project_id, task, &job.checkout_id)?
        != job.worktree_owner_task_id
    {
        return Ok(false);
    }
    // A resumed writer inherits the exact captured checkout while pending edits
    // keep the attribution of the earlier attempt that produced them.
    Ok(db.query_row(
        "WITH RECURSIVE checkpoints(run_id) AS (
            SELECT ?1
            UNION SELECT previous.checkpoint_run_id
                FROM goal_writer_attempts previous JOIN checkpoints on checkpoints.run_id = previous.run_id
            UNION SELECT question.run_id FROM structured_question_continuations link
                JOIN checkpoints ON checkpoints.run_id = link.continuation_run_id
                JOIN agent_questions question ON question.request_event_seq = link.request_event_seq
         )
         SELECT EXISTS(SELECT 1 FROM job_worktree_candidates WHERE run_id = ?1 AND capture_confirmed = 1
                   AND NOT EXISTS (SELECT 1 FROM history_deleted_attempt_diffs WHERE run_id = ?1))
         AND NOT EXISTS(SELECT 1 FROM job_writer_reservations WHERE checkout_id = ?2)
         AND NOT EXISTS(SELECT 1 FROM app_worktree_writers WHERE checkout_id = ?2)
         AND NOT EXISTS(SELECT 1 FROM pending_worktree_changes
             WHERE checkout_id = ?2 AND producing_run_id IS NOT ?1
               AND NOT (scope_task_id = ?3 AND ((producing_run_id IS NOT NULL AND producing_run_id IN (SELECT run_id FROM checkpoints))
                   OR EXISTS (SELECT 1 FROM native_goal_executors WHERE run_id = ?1))))",
        params![run, job.checkout_id, task],
        |row| row.get(0),
    )?)
}
