use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{
    Job, RecoveryAction, load_job, now_ms, question_waits::has_open_questions,
    recovery::recovery_source_for_goal_resume,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalControlState {
    Active,
    Pausing,
    Paused,
    Cancelling,
    Cancelled,
    NeedsReview,
}

impl GoalControlState {
    fn key(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pausing => "pausing",
            Self::Paused => "paused",
            Self::Cancelling => "cancelling",
            Self::Cancelled => "cancelled",
            Self::NeedsReview => "needs_review",
        }
    }

    pub(super) fn from_key(key: &str) -> Result<Self> {
        Ok(match key {
            "active" => Self::Active,
            "pausing" => Self::Pausing,
            "paused" => Self::Paused,
            "cancelling" => Self::Cancelling,
            "cancelled" => Self::Cancelled,
            "needs_review" => Self::NeedsReview,
            _ => anyhow::bail!("Unknown goal control state"),
        })
    }

    pub(super) fn blocker(self) -> Option<&'static str> {
        match self {
            Self::Active => None,
            Self::Pausing => Some("Waiting for the goal to pause"),
            Self::Paused => Some("The goal is paused"),
            Self::Cancelling => Some("Waiting for the goal to stop"),
            Self::Cancelled => Some("The goal was cancelled"),
            Self::NeedsReview => Some("The previous goal attempt needs stop confirmation"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStopAction {
    Pause,
    Cancel,
}

impl GoalStopAction {
    fn key(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Cancel => "cancel",
        }
    }

    fn requested(self) -> GoalControlState {
        match self {
            Self::Pause => GoalControlState::Pausing,
            Self::Cancel => GoalControlState::Cancelling,
        }
    }

    fn settled(self) -> GoalControlState {
        match self {
            Self::Pause => GoalControlState::Paused,
            Self::Cancel => GoalControlState::Cancelled,
        }
    }
}

impl Store {
    pub async fn request_goal_stop(
        &self,
        project: String,
        revision: String,
        action: GoalStopAction,
    ) -> Result<GoalControlState> {
        let state = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (current, owner, previously_stopping): (String, String, Option<String>) = tx.query_row(
                "SELECT controls.state, revision.continuation_owner, controls.stop_run_id
                 FROM goal_revision_controls controls JOIN goal_revisions revision ON revision.id = controls.revision_id
                 JOIN goals g ON g.id = revision.goal_id LEFT JOIN active_goals active ON active.revision_id = revision.id
                 WHERE controls.revision_id = ?1 AND g.project_id = ?2
                   AND (active.revision_id IS NOT NULL OR controls.state = 'cancelled')",
                params![revision, project], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional()?.context("The goal revision is no longer active in this project")?;
            let current = GoalControlState::from_key(&current)?;
            if current == action.requested() || current == action.settled() {
                return Ok(current);
            }
            ensure!(
                current == GoalControlState::Active
                    || (action == GoalStopAction::Cancel && matches!(current, GoalControlState::Paused | GoalControlState::Pausing)),
                "The previous goal stop still needs reconciliation"
            );
            let mut runs = tx.prepare(
                "SELECT a.run_id FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
                 JOIN active_agent_attempts active ON active.run_id = a.run_id WHERE c.revision_id = ?1",
            )?;
            let live = runs.query_map([&revision], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(runs);
            ensure!(live.len() <= 1, "The goal has multiple live executors");
            let run = live.first();
            let state = if run.is_some() {
                action.requested()
            } else if has_unsettled_reservation(&tx, &revision)?
                || (owner == "native" && has_launched_executor(&tx, &revision)?)
                || previously_stopping.is_some_and(|_| current == GoalControlState::Pausing)
            {
                GoalControlState::NeedsReview
            } else {
                action.settled()
            };
            let now = now_ms();
            tx.execute(
                "UPDATE goal_revision_controls SET state = ?2, stop_action = ?3,
                        stop_run_id = CASE WHEN ?2 = 'needs_review' THEN coalesce(?4, stop_run_id) ELSE ?4 END,
                        requested_ms = ?5, settled_ms = ?6,
                        confirmed_by = CASE WHEN ?6 IS NOT NULL THEN 'recorded_idle' END WHERE revision_id = ?1",
                params![revision, state.key(), action.key(), run, now,
                    (state == action.settled()).then_some(now)],
            )?;
            if let Some(run) = run {
                tx.execute("UPDATE job_attempts SET cancel_requested = 1 WHERE run_id = ?1", [run])?;
            }
            if state == GoalControlState::Cancelled {
                release_goal(&tx, &revision)?;
            }
            tx.commit()?;
            Ok(state)
        }).await?;
        self.job_signal.send_replace(());
        Ok(state)
    }

    pub async fn resume_goal(&self, project: String, revision: String) -> Result<()> {
        let recovery = self
            .execute({
                let (project, revision) = (project.clone(), revision.clone());
                move |db| {
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let (job, stopped_run) = paused_job(&tx, &project, &revision)?;
                    let mut recovery = None;
                    if let Some(run) = stopped_run
                        && job.downstream_wait.is_none()
                        && !has_open_questions(&tx, &run)?
                        && job.queued_recovery.is_none()
                    {
                        let source = recovery_source_for_goal_resume(&tx, &job, &revision)?
                            .filter(|source| source.run_id() == run)
                            .context("The paused goal no longer has a recoverable checkpoint")?;
                        let action =
                            if writer_pause_checkpoint(&tx, &job, &revision, &run)? {
                                RecoveryAction::Resume
                            } else {
                                source.actions(job.mode).into_iter().last().context(
                                    "The paused goal needs a confirmed recovery checkpoint",
                                )?
                            };
                        recovery = Some((job.id.clone(), run, action));
                    }
                    if recovery.is_none() {
                        activate_paused_goal(&tx, &project, &revision, &job.id)?;
                    }
                    tx.commit()?;
                    Ok(recovery)
                }
            })
            .await?;
        if let Some((job, run, action)) = recovery {
            self.queue_job_recovery_with_goal(project, job, run, action, Some(revision))
                .await?;
        }
        self.job_signal.send_replace(());
        Ok(())
    }

    /// Records the operator's review of a managed executor whose stop could not be observed.
    pub async fn confirm_managed_goal_stop(
        &self,
        project: String,
        revision: String,
        run: String,
    ) -> Result<GoalControlState> {
        let state = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let action: String = tx.query_row(
                "SELECT controls.stop_action FROM goal_revision_controls controls
                 JOIN goal_revisions r ON r.id = controls.revision_id JOIN goals g ON g.id = r.goal_id
                 JOIN active_goals active ON active.revision_id = r.id
                 WHERE controls.revision_id = ?1 AND g.project_id = ?2 AND controls.stop_run_id = ?3
                   AND controls.state = 'needs_review' AND r.continuation_owner = 'managed'
                   AND NOT EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = ?3)",
                params![revision, project, run], |r| r.get(0),
            ).optional()?.context("This managed goal stop no longer awaits review")?;
            ensure!(!has_unsettled_reservation(&tx, &revision)?, "The previous executor still holds a reservation");
            let state = settled_state(&action);
            db_set_stop_outcome(&tx, &revision, state, Some("operator"))?;
            if state == GoalControlState::Cancelled {
                release_goal(&tx, &revision)?;
            }
            tx.commit()?;
            Ok(state)
        }).await?;
        self.job_signal.send_replace(());
        Ok(state)
    }

    pub(crate) async fn goal_pause_requests_input_retention(&self, run: String) -> Result<bool> {
        self.execute(move |db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM goal_revision_controls WHERE stop_run_id = ?1 AND state = 'pausing')",
                [run], |r| r.get(0),
            )?)
        }).await
    }
}

