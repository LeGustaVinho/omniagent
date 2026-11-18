use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{providers::questions::QuestionRoute, store::Store};

use super::{
    Job, JobContext, QueueState, change_pending_job_state, load_job,
    pipeline::{ReleaseBlocker, load_attempt_job, release_blocker},
    policy::attempt_requirements,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum QueueHistoryPolicy {
    #[default]
    Review,
    Recorded,
    Cancel,
}

impl QueueHistoryPolicy {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Recorded => "recorded",
            Self::Cancel => "cancel",
        }
    }

    fn from_sql(value: Option<String>) -> Result<Self> {
        match value.as_deref() {
            None | Some("review") => Ok(Self::Review),
            Some("recorded") => Ok(Self::Recorded),
            Some("cancel") => Ok(Self::Cancel),
            _ => anyhow::bail!("Unknown queue history policy"),
        }
    }
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct QueueHistorySettings {
    pub project_default: QueueHistoryPolicy,
    pub conversation_override: Option<QueueHistoryPolicy>,
}

fn project_policy(db: &Connection, project: &str) -> Result<QueueHistoryPolicy> {
    let value = db.query_row(
        "SELECT p.policy FROM projects j LEFT JOIN project_queue_history_policies p ON p.project_id = j.id
         WHERE j.id = ?1",
        [project],
        |row| row.get(0),
    )?;
    QueueHistoryPolicy::from_sql(value)
}

pub(super) fn conversation_policy(
    db: &Connection,
    conversation: &str,
) -> Result<(String, QueueHistorySettings)> {
    let (project, value): (String, Option<String>) = db.query_row(
        "SELECT w.project_id, c.queue_history_policy FROM conversations c
         JOIN checkouts w ON w.id = c.checkout_id WHERE c.id = ?1",
        [conversation],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let settings = QueueHistorySettings {
        project_default: project_policy(db, &project)?,
        conversation_override: value
            .map(|value| QueueHistoryPolicy::from_sql(Some(value)))
            .transpose()?,
    };
    Ok((project, settings))
}

impl Store {
    pub async fn project_queue_history_policy(
        &self,
        project: String,
    ) -> Result<QueueHistoryPolicy> {
        self.execute(move |db| project_policy(db, &project)).await
    }

    pub async fn set_project_queue_history_policy(
        &self,
        project: String,
        policy: QueueHistoryPolicy,
    ) -> Result<()> {
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_queue_history_policies(project_id, policy) VALUES (?1, ?2)
                 ON CONFLICT(project_id) DO UPDATE SET policy = excluded.policy",
                params![project, policy.as_sql()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn conversation_queue_history_policy(
        &self,
        id: String,
    ) -> Result<QueueHistorySettings> {
        self.execute(move |db| Ok(conversation_policy(db, &id)?.1))
            .await
    }

    pub async fn set_conversation_queue_history_policy(
        &self,
        id: String,
        policy: Option<QueueHistoryPolicy>,
    ) -> Result<QueueHistorySettings> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE conversations SET queue_history_policy = ?1 WHERE id = ?2",
                params![policy.map(QueueHistoryPolicy::as_sql), id],
            )?;
            let (_, settings) = conversation_policy(&tx, &id)?;
            tx.commit()?;
            Ok(settings)
        })
        .await
    }
}

pub(super) struct HistoryBinding {
    pub context: JobContext,
    pub pipeline_pin: bool,
    pub question_route: QuestionRoute,
}

pub(super) fn load_binding(db: &Connection, job: &str) -> Result<Option<HistoryBinding>> {
    Ok(db
        .query_row(
            "SELECT c.conversation_id, c.id, h.pipeline_pin, h.structured_questions FROM job_history_bindings h
         JOIN conversation_contexts c ON c.id = h.context_id WHERE h.job_id = ?1",
            [job],
            |row| {
                Ok(HistoryBinding {
                    context: JobContext {
                        conversation_id: row.get(0)?,
                        context_id: row.get(1)?,
                    },
                    pipeline_pin: row.get(2)?,
                question_route: if row.get(3)? { QuestionRoute::StructuredTurn } else { QuestionRoute::Native },
                })
            },
        )
        .optional()?)
}

