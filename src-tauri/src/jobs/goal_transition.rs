use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    conversations::SessionScope,
    protection,
    providers::{Provider, SessionMode, capabilities::revision},
};

use super::{
    GoalCheckpoint, GoalContinuationOwner, GoalControlState, GoalDefinition,
    GoalVerificationStatus, goals::load_revision, history::load_binding,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalTransitionReview {
    pub revision_id: String,
    pub definition: GoalDefinition,
    pub continuation_owner: GoalContinuationOwner,
    pub control_state: GoalControlState,
    pub checkpoint: Option<GoalCheckpoint>,
    pub checkpoint_context_id: Option<String>,
    pub checkpoint_run_id: Option<String>,
    pub stop_review_run_id: Option<String>,
    pub scope: GoalTransitionScope,
    pub review_token: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalTransitionScope {
    pub project_id: String,
    pub checkout_id: String,
    pub checkout_root: PathBuf,
    pub provider: Provider,
    pub profile_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: SessionMode,
}

pub(crate) fn conversation_goal_transition(
    db: &Connection,
    conversation: &str,
) -> Result<Option<GoalTransitionReview>> {
    let revision_id: Option<String> = db.query_row(
        "SELECT active.revision_id FROM active_goals active JOIN goals g ON g.id = active.goal_id
         WHERE g.conversation_id = ?1 AND g.pipeline_run_id IS NULL",
        [conversation], |row| row.get(0),
    ).optional()?;
    let Some(revision_id) = revision_id else {
        return Ok(None);
    };
    let goal = load_revision(db, revision_id)?;
    if goal
        .verification
        .as_ref()
        .is_some_and(|item| item.status == GoalVerificationStatus::Completed)
    {
        return Ok(None);
    }
    let (protected, stop_run, stopped_context, current_context): (Vec<u8>, Option<String>, Option<String>, String) = db.query_row(
        "SELECT j.operator_scope, controls.stop_run_id, attempt.result_context_id, c.active_context_id
         FROM jobs j JOIN goal_revision_controls controls ON controls.revision_id = ?2
         JOIN conversations c ON c.id = ?3
         LEFT JOIN agent_attempts attempt ON attempt.run_id = controls.stop_run_id
         WHERE j.id = ?1 AND j.operator_scope IS NOT NULL",
        params![goal.job.id, goal.id, conversation], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).context("The active goal lost its recorded scope")?;
    let scope: SessionScope = serde_json::from_slice(&protection::unprotect(&protected)?)?;
    let recorded = load_binding(db, &goal.job.id)?;
    let checkpoint_context_id = recorded
        .map(|item| item.context.context_id)
        .or(stopped_context)
        .or_else(|| {
            goal.checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.context_id.clone())
        })
        .or_else(|| {
            goal.job
                .context
                .as_ref()
                .map(|context| context.context_id.clone())
        });
    let stop_review_run_id = if goal.control_state == GoalControlState::NeedsReview {
        stop_run.clone()
    } else {
        None
    };
    let checkpoint_run_id = stop_run.or_else(|| {
        goal.checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.run_id.clone())
    });
    let review_token = revision(&(
        &goal,
        &scope,
        &checkpoint_context_id,
        &checkpoint_run_id,
        &current_context,
    ))?;
    Ok(Some(GoalTransitionReview {
        revision_id: goal.id,
        definition: goal.definition,
        continuation_owner: goal.continuation_owner,
        control_state: goal.control_state,
        checkpoint: goal.checkpoint,
        checkpoint_context_id,
        checkpoint_run_id,
        stop_review_run_id,
        scope: GoalTransitionScope {
            project_id: goal.job.project_id,
            checkout_id: goal.job.checkout_id,
            checkout_root: scope.checkout,
            provider: scope.provider,
            profile_id: scope.profile_id,
            model: scope.model,
            effort: scope.effort,
            mode: scope.mode,
        },
        review_token,
    }))
}

pub(super) fn validate_goal_resume_review(
    db: &Connection,
    project: &str,
    revision: &str,
    token: &str,
) -> Result<()> {
    let conversation: String = db
        .query_row(
            "SELECT g.conversation_id FROM goals g JOIN active_goals a ON a.goal_id = g.id
         WHERE a.revision_id = ?1 AND g.project_id = ?2 AND g.pipeline_run_id IS NULL",
            params![revision, project],
            |row| row.get(0),
        )
        .context("The reviewed goal is no longer active in this conversation")?;
    let review = conversation_goal_transition(db, &conversation)?
        .context("This goal no longer awaits continuation")?;
    ensure!(
        review.revision_id == revision
            && review.control_state == GoalControlState::Paused
            && review.review_token == token,
        "The goal checkpoint or scope changed. Review the current goal before resuming."
    );
    Ok(())
}

pub(crate) fn native_goal_fork_is_safe(db: &Connection, source_session_id: &str) -> Result<bool> {
    // Even a confirmed pause can leave a resumable provider goal on this thread.
    Ok(db.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM native_goal_executors goal
         JOIN agent_attempts source ON source.run_id = goal.run_id WHERE source.agent_session_id = ?1)",
        [source_session_id], |row| row.get(0),
    )?)
}