pub(super) fn worker_confirmed_goal_pause(
    db: &Connection,
    job: &Job,
    revision: &str,
    run: &str,
) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM goal_revision_controls controls JOIN goal_continuations c ON c.revision_id = controls.revision_id
            JOIN active_goals active ON active.revision_id = controls.revision_id
            JOIN goal_revisions goal ON goal.id = active.revision_id
            JOIN job_attempts attempt ON attempt.job_id = c.job_id AND attempt.run_id = controls.stop_run_id
            WHERE controls.revision_id = ?1 AND c.job_id = ?2 AND attempt.run_id = ?3
              AND controls.state = 'paused' AND controls.stop_action = 'pause'
              AND controls.confirmed_by = 'worker' AND goal.continuation_owner = 'managed'
         )",
        params![revision, job.id, run], |row| row.get(0),
    )?)
}

pub(super) fn writer_pause_checkpoint(
    db: &Connection,
    job: &Job,
    revision: &str,
    run: &str,
) -> Result<bool> {
    if job.mode != crate::providers::SessionMode::ApprovedWrite
        || !worker_confirmed_goal_pause(db, job, revision, run)?
    {
        return Ok(false);
    }
    Ok(db.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM job_worktree_candidates candidate JOIN job_worktree_starts start ON start.run_id = candidate.run_id
            WHERE candidate.run_id = ?1 AND start.checkout_id = ?2 AND candidate.capture_confirmed
         )",
        params![run, job.checkout_id], |row| row.get(0),
    )?)
}