pub(super) fn bind_pipeline_context(
    db: &Connection,
    job: &str,
    context: &str,
    route: QuestionRoute,
) -> Result<()> {
    db.execute(
        "INSERT INTO job_history_bindings(job_id, context_id, pipeline_pin, structured_questions)
         VALUES (?1, ?2, 1, ?3)",
        params![job, context, !route.is_native()],
    )?;
    Ok(())
}

pub(super) fn uses_recorded_checkpoint(
    db: &Connection,
    job: &Job,
    run: &str,
    conversation: &str,
) -> Result<bool> {
    if load_binding(db, &job.id)?
        .is_some_and(|binding| binding.context.conversation_id == conversation)
    {
        return Ok(true);
    }
    Ok(db.query_row(
        "SELECT detached_context FROM agent_attempts WHERE run_id = ?1 AND conversation_id = ?2",
        params![run, conversation],
        |row| row.get(0),
    )?)
}

pub(super) fn effective_release_blocker(
    db: &Connection,
    job: &Job,
) -> Result<Option<ReleaseBlocker>> {
    if load_binding(db, &job.id)?.is_some_and(|binding| binding.pipeline_pin) {
        return pinned_release_blocker(db, job);
    }
    release_blocker(db, job)
}

fn pinned_release_blocker(db: &Connection, job: &Job) -> Result<Option<ReleaseBlocker>> {
    let origin = job
        .pipeline
        .as_ref()
        .context("Pinned job has no pipeline origin")?;
    if let Some(release) = &origin.release
        && crate::workflows::write_scope::release_is_human(
            db, &job.project_id, &origin.pipeline_run_id, &release.approval_node_id,
        )?
    {
        return release_blocker(db, job);
    }
    if let Some(current) = crate::workflows::stage_plan::plan_head_is_current(db, &job.id)? {
        if !current {
            return Ok(Some(ReleaseBlocker::StalePlan));
        }
        return release_blocker(db, job);
    }
    // A structured planner pins a conversation input to produce the plan.
    // The plan it publishes will be pinned when downstream jobs consume it.
    if origin.plan_revision.is_none()
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_structured_planning_jobs WHERE job_id = ?1)",
            [&job.id],
            |row| row.get::<_, bool>(0),
        )?
    {
        return release_blocker(db, job);
    }
    let current: Option<String> = db.query_row(
        "SELECT plan_revision FROM job_pipeline_runs WHERE project_id = ?1 AND id = ?2",
        params![job.project_id, origin.pipeline_run_id],
        |row| row.get(0),
    )?;
    let unplanned_goal = current.is_none() && job.mode != crate::providers::SessionMode::ApprovedWrite
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM goal_continuations g JOIN workflow_job_inputs w ON w.job_id = g.job_id WHERE g.job_id = ?1)",
            [&job.id], |row| row.get::<_, bool>(0),
        )?;
    if (!unplanned_goal && current.is_none()) || current != origin.plan_revision {
        return Ok(Some(ReleaseBlocker::StalePlan));
    }
    release_blocker(db, job)
}

fn pipeline_pin_is_valid(db: &Connection, job: &Job) -> Result<bool> {
    let pinned = load_binding(db, &job.id)?.is_some_and(|binding| binding.pipeline_pin);
    Ok(pinned && pinned_release_blocker(db, job)?.is_none())
}

