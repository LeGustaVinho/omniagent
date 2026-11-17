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
    pub(crate) async fn pause_goal_if_policy_changed(&self, run: String) -> Result<bool> {
        let paused = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let paused = pause_revoked_goal(&tx, &run)?;
                tx.commit()?;
                Ok(paused)
            })
            .await?;
        if paused {
            self.job_signal.send_replace(());
        }
        Ok(paused)
    }

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
                tx.execute(
                    "UPDATE goal_revision_controls SET resume_after_wait = 0 WHERE revision_id = ?1",
                    [&revision],
                )?;
                tx.commit()?;
                return Ok(current);
            }
            ensure!(
                current == GoalControlState::Active
                    || (action == GoalStopAction::Cancel && matches!(current, GoalControlState::Paused | GoalControlState::Pausing)),
                "The previous goal stop still needs reconciliation"
            );
            if action == GoalStopAction::Cancel {
                stop_owned_children(&tx, &revision)?;
            }
            let mut runs = tx.prepare(
                "SELECT a.run_id FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
                 JOIN active_agent_attempts active ON active.run_id = a.run_id WHERE c.revision_id = ?1",
            )?;
            let live = runs.query_map([&revision], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(runs);
            ensure!(live.len() <= 1, "The goal has multiple live executors");
            let run = live.first();
            let mut state = if run.is_some() {
                action.requested()
            } else if has_unsettled_reservation(&tx, &revision)?
                || (owner == "native" && current != GoalControlState::Paused
                    && !(action == GoalStopAction::Cancel && native_result_is_settled(&tx, &revision)?)
                    && has_launched_executor(&tx, &revision)?)
                || previously_stopping.is_some_and(|_| current == GoalControlState::Pausing)
            {
                GoalControlState::NeedsReview
            } else {
                action.settled()
            };
            let root_settled = state == action.settled();
            if state == GoalControlState::Cancelled && owned_children_unsettled(&tx, &revision)? {
                state = GoalControlState::Cancelling;
            }
            let now = now_ms();
            tx.execute(
                "UPDATE goal_revision_controls SET state = ?2, stop_action = ?3,
                        stop_run_id = CASE WHEN ?2 = 'cancelling' THEN coalesce(?4, stop_run_id,
                            (SELECT a.run_id FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
                             WHERE c.revision_id = ?1 ORDER BY c.checkpoint_ordinal DESC, a.ordinal DESC LIMIT 1))
                            WHEN ?2 = 'needs_review' THEN coalesce(?4, stop_run_id) ELSE ?4 END,
                        requested_ms = ?5, settled_ms = ?6, resume_after_wait = 0,
                        stop_event_floor = coalesce((SELECT max(seq) FROM agent_events WHERE run_id = ?4), 0),
                        confirmed_by = CASE WHEN ?6 IS NOT NULL THEN 'recorded_idle' END WHERE revision_id = ?1",
                params![revision, state.key(), action.key(), run, now,
                    root_settled.then_some(now)],
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
        self.resume_goal_with_review(project, revision, None).await
    }

    pub async fn resume_goal_after_review(
        &self,
        project: String,
        revision: String,
        review_token: String,
    ) -> Result<()> {
        self.resume_goal_with_review(project, revision, Some(review_token))
            .await
    }

    async fn resume_goal_with_review(
        &self,
        project: String,
        revision: String,
        review_token: Option<String>,
    ) -> Result<()> {
        let recovery = self
            .execute({
                let (project, revision) = (project.clone(), revision.clone());
                let checked = review_token.clone();
                move |db| {
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    if let Some(token) = checked.as_deref() {
                        super::goal_transition::validate_goal_resume_review(
                            &tx, &project, &revision, token,
                        )?;
                    }
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
            self.queue_job_recovery_with_goal(
                project,
                job,
                run,
                action,
                Some(revision),
                review_token,
            )
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
            let state = settle_confirmed_goal_stop(&tx, &revision, &action, "operator")?;
            tx.commit()?;
            Ok(state)
        }).await?;
        self.job_signal.send_replace(());
        Ok(state)
    }

    pub async fn confirm_native_goal_stop(
        &self,
        project: String,
        revision: String,
        run: String,
    ) -> Result<GoalControlState> {
        let capture = self.execute({
            let (project, revision, run) = (project.clone(), revision.clone(), run.clone());
            move |db| {
                native_stop_review(db, &project, &revision, &run)?;
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM job_writer_reservations WHERE run_id = ?1)
                     AND NOT EXISTS(SELECT 1 FROM job_worktree_candidates WHERE run_id = ?1 AND capture_confirmed)",
                    [&run], |row| row.get::<_, bool>(0),
                )?)
            }
        }).await?;
        if capture {
            crate::repository::save_reconciled_native_candidate(self, run.clone()).await?;
        }
        let state = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let action = native_stop_review(&tx, &project, &revision, &run)?;
                super::release_settled_writer(&tx, &run)?;
                ensure!(
                    !has_unsettled_reservation(&tx, &revision)?,
                    "The native goal still needs an exact worktree or runner checkpoint"
                );
                let state = settle_confirmed_goal_stop(&tx, &revision, &action, "operator")?;
                tx.commit()?;
                Ok(state)
            })
            .await?;
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

    pub(crate) async fn native_goal_stop_action(
        &self,
        run: String,
        for_wait: bool,
    ) -> Result<GoalStopAction> {
        let action = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (revision, state, action): (String, String, Option<String>) = tx.query_row(
                    "SELECT g.revision_id, controls.state,
                        CASE WHEN controls.stop_run_id = ?1 AND controls.state IN ('pausing', 'cancelling')
                             THEN controls.stop_action END
                     FROM native_goal_executors x
                     JOIN active_goals g ON g.revision_id = x.revision_id
                     JOIN goal_revision_controls controls ON controls.revision_id = x.revision_id
                     WHERE x.run_id = ?1 AND NOT x.stop_confirmed",
                    [&run], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            let action = match action.as_deref() {
                Some("cancel") => GoalStopAction::Cancel,
                Some("pause") => GoalStopAction::Pause,
                _ => {
                    ensure!(state == "active", "The native goal already has a stop boundary");
                    stage_runtime_goal_pause(&tx, &revision, &run, for_wait)?;
                    GoalStopAction::Pause
                }
            };
            tx.commit()?;
            Ok(action)
        }).await?;
        self.job_signal.send_replace(());
        Ok(action)
    }
}