fn paused_job(db: &Connection, project: &str, revision: &str) -> Result<(Job, Option<String>)> {
    let (id, stopped): (String, Option<String>) = db
        .query_row(
            "SELECT c.job_id, controls.stop_run_id FROM goal_revision_controls controls
         JOIN goal_revisions r ON r.id = controls.revision_id JOIN goals g ON g.id = r.goal_id
         JOIN active_goals active ON active.revision_id = r.id
         JOIN goal_continuations c ON c.revision_id = r.id
         WHERE controls.revision_id = ?1 AND g.project_id = ?2 AND controls.state = 'paused'
           AND r.continuation_owner = 'managed' ORDER BY c.checkpoint_ordinal DESC LIMIT 1",
            params![revision, project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .context("The managed goal is no longer paused in this project")?;
    ensure!(
        !has_unsettled_reservation(db, revision)?,
        "The previous executor still holds a reservation"
    );
    let job = load_job(db, project, &id)?.context("The paused goal lost its latest job")?;
    Ok((job, stopped))
}

pub(super) fn activate_paused_goal(
    db: &Connection,
    project: &str,
    revision: &str,
    job: &str,
) -> Result<()> {
    let (current, _) = paused_job(db, project, revision)?;
    ensure!(current.id == job, "The goal has a newer continuation");
    let changed = db.execute(
        "UPDATE goal_revision_controls SET state = 'active' WHERE revision_id = ?1 AND state = 'paused'",
        [revision],
    )?;
    ensure!(changed == 1, "The goal was already resumed or cancelled");
    Ok(())
}

pub(crate) fn settle_goal_stop(
    db: &Connection,
    run: &str,
    quiescence_confirmed: bool,
) -> Result<()> {
    let pending: Option<(String, String, String, bool)> = db.query_row(
        "SELECT controls.revision_id, controls.stop_action, r.continuation_owner, a.process_launch_requested
         FROM goal_revision_controls controls JOIN goal_revisions r ON r.id = controls.revision_id
         JOIN job_attempts a ON a.run_id = controls.stop_run_id
         WHERE controls.stop_run_id = ?1 AND controls.state IN ('pausing','cancelling')",
        [run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional()?;
    let Some((revision, action, owner, launched)) = pending else {
        if !quiescence_confirmed {
            db.execute(
                "UPDATE goal_revision_controls SET state = 'needs_review', stop_action = 'pause',
                    stop_run_id = ?1, requested_ms = ?2, settled_ms = NULL, confirmed_by = NULL
                 WHERE state = 'active' AND revision_id IN (
                     SELECT c.revision_id FROM job_attempts a JOIN goal_continuations c ON c.job_id = a.job_id
                     JOIN active_goals active ON active.revision_id = c.revision_id
                     WHERE a.run_id = ?1 AND a.process_launch_requested
                 )",
                params![run, now_ms()],
            )?;
        }
        return Ok(());
    };
    let stopped = !launched
        || (owner == "managed"
            && quiescence_confirmed
            && !has_unsettled_reservation(db, &revision)?);
    let state = if stopped {
        settled_state(&action)
    } else {
        GoalControlState::NeedsReview
    };
    db_set_stop_outcome(
        db,
        &revision,
        state,
        stopped.then_some(if launched { "worker" } else { "unlaunched" }),
    )?;
    if state == GoalControlState::Cancelled {
        release_goal(db, &revision)?;
    }
    Ok(())
}

fn settled_state(action: &str) -> GoalControlState {
    if action == "pause" {
        GoalControlState::Paused
    } else {
        GoalControlState::Cancelled
    }
}

fn db_set_stop_outcome(
    db: &Connection,
    revision: &str,
    state: GoalControlState,
    proof: Option<&str>,
) -> Result<()> {
    db.execute(
        "UPDATE goal_revision_controls SET state = ?2, settled_ms = ?3, confirmed_by = ?4 WHERE revision_id = ?1",
        params![revision, state.key(), proof.map(|_| now_ms()), proof],
    )?;
    Ok(())
}

pub(super) fn cancel_parked_goal_for_history(
    db: &Connection,
    revision: &str,
    stop_confirmed: bool,
    run: Option<&str>,
) -> Result<()> {
    let settled = stop_confirmed && !has_unsettled_reservation(db, revision)?;
    db.execute(
        "UPDATE goal_revision_controls SET stop_action = 'cancel', requested_ms = ?2,
             stop_run_id = coalesce(?3, stop_run_id) WHERE revision_id = ?1",
        params![revision, now_ms(), run],
    )?;
    if settled {
        db_set_stop_outcome(
            db,
            revision,
            GoalControlState::Cancelled,
            Some("recorded_idle"),
        )?;
        release_goal(db, revision)?;
    } else {
        db_set_stop_outcome(db, revision, GoalControlState::NeedsReview, None)?;
    }
    Ok(())
}

fn has_unsettled_reservation(db: &Connection, revision: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
            WHERE c.revision_id = ?1 AND (
                EXISTS (SELECT 1 FROM job_writer_reservations WHERE run_id = a.run_id)
                OR EXISTS (SELECT 1 FROM unconfirmed_question_stops WHERE run_id = a.run_id)
            )
         )",
        [revision],
        |row| row.get(0),
    )?)
}

pub(super) fn hold_goal_for_stop_review(db: &Connection, revision: &str, run: &str) -> Result<()> {
    db.execute(
        "UPDATE goal_revision_controls SET stop_action = 'pause', requested_ms = ?2,
             stop_run_id = ?3 WHERE revision_id = ?1",
        params![revision, now_ms(), run],
    )?;
    db_set_stop_outcome(db, revision, GoalControlState::NeedsReview, None)
}

fn has_launched_executor(db: &Connection, revision: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
                       WHERE c.revision_id = ?1 AND a.process_launch_requested)",
        [revision], |row| row.get(0),
    )?)
}