pub(crate) fn apply_queue_history_policy(db: &Connection, conversation: &str) -> Result<()> {
    let (project, settings) = conversation_policy(db, conversation)?;
    let policy = settings
        .conversation_override
        .unwrap_or(settings.project_default);
    let pending = {
        let mut query = db.prepare(
            "SELECT j.id, coalesce(question.context_id, w.context_id, recovery.context_id, j.context_id),
                    question.context_id IS NOT NULL
             FROM jobs j
             LEFT JOIN job_recoveries recovery ON recovery.job_id = j.id
             LEFT JOIN conversation_contexts retry_context ON retry_context.id = recovery.context_id
             LEFT JOIN job_attempts latest ON latest.job_id = j.id AND latest.ordinal = (
                 SELECT max(ordinal) FROM job_attempts WHERE job_id = j.id
             )
             LEFT JOIN job_downstream_waits w ON w.parent_run_id = latest.run_id
                 AND w.state IN ('waiting', 'needs_review')
             LEFT JOIN conversation_contexts wait_context ON wait_context.id = w.context_id
             LEFT JOIN ready_question_continuations question ON question.job_id = j.id
             WHERE j.project_id = ?1 AND j.queue_state IN ('queued', 'paused')
               AND NOT EXISTS (SELECT 1 FROM borrowed_instruction_deliveries WHERE job_id = j.id)
               AND (j.conversation_id = ?2 OR retry_context.conversation_id = ?2 OR wait_context.conversation_id = ?2
                    OR question.conversation_id = ?2)
               AND (latest.run_id IS NULL OR recovery.job_id IS NOT NULL OR w.id IS NOT NULL OR question.job_id IS NOT NULL)
               AND NOT EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = latest.run_id)
             ORDER BY j.sequence",
        )?;
        query
            .query_map(params![project, conversation], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, context, answered_question) in pending {
        let job = load_job(db, &project, &id)?.context("Pending job disappeared")?;
        if pipeline_pin_is_valid(db, &job)? {
            continue;
        }
        apply_job_history_policy(db, &job, &context, answered_question, policy)?;
    }
    apply_parked_goal_history_policy(db, &project, conversation, policy)?;
    apply_saved_goal_input_history_policy(db, &project, conversation, policy)?;
    archive_superseded_questions(db, conversation)
}