fn native_stop_review(db: &Connection, project: &str, revision: &str, run: &str) -> Result<String> {
    let review: Option<(String, bool)> = db
        .query_row(
            "SELECT controls.stop_action, x.stop_confirmed FROM goal_revision_controls controls
         JOIN goal_revisions r ON r.id = controls.revision_id JOIN goals g ON g.id = r.goal_id
         JOIN active_goals active ON active.revision_id = r.id
         JOIN native_goal_executors x ON x.revision_id = r.id AND x.run_id = controls.stop_run_id
         WHERE controls.revision_id = ?1 AND g.project_id = ?2 AND x.run_id = ?3
           AND controls.state = 'needs_review' AND r.continuation_owner = 'native'
           AND NOT EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = ?3)",
            params![revision, project, run],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (action, stopped) = review.context("This native goal stop no longer awaits review")?;
    ensure!(
        stopped,
        "The runner has not confirmed that the previous native executor stopped"
    );
    Ok(action)
}

pub(crate) fn pause_revoked_goal(db: &Connection, run: &str) -> Result<bool> {
    let revision: Option<String> = db
        .query_row(
            "SELECT c.revision_id FROM active_agent_attempts a JOIN job_attempts j ON j.run_id = a.run_id
             JOIN goal_continuations c ON c.job_id = j.job_id
             JOIN active_goals g ON g.revision_id = c.revision_id
             JOIN goal_revision_controls controls ON controls.revision_id = c.revision_id
             WHERE a.run_id = ?1 AND controls.state = 'active' AND NOT j.cancel_requested",
            [run],
            |row| row.get(0),
        )
        .optional()?;
    let Some(revision) = revision else {
        return Ok(false);
    };
    if super::policy::validate_attempt_policy(db, run)? {
        return Ok(false);
    }
    stage_runtime_goal_pause(db, &revision, run, false)?;
    Ok(true)
}

