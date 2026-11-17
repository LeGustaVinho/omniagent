use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::{
    conversations::SessionScope,
    providers::{Provider, SessionMode, capabilities::revision},
    store::Store,
    workflows::{
        WorkflowAgentSubmission, WorkflowJobInput, stage_goal::load_stage_goal, stage_job_input,
        workflow_release_authorized,
    },
};

use super::{
    Job, JobRequest, PipelineDispatch,
    goal_control::{paused_job, retire_requalified_goal},
    goals::{confirmed_stage_worktree, load_revision},
    history::effective_release_blocker,
    now_ms,
};

pub(super) struct ProposedScope<'a> {
    pub binding: &'a PipelineDispatch,
    pub profile: &'a SessionScope,
    pub request: &'a JobRequest,
    pub inputs: &'a WorkflowJobInput,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRequalificationScope {
    pub checkout_id: String,
    pub task_id: String,
    pub unit_id: String,
    pub plan_revision: String,
    pub release_id: String,
    pub provider: Provider,
    pub profile_id: String,
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowGoalRequalification {
    pub prior_revision_id: String,
    pub prior_checkpoint_run_id: String,
    pub prior_scope: GoalRequalificationScope,
    pub replacement_scope: GoalRequalificationScope,
    pub preserves_worktree_checkpoint: bool,
    pub turns_already_used: u32,
    pub review_token: String,
}

pub(super) struct PreparedRequalification {
    pub review: WorkflowGoalRequalification,
    checkpoint_context: String,
    deadline: Option<i64>,
}

impl Store {
    pub async fn preview_workflow_goal_requalification(
        &self,
        submission: WorkflowAgentSubmission,
    ) -> Result<WorkflowGoalRequalification> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let input = WorkflowJobInput {
                unit_id: submission.unit_id,
                artifacts: submission.artifacts,
                plan_revision_request: None,
                comparison: None,
            };
            let pending = prepare_requalification(
                &tx,
                &submission.project_id,
                ProposedScope {
                    binding: &submission.dispatch,
                    profile: &submission.scope,
                    request: &submission.request,
                    inputs: &input,
                },
            )?;
            retire_requalified_goal(&tx, &pending.review.prior_revision_id)?;
            stage_job_input(
                &tx,
                &submission.project_id,
                &submission.dispatch,
                &submission.request,
                &input,
            )?;
            tx.rollback()?;
            Ok(pending.review)
        })
        .await
    }
}