fn apply_parked_goal_history_policy(
    db: &Connection,
    project: &str,
    conversation: &str,
    policy: QueueHistoryPolicy,
) -> Result<()> {
    let affected = {
        let mut query = db.prepare(
            "SELECT g.revision_id, controls.state, c.job_id,
                    coalesce(recovery.context_id, attempt.result_context_id, j.context_id)
             FROM active_goals g JOIN goals owner ON owner.id = g.goal_id
             JOIN goal_revision_controls controls ON controls.revision_id = g.revision_id
             JOIN goal_continuations c ON c.revision_id = g.revision_id
             JOIN jobs j ON j.id = c.job_id
             LEFT JOIN job_recoveries recovery ON recovery.job_id = j.id
             LEFT JOIN agent_attempts attempt ON attempt.run_id = controls.stop_run_id
             WHERE owner.project_id = ?1 AND owner.conversation_id = ?2
               AND controls.state IN ('paused','needs_review')
               AND c.checkpoint_ordinal = (SELECT max(next.checkpoint_ordinal)
                                            FROM goal_continuations next WHERE next.revision_id = c.revision_id)",
        )?;
        query
            .query_map(params![project, conversation], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (revision, state, id, context) in affected {
        let job = load_job(db, project, &id)?.context("History changed with a missing goal job")?;
        if pipeline_pin_is_valid(db, &job)? {
            continue;
        }
        if policy == QueueHistoryPolicy::Cancel {
            super::goal_control::cancel_parked_goal_for_history(
                db,
                &revision,
                state == "paused",
                None,
            )?;
        } else {
            bind_recorded_goal_context(db, &job.id, &context)?;
        }
    }
    Ok(())
}

fn bind_recorded_goal_context(db: &Connection, job: &str, context: &str) -> Result<()> {
    db.execute(
        "INSERT INTO job_history_bindings(job_id, context_id, pipeline_pin) VALUES (?1, ?2, 0)
         ON CONFLICT(job_id) DO UPDATE SET context_id = excluded.context_id WHERE pipeline_pin = 0",
        params![job, context],
    )?;
    Ok(())
}

fn apply_saved_goal_input_history_policy(
    db: &Connection,
    project: &str,
    conversation: &str,
    policy: QueueHistoryPolicy,
) -> Result<()> {
    let waiting = {
        let mut query = db.prepare(
            "SELECT c.revision_id, c.job_id, a.run_id, attempt.result_context_id,
                    (SELECT min(request_event_seq) FROM open_agent_questions WHERE run_id = a.run_id AND state = 'saved')
             FROM goal_continuations c JOIN active_goals active ON active.revision_id = c.revision_id
             JOIN goal_revisions r ON r.id = c.revision_id JOIN goals g ON g.id = r.goal_id
             JOIN goal_revision_controls control ON control.revision_id = c.revision_id
             JOIN job_attempts a ON a.job_id = c.job_id JOIN agent_attempts attempt ON attempt.run_id = a.run_id
             JOIN jobs j ON j.id = c.job_id
             WHERE g.project_id = ?1 AND attempt.conversation_id = ?2 AND r.continuation_owner = 'managed'
               AND control.state = 'active' AND j.queue_state IN ('queued','paused')
               AND NOT EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = a.run_id)
               AND NOT EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = c.job_id)
               AND NOT EXISTS (SELECT 1 FROM ready_question_continuations WHERE job_id = c.job_id)
               AND NOT EXISTS (SELECT 1 FROM job_downstream_waits WHERE parent_run_id = a.run_id
                               AND state IN ('waiting','needs_review'))
               AND EXISTS (SELECT 1 FROM open_agent_questions WHERE run_id = a.run_id AND state = 'saved')
               AND a.ordinal = (SELECT max(last.ordinal) FROM job_attempts last WHERE last.job_id = c.job_id)
               AND c.checkpoint_ordinal = (SELECT max(last.checkpoint_ordinal) FROM goal_continuations last
                                            WHERE last.revision_id = c.revision_id)",
        )?;
        query
            .query_map(params![project, conversation], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (revision, id, run, context, question) in waiting {
        let job = load_job(db, project, &id)?.context("The goal waiting for input lost its job")?;
        if pipeline_pin_is_valid(db, &job)? {
            continue;
        }
        if policy == QueueHistoryPolicy::Cancel {
            super::goal_control::cancel_parked_goal_for_history(db, &revision, true, Some(&run))?;
            continue;
        }
        if detached_question_context(db, conversation, &run, question)?.is_some() {
            continue;
        }
        bind_recorded_goal_context(db, &id, &context)?;
        let writer = job.mode == crate::providers::SessionMode::ApprovedWrite;
        if writer {
            let confirmed: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM job_worktree_candidates c JOIN job_worktree_starts s ON s.run_id = c.run_id
                 WHERE c.run_id = ?1 AND c.capture_confirmed AND s.checkout_id = ?2)",
                params![run, job.checkout_id], |row| row.get(0),
            )?;
            if !confirmed {
                super::goal_control::hold_goal_for_stop_review(db, &revision, &run)?;
                continue;
            }
        }
        db.execute(
            "INSERT INTO job_recoveries(job_id, source_run_id, action, context_id) VALUES (?1, ?2, 'resume', ?3)",
            params![id, run, context],
        )?;
        if writer {
            db.execute(
                "INSERT INTO goal_writer_recoveries(job_id, checkpoint_run_id) VALUES (?1, ?2)",
                params![id, run],
            )?;
        }
        apply_job_history_policy(db, &job, &context, false, policy)?;
        super::policy::reconcile_approval(db, &job)?;
    }
    Ok(())
}

pub(super) fn apply_job_history_policy(
    db: &Connection,
    job: &Job,
    context: &str,
    answered_question: bool,
    policy: QueueHistoryPolicy,
) -> Result<()> {
    if policy == QueueHistoryPolicy::Cancel {
        change_pending_job_state(db, &job.project_id, &job.id, QueueState::Cancelled)?;
    } else {
        db.execute(
            "INSERT INTO job_history_bindings(job_id, context_id, pipeline_pin) VALUES (?1, ?2, 0)
             ON CONFLICT(job_id) DO UPDATE SET context_id = excluded.context_id WHERE ?3 AND pipeline_pin = 0",
            params![job.id, context, answered_question],
        )?;
        if policy == QueueHistoryPolicy::Review {
            change_pending_job_state(db, &job.project_id, &job.id, QueueState::Paused)?;
        }
    }
    Ok(())
}