fn release_goal(db: &Connection, revision: &str) -> Result<()> {
    let questions = {
        let mut query = db.prepare(
            "SELECT q.request_event_seq FROM open_agent_questions q JOIN job_attempts a ON a.run_id = q.run_id
             JOIN goal_continuations c ON c.job_id = a.job_id WHERE c.revision_id = ?1",
        )?;
        query
            .query_map([revision], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?
    };
    let cancelled = {
        let mut statement = db.prepare(
        "UPDATE jobs SET queue_state = 'cancelled'
         WHERE id IN (SELECT job_id FROM goal_continuations WHERE revision_id = ?1)
           AND queue_state <> 'cancelled'
           AND (
               NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = jobs.id)
               OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = jobs.id)
               OR EXISTS (SELECT 1 FROM ready_question_continuations WHERE job_id = jobs.id)
               OR EXISTS (SELECT 1 FROM job_attempts a WHERE a.job_id = jobs.id
                   AND a.ordinal = (SELECT max(ordinal) FROM job_attempts WHERE job_id = jobs.id) AND (
                   a.cancel_requested = 1
                   OR EXISTS (SELECT 1 FROM open_agent_questions WHERE run_id = a.run_id)
                   OR EXISTS (SELECT 1 FROM job_downstream_waits WHERE parent_run_id = a.run_id
                              AND state IN ('yielding','waiting','needs_review'))
               ))
           ) RETURNING project_id, id",
        )?;
        statement
            .query_map([revision], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    db.execute(
        "DELETE FROM job_recoveries WHERE job_id IN (SELECT job_id FROM goal_continuations WHERE revision_id = ?1)",
        [revision],
    )?;
    db.execute(
        "UPDATE job_downstream_waits SET state = 'cancelled' WHERE state IN ('yielding','waiting','needs_review')
         AND parent_run_id IN (
             SELECT a.run_id FROM job_attempts a JOIN goal_continuations c ON c.job_id = a.job_id WHERE c.revision_id = ?1
         )",
        [revision],
    )?;
    for question in questions {
        db.execute(
            "UPDATE agent_questions SET delivery = CASE delivery WHEN 'prepared' THEN 'unconfirmed' ELSE 'closed' END
             WHERE request_event_seq = ?1 AND delivery IN ('pending', 'prepared')",
            [question],
        )?;
        super::archive_saved_question(db, question)?;
    }
    db.execute(
        "DELETE FROM active_goals WHERE revision_id = ?1",
        [revision],
    )?;
    for (project, id) in cancelled {
        let job = load_job(db, &project, &id)?.context("The cancelled goal lost its job")?;
        super::downstream::observe_cancelled_job(db, &job)?;
    }
    Ok(())
}