fn stage_runtime_goal_pause(
    db: &Connection,
    revision: &str,
    run: &str,
    for_wait: bool,
) -> Result<()> {
    db.execute(
        "UPDATE goal_revision_controls SET state = 'pausing', stop_action = 'pause',
            stop_run_id = ?2, requested_ms = ?3, settled_ms = NULL, confirmed_by = NULL,
            resume_after_wait = ?4,
            stop_event_floor = coalesce((SELECT max(seq) FROM agent_events WHERE run_id = ?2), 0)
         WHERE revision_id = ?1",
        params![revision, run, now_ms(), for_wait],
    )?;
    db.execute(
        "UPDATE job_attempts SET cancel_requested = 1 WHERE run_id = ?1",
        [run],
    )?;
    Ok(())
}

pub(super) fn confirmed_goal_pause(
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
              AND controls.confirmed_by IN ('worker', 'operator')
              AND (goal.continuation_owner = 'managed' OR EXISTS (
                  SELECT 1 FROM native_goal_executors x
                  WHERE x.run_id = attempt.run_id AND x.revision_id = controls.revision_id AND x.stop_confirmed
              ))
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
        || !confirmed_goal_pause(db, job, revision, run)?
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

pub(super) fn paused_job(
    db: &Connection,
    project: &str,
    revision: &str,
) -> Result<(Job, Option<String>)> {
    let (id, stopped): (String, Option<String>) = db
        .query_row(
            "SELECT c.job_id, controls.stop_run_id FROM goal_revision_controls controls
         JOIN goal_revisions r ON r.id = controls.revision_id JOIN goals g ON g.id = r.goal_id
         JOIN active_goals active ON active.revision_id = r.id
         JOIN goal_continuations c ON c.revision_id = r.id
         WHERE controls.revision_id = ?1 AND g.project_id = ?2 AND controls.state = 'paused'
           AND (r.continuation_owner = 'managed' OR EXISTS(
               SELECT 1 FROM native_goal_executors x
               WHERE x.revision_id = r.id AND x.run_id = controls.stop_run_id AND x.stop_confirmed
           )) ORDER BY c.checkpoint_ordinal DESC LIMIT 1",
            params![revision, project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .context("The goal has no confirmed pause in this project")?;
    ensure!(
        !has_unsettled_reservation(db, revision)?,
        "The previous executor still holds a reservation"
    );
    ensure!(
        !owned_children_unsettled(db, revision)?,
        "Delegated work is still stopping"
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
    let (current, stopped_run) = paused_job(db, project, revision)?;
    ensure!(current.id == job, "The goal has a newer continuation");
    ensure!(
        super::history::effective_release_blocker(db, &current)?.is_none(),
        "The workflow goal needs current approval for this continuation scope"
    );
    let (owner, deadline, definition): (String, Option<i64>, Vec<u8>) = db.query_row(
        "SELECT continuation_owner, deadline_ms, definition FROM goal_revisions WHERE id = ?1",
        [revision],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    ensure!(
        deadline.is_none_or(|deadline| now_ms() < deadline),
        "The goal reached its time limit"
    );
    if owner == "native" {
        let run = stopped_run.context("The previous native executor has no stop evidence")?;
        ensure!(
            confirmed_goal_pause(db, &current, revision, &run)?,
            "The previous native executor has not been reconciled"
        );
        let definition: super::GoalDefinition =
            serde_json::from_slice(&crate::protection::unprotect(&definition)?)?;
        let remaining = if let Some(budget) = definition.token_budget {
            let remaining = super::native_goal_evidence::paused_native_budget(db, &run, budget)?
                .context("The previous native goal's token usage could not be verified")?;
            ensure!(remaining > 0, "The goal reached its token budget");
            Some(remaining)
        } else {
            None
        };
        pin_native_resume(db, revision, job, &run, remaining)?;
    }
    activate_goal(db, revision)
}

fn pin_native_resume(
    db: &Connection,
    revision: &str,
    job: &str,
    run: &str,
    budget: Option<u64>,
) -> Result<()> {
    db.execute(
        "INSERT INTO native_goal_resume_checkpoints VALUES (?1, ?2, ?3)
         ON CONFLICT(job_id) DO UPDATE SET source_run_id = excluded.source_run_id, remaining_tokens = excluded.remaining_tokens",
        params![job, run, budget.map(i64::try_from).transpose()?],
    )?;
    db.execute(
        "UPDATE goal_revisions SET continuation_owner = 'managed' WHERE id = ?1",
        [revision],
    )?;
    Ok(())
}

fn activate_goal(db: &Connection, revision: &str) -> Result<()> {
    let changed = db.execute(
        "UPDATE goal_revision_controls SET state = 'active', resume_after_wait = 0
         WHERE revision_id = ?1 AND state = 'paused'",
        [revision],
    )?;
    ensure!(changed == 1, "The goal was already resumed or cancelled");
    Ok(())
}

fn resume_after_confirmed_wait(db: &Connection, revision: &str, run: &str) -> Result<()> {
    let boundary: Option<(String, Vec<u8>)> = db.query_row(
        "SELECT a.job_id, r.definition FROM job_attempts a
         JOIN goal_revisions r ON r.initial_job_id = a.job_id
         JOIN goal_revision_controls c ON c.revision_id = r.id
         WHERE a.run_id = ?1 AND r.id = ?2 AND r.continuation_owner = 'native'
           AND c.state = 'paused' AND c.resume_after_wait AND c.confirmed_by = 'worker'
           AND (r.deadline_ms IS NULL OR r.deadline_ms > ?3)
           AND (EXISTS (SELECT 1 FROM job_downstream_waits w WHERE w.parent_run_id = a.run_id AND w.state = 'waiting')
              OR EXISTS (SELECT 1 FROM open_agent_questions q WHERE q.run_id = a.run_id AND q.state = 'saved'))
           AND (NOT EXISTS (SELECT 1 FROM job_worktree_starts w WHERE w.run_id = a.run_id)
              OR EXISTS (SELECT 1 FROM job_worktree_candidates w WHERE w.run_id = a.run_id AND w.capture_confirmed))",
        params![run, revision, now_ms()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((job, definition)) = boundary else {
        return Ok(());
    };
    if super::policy::attempt_requirements(db, run)?.is_err() {
        return Ok(());
    }
    let definition: super::GoalDefinition =
        serde_json::from_slice(&crate::protection::unprotect(&definition)?)?;
    let remaining = if let Some(budget) = definition.token_budget {
        let Some(remaining) = super::native_goal_evidence::paused_native_budget(db, run, budget)?
            .filter(|remaining| *remaining > 0)
        else {
            return Ok(());
        };
        Some(remaining)
    } else {
        None
    };
    pin_native_resume(db, revision, &job, run, remaining)?;
    activate_goal(db, revision)
}

pub(crate) fn settle_goal_stop(
    db: &Connection,
    run: &str,
    quiescence_confirmed: bool,
) -> Result<()> {
    if !quiescence_confirmed && super::goals::goal_owns_run(db, run)? {
        db.execute(
            "INSERT INTO unconfirmed_managed_goal_stops SELECT a.run_id FROM job_attempts a
             WHERE a.run_id = ?1 AND a.process_launch_requested
               AND NOT EXISTS (SELECT 1 FROM native_goal_executors x WHERE x.run_id = a.run_id)
             ON CONFLICT(run_id) DO NOTHING",
            [run],
        )?;
    }
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
    let provider_stopped = owner == "managed"
        || super::native_goal_evidence::confirmed_stop_control(db, run, &revision, &action)?;
    let stopped = !launched
        || (quiescence_confirmed && provider_stopped && !has_unsettled_reservation(db, &revision)?);
    if stopped {
        settle_confirmed_goal_stop(
            db,
            &revision,
            &action,
            if launched { "worker" } else { "unlaunched" },
        )?;
        if owner == "native" && action == "pause" {
            resume_after_confirmed_wait(db, &revision, run)?;
        }
    } else {
        db_set_stop_outcome(db, &revision, GoalControlState::NeedsReview, None)?;
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

fn settle_confirmed_goal_stop(
    db: &Connection,
    revision: &str,
    action: &str,
    proof: &str,
) -> Result<GoalControlState> {
    let mut state = settled_state(action);
    if state == GoalControlState::Cancelled && owned_children_unsettled(db, revision)? {
        state = GoalControlState::Cancelling;
    }
    db_set_stop_outcome(db, revision, state, Some(proof))?;
    if state == GoalControlState::Cancelled {
        release_goal(db, revision)?;
    }
    Ok(state)
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
    stop_owned_children(db, revision)?;
    let settled = stop_confirmed && !has_unsettled_reservation(db, revision)?;
    db.execute(
        "UPDATE goal_revision_controls SET stop_action = 'cancel', requested_ms = ?2,
             stop_run_id = coalesce(?3, stop_run_id,
                (SELECT a.run_id FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
                 WHERE c.revision_id = ?1 ORDER BY c.checkpoint_ordinal DESC, a.ordinal DESC LIMIT 1))
             WHERE revision_id = ?1",
        params![revision, now_ms(), run],
    )?;
    if settled {
        settle_confirmed_goal_stop(db, revision, "cancel", "recorded_idle")?;
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
                OR EXISTS (SELECT 1 FROM unconfirmed_agent_stops WHERE run_id = a.run_id)
                OR EXISTS (SELECT 1 FROM native_subagent_reservations WHERE run_id = a.run_id AND state <> 'released')
                OR EXISTS (SELECT 1 FROM native_goal_executors WHERE run_id = a.run_id AND NOT stop_confirmed)
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

fn native_result_is_settled(db: &Connection, revision: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT stop_confirmed AND provider_result IS NOT NULL FROM native_goal_executors
         WHERE revision_id = ?1
         ORDER BY (SELECT ordinal FROM job_attempts WHERE run_id = native_goal_executors.run_id) DESC LIMIT 1",
        [revision], |row| row.get(0),
    ).optional()?.unwrap_or(false))
}

const OWNED_GOAL_JOBS: &str = "WITH RECURSIVE owned(id) AS (
        SELECT child.id FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
        JOIN jobs child ON child.origin_run_id = a.run_id WHERE c.revision_id = ?1
        UNION
        SELECT child.id FROM owned parent JOIN job_attempts a ON a.job_id = parent.id
        JOIN jobs child ON child.origin_run_id = a.run_id
    )";

fn stop_owned_children(db: &Connection, revision: &str) -> Result<()> {
    let children = {
        let mut query = db.prepare(&format!(
            "{OWNED_GOAL_JOBS} SELECT j.project_id, j.id,
               EXISTS (SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id
                   WHERE a.job_id = j.id AND r.status IN ('starting','running','waiting_for_input'))
             FROM owned JOIN jobs j ON j.id = owned.id
             WHERE j.queue_state IN ('queued','paused') AND (
               NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
               OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = j.id)
               OR EXISTS (SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id WHERE a.job_id = j.id
                 AND (r.status IN ('starting','running','waiting_for_input')
                      OR EXISTS (SELECT 1 FROM open_agent_questions q WHERE q.run_id = a.run_id AND q.state IN ('saved','needs_review'))
                      OR EXISTS (SELECT 1 FROM job_downstream_waits w WHERE w.parent_run_id = a.run_id AND w.state IN ('waiting','needs_review'))))
             )"
        ))?;
        query
            .query_map([revision], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (project, child, live) in children {
        if live {
            db.execute(
                "UPDATE job_attempts SET cancel_requested = 1 WHERE job_id = ?1
                 AND run_id IN (SELECT id FROM runs WHERE status IN ('starting','running','waiting_for_input'))",
                [&child],
            )?;
            db.execute(
                "UPDATE jobs SET queue_state = 'cancelled' WHERE id = ?1",
                [&child],
            )?;
        } else {
            super::change_pending_job_state(db, &project, &child, super::QueueState::Cancelled)?;
            let questions = {
                let mut query = db.prepare(
                    "SELECT q.request_event_seq FROM open_agent_questions q
                     JOIN job_attempts a ON a.run_id = q.run_id WHERE a.job_id = ?1",
                )?;
                query
                    .query_map([child], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?
            };
            for question in questions {
                super::archive_saved_question(db, question)?;
            }
        }
    }
    Ok(())
}

fn owned_children_unsettled(db: &Connection, revision: &str) -> Result<bool> {
    Ok(db.query_row(
        &format!("{OWNED_GOAL_JOBS} SELECT EXISTS(
            SELECT 1 FROM owned JOIN job_attempts a ON a.job_id = owned.id
            JOIN runs r ON r.id = a.run_id
            WHERE r.status IN ('starting','running','waiting_for_input')
              OR EXISTS (SELECT 1 FROM job_writer_reservations w WHERE w.run_id = a.run_id)
              OR EXISTS (SELECT 1 FROM unconfirmed_agent_stops w WHERE w.run_id = a.run_id)
              OR EXISTS (SELECT 1 FROM native_subagent_reservations w WHERE w.run_id = a.run_id AND w.state <> 'released')
        )"),
        [revision], |row| row.get(0),
    )?)
}

pub(crate) fn settle_owned_goal_cancellations(db: &Connection) -> Result<()> {
    let goals = {
        let mut query = db.prepare(
            "SELECT revision_id, confirmed_by FROM goal_revision_controls
             WHERE state = 'cancelling' AND stop_action = 'cancel' AND confirmed_by IS NOT NULL",
        )?;
        query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (revision, proof) in goals {
        if !owned_children_unsettled(db, &revision)? {
            settle_confirmed_goal_stop(db, &revision, "cancel", &proof)?;
        }
    }
    Ok(())
}

pub(crate) fn recover_managed_goal_stops(db: &Connection) -> Result<()> {
    // A previous desktop process owned the non-inheritable Windows Job Object.
    // Guest attempts still need confirmation from their runner.
    let stopped = {
        let mut statement = db.prepare(
            "DELETE FROM unconfirmed_managed_goal_stops WHERE run_id IN (
                 SELECT a.run_id FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
                 WHERE json_extract(s.scope, '$.runner') = ?1
             ) RETURNING run_id",
        )?;
        statement
            .query_map([crate::runner::DirectWindowsRunner::IDENTITY], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for run in stopped {
        super::observe_child_run(db, &run)?;
    }
    Ok(())
}

pub(super) fn retire_requalified_goal(db: &Connection, revision: &str) -> Result<()> {
    let confirmed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM active_goals active
         JOIN goal_revision_controls controls ON controls.revision_id = active.revision_id
         WHERE active.revision_id = ?1 AND controls.state = 'paused')",
        [revision],
        |row| row.get(0),
    )?;
    ensure!(
        confirmed && !owned_children_unsettled(db, revision)?,
        "The workflow goal continuation has already changed"
    );
    stop_owned_children(db, revision)?;
    release_goal(db, revision)
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
        "DELETE FROM native_goal_resume_checkpoints WHERE job_id IN (SELECT job_id FROM goal_continuations WHERE revision_id = ?1)",
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