fn archive_superseded_questions(db: &Connection, conversation: &str) -> Result<()> {
    let questions = {
        let mut query = db.prepare(
            "SELECT q.request_event_seq, q.run_id FROM agent_questions q
             JOIN agent_attempts a ON a.run_id = q.run_id
             WHERE a.conversation_id = ?1 AND (
                 q.delivery IN ('pending', 'prepared') OR EXISTS (
                     SELECT 1 FROM native_question_waits w WHERE w.request_event_seq = q.request_event_seq
                         AND w.state IN ('saved', 'needs_review')
                 )
             )",
        )?;
        query
            .query_map([conversation], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (seq, run) in questions {
        if detached_question_context(db, conversation, &run, seq)?.is_some() {
            continue;
        }
        db.execute(
            "UPDATE agent_questions
             SET delivery = CASE delivery WHEN 'prepared' THEN 'unconfirmed' ELSE 'closed' END
             WHERE request_event_seq = ?1 AND delivery IN ('pending', 'prepared')",
            [seq],
        )?;
        super::question_waits::archive_saved_question(db, seq)?;
    }
    Ok(())
}

pub(crate) fn detached_question_context(
    db: &Connection,
    conversation: &str,
    run: &str,
    seq: i64,
) -> Result<Option<String>> {
    let checkpoint: Option<(String, bool)> = db.query_row(
        "SELECT l.context_id, a.detached_context FROM structured_question_continuations l
         JOIN agent_questions q ON q.request_event_seq = l.request_event_seq
         JOIN agent_attempts a ON a.run_id = q.run_id
         WHERE q.request_event_seq = ?1 AND a.run_id = ?2 AND a.conversation_id = ?3
           AND (q.delivery = 'pending' OR EXISTS (
                 SELECT 1 FROM native_question_waits w WHERE w.request_event_seq = q.request_event_seq
                     AND w.state IN ('saved', 'needs_review')
           )) AND l.continuation_run_id IS NULL AND l.context_id = a.result_context_id",
        params![seq, run, conversation],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((context, detached)) = checkpoint else {
        return Ok(None);
    };
    let Some(origin) = structured_job_run(db, run)? else {
        return Ok(detached.then_some(context));
    };
    let job = load_attempt_job(db, &origin)?.context("Saved question lost its job")?;
    if job.queue_state == QueueState::Cancelled {
        return Ok(None);
    }
    let pinned = load_binding(db, &job.id)?.is_some_and(|binding| binding.pipeline_pin);
    let recorded_answer: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM job_history_bindings h JOIN ready_question_continuations q
         ON q.job_id = h.job_id AND q.context_id = h.context_id
         WHERE h.pipeline_pin = 0 AND q.run_id = ?1 AND q.context_id = ?2
         AND EXISTS (SELECT 1 FROM recorded_question_answers WHERE request_event_seq = ?3))",
        params![run, context, seq],
        |r| r.get(0),
    )?;
    Ok(
        ((pinned || detached || recorded_answer) && attempt_requirements(db, &origin)?.is_ok())
            .then_some(context),
    )
}

pub(super) fn structured_job_run(db: &Connection, run: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "WITH RECURSIVE originating_runs(run_id) AS (
             SELECT ?1
             UNION
             SELECT q.run_id FROM originating_runs a
             JOIN structured_question_continuations l ON l.continuation_run_id = a.run_id
             JOIN agent_questions q ON q.request_event_seq = l.request_event_seq
         )
         SELECT j.run_id FROM originating_runs a JOIN job_attempts j ON j.run_id = a.run_id",
            [run],
            |row| row.get(0),
        )
        .optional()?)
}