pub(super) fn prepare_requalification(
    db: &Connection,
    project: &str,
    proposed: ProposedScope<'_>,
) -> Result<PreparedRequalification> {
    let ProposedScope {
        binding,
        profile,
        request,
        inputs,
    } = proposed;
    let pin = load_stage_goal(
        db,
        project,
        &binding.pipeline_run_id,
        &binding.node_instance_id,
    )?
    .context("This stage does not have a pinned Goal configuration")?;
    let (prior_revision, deadline): (String, Option<i64>) = db.query_row(
        "SELECT active.revision_id, revision.deadline_ms FROM active_goals active
         JOIN goals g ON g.id = active.goal_id JOIN goal_revisions revision ON revision.id = active.revision_id
         WHERE g.project_id = ?1 AND g.pipeline_run_id = ?2 AND g.node_instance_id = ?3",
        params![project, binding.pipeline_run_id, binding.node_instance_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).context("This workflow stage has no goal to requalify")?;
    let goal = load_revision(db, prior_revision.clone())?;
    let (prior, stopped_run) = paused_job(db, project, &prior_revision)?;
    ensure!(
        prior.mode == SessionMode::ApprovedWrite
            && request.mode == SessionMode::ApprovedWrite
            && profile.mode == SessionMode::ApprovedWrite,
        "Only approved writing stages requalify their write scope"
    );
    ensure!(
        goal.definition == pin.configuration.definition(),
        "The previous Goal configuration differs from the pinned stage"
    );
    ensure!(
        goal.definition.token_budget.is_none(),
        "A token-metered goal needs verified token accounting across scopes"
    );
    ensure!(
        effective_release_blocker(db, &prior)?.is_some(),
        "The current goal release remains valid; resume its existing scope"
    );
    ensure!(
        deadline.is_none_or(|deadline| deadline > now_ms()),
        "The goal reached its time limit"
    );
    let checkpoint_run = stopped_run
        .or_else(|| {
            prior
                .latest_attempt
                .as_ref()
                .map(|attempt| attempt.run_id.clone())
        })
        .or_else(|| {
            goal.checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.run_id.clone())
        })
        .context("The previous Goal has no stopped checkpoint")?;
    let (checkpoint_context, prior_profile): (String, String) = db
        .query_row(
            "SELECT a.result_context_id, session.scope FROM agent_attempts a
         JOIN agent_sessions session ON session.id = a.agent_session_id
         JOIN job_attempts attempt ON attempt.run_id = a.run_id
         JOIN goal_continuations g ON g.job_id = attempt.job_id
         WHERE a.run_id = ?1 AND g.revision_id = ?2 AND a.result_context_id IS NOT NULL",
            params![checkpoint_run, prior_revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("The previous Goal checkpoint has no protected result context")?;
    let turns_already_used = total_turns(db, &prior_revision)?;
    ensure!(
        turns_already_used > 0
            && goal
                .definition
                .max_turns
                .is_none_or(|max| turns_already_used < max),
        "The goal reached its turn limit"
    );

    let prior_release = prior
        .pipeline
        .as_ref()
        .and_then(|origin| origin.release.as_ref())
        .context("The previous Goal did not have a workflow release")?;
    let prior_unit = crate::workflows::load_workflow_job_input(db, &prior.id)?
        .and_then(|input| input.unit_id)
        .context("The previous Goal lost its logical unit")?;
    let prior_profile: SessionScope = serde_json::from_str(&prior_profile)?;
    let prior_scope = GoalRequalificationScope {
        checkout_id: prior_release.checkout_id.clone(),
        task_id: prior_release.task_id.clone(),
        unit_id: prior_unit,
        plan_revision: prior_release.plan_revision.clone(),
        release_id: prior_release.id.clone(),
        provider: prior_profile.provider,
        profile_id: prior_profile.profile_id.clone(),
        model: prior_profile.model.clone(),
    };
    let replacement_scope = active_scope(db, project, &proposed)?;
    ensure!(
        replacement_scope.release_id != prior_scope.release_id,
        "Approve and prepare a new release before requalifying this Goal"
    );
    let preserves_worktree_checkpoint = replacement_scope.checkout_id == prior_scope.checkout_id
        && replacement_scope.task_id == prior_scope.task_id;
    if preserves_worktree_checkpoint {
        ensure!(
            confirmed_stage_worktree(db, &prior, &checkpoint_run)?,
            "The approved worktree's prior checkpoint has no confirmed Git capture"
        );
    }
    let review_token = revision(&(
        project,
        &binding.pipeline_run_id,
        &binding.node_instance_id,
        pin.stage_revision,
        pin.configuration_revision,
        &prior_revision,
        &checkpoint_run,
        &checkpoint_context,
        &prior_scope,
        &prior_profile,
        &replacement_scope,
        profile,
        &inputs.artifacts,
        preserves_worktree_checkpoint,
        turns_already_used,
        deadline,
    ))?;
    Ok(PreparedRequalification {
        review: WorkflowGoalRequalification {
            prior_revision_id: prior_revision,
            prior_checkpoint_run_id: checkpoint_run,
            prior_scope,
            replacement_scope,
            preserves_worktree_checkpoint,
            turns_already_used,
            review_token,
        },
        checkpoint_context,
        deadline,
    })
}

fn active_scope(
    db: &Connection,
    project: &str,
    proposed: &ProposedScope<'_>,
) -> Result<GoalRequalificationScope> {
    let ProposedScope {
        binding,
        profile,
        request,
        inputs,
    } = proposed;
    let plan = binding
        .plan_revision
        .clone()
        .context("The replacement scope needs an approved plan revision")?;
    let release = binding
        .release_id
        .clone()
        .context("The replacement scope needs a workflow release")?;
    let checkout = request
        .checkout_id
        .clone()
        .context("The replacement scope needs a registered worktree")?;
    let task = request
        .task_id
        .clone()
        .context("The replacement scope needs a linked task")?;
    let unit = inputs
        .unit_id
        .clone()
        .context("The replacement scope needs an approved logical unit")?;
    let approved: Option<String> = db
        .query_row(
            "SELECT c.root FROM job_pipeline_releases r
         JOIN job_pipeline_runs w ON w.id = r.pipeline_run_id AND w.plan_revision = r.plan_revision
         JOIN checkouts c ON c.id = r.checkout_id AND c.project_id = r.project_id
         WHERE r.id = ?1 AND r.project_id = ?2 AND r.pipeline_run_id = ?3 AND r.plan_revision = ?4
           AND r.checkout_id = ?5 AND r.task_id = ?6 AND NOT r.revoked",
            params![
                release,
                project,
                binding.pipeline_run_id,
                plan,
                checkout,
                task
            ],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        approved.is_some_and(|root| profile.checkout == std::path::Path::new(&root))
            && workflow_release_authorized(db, project, &binding.pipeline_run_id, &release)?
                == Some(true),
        "Approve and prepare the exact current work scope before requalifying this Goal"
    );
    Ok(GoalRequalificationScope {
        checkout_id: checkout,
        task_id: task,
        unit_id: unit,
        plan_revision: plan,
        release_id: release,
        provider: profile.provider,
        profile_id: profile.profile_id.clone(),
        model: profile.model.clone(),
    })
}

impl PreparedRequalification {
    pub(super) fn consume(&self, db: &Connection, token: &str) -> Result<()> {
        ensure!(
            self.review.review_token == token,
            "Review the current Goal checkpoint and approved scope before requalifying"
        );
        retire_requalified_goal(db, &self.review.prior_revision_id)
    }

    pub(super) fn record(&self, db: &Connection, replacement_revision: &str) -> Result<()> {
        if let Some(deadline) = self.deadline {
            db.execute(
                "UPDATE goal_revisions SET deadline_ms = ?2 WHERE id = ?1",
                params![replacement_revision, deadline],
            )?;
        }
        db.execute("INSERT INTO workflow_goal_requalifications VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)", params![
            replacement_revision, self.review.prior_revision_id, self.review.prior_checkpoint_run_id, self.checkpoint_context,
            self.review.prior_scope.release_id, self.review.replacement_scope.release_id, self.review.turns_already_used,
            self.review.preserves_worktree_checkpoint, now_ms(),
        ])?;
        Ok(())
    }
}

pub(super) fn total_turns(db: &Connection, goal: &str) -> Result<u32> {
    Ok(db.query_row(
        "SELECT coalesce((SELECT prior_turns FROM workflow_goal_requalifications WHERE revision_id = ?1), 0) + count(*)
         FROM goal_continuations c JOIN job_attempts a ON a.job_id = c.job_id
         WHERE c.revision_id = ?1 AND EXISTS (
            SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id WHERE s.run_id = a.run_id AND d.ordinal = 1
         )", [goal], |row| row.get(0),
    )?)
}

pub(super) struct PriorScopeCheckpoint {
    pub run: String,
    pub same_worktree: bool,
}

pub(super) fn initial_checkpoint(
    db: &Connection,
    job: &Job,
) -> Result<Option<PriorScopeCheckpoint>> {
    if job.latest_attempt.is_some() {
        return Ok(None);
    }
    Ok(db
        .query_row(
            "SELECT replacement.checkpoint_run_id, replacement.same_worktree FROM workflow_goal_requalifications replacement
         JOIN goal_revisions new ON new.id = replacement.revision_id
         WHERE new.initial_job_id = ?1",
            [&job.id],
            |row| Ok(PriorScopeCheckpoint { run: row.get(0)?, same_worktree: row.get(1)? }),
        )
        .optional()?)
}
