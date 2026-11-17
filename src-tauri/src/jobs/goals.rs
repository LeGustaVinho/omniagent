use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    conversations::{SessionScope, consume_matching_draft, load_conversation},
    protection,
    providers::{
        Provider, ProviderEvent, ProviderEventData, SessionMode, TurnOutcome, goal::NativeGoalObservation,
        questions::QuestionRoute,
    },
    repository::writers::writer_owner,
    store::Store,
    workflows::{WorkflowJobInput, bind_job_input, load_workflow_job_input, stage_job_input},
};

use super::{
    GoalControlState, GoalVerification, GoalVerificationStatus, Job, JobContext, JobOrigin,
    JobRequest, PipelineDispatch,
    goal_verification::load_goal_verification,
    history::{bind_pipeline_context, load_binding},
    insert_job, now_ms,
    pipeline::resolve_node_checkout,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalDefinition {
    pub objective: String,
    pub criteria: Vec<String>,
    pub max_turns: Option<u32>,
    pub max_seconds: Option<u32>,
    pub token_budget: Option<u64>,
}

impl GoalDefinition {
    fn validate(&self) -> Result<()> {
        ensure!(!self.objective.trim().is_empty(), "Enter a goal objective");
        ensure!(
            self.criteria
                .iter()
                .all(|criterion| !criterion.trim().is_empty()),
            "Goal criteria must be nonempty"
        );
        ensure!(
            self.max_turns != Some(0)
                && self.max_seconds != Some(0)
                && self.token_budget != Some(0),
            "Goal limits must be positive"
        );
        Ok(())
    }

    pub(super) fn instructions(&self) -> String {
        let mut prompt = format!(
            "Goal objective\n{}\n\nTake one bounded turn toward this goal. Before ending the turn, use report_goal_progress to record evidence and select the next action.",
            self.objective.trim()
        );
        if !self.criteria.is_empty() {
            prompt.push_str("\n\nAcceptance criteria\n");
            for criterion in &self.criteria {
                prompt.push_str("\n- ");
                prompt.push_str(criterion.trim());
            }
        }
        prompt
    }

    pub(super) fn native_objective(&self) -> String {
        let mut objective = self.objective.trim().to_owned();
        if !self.criteria.is_empty() {
            objective.push_str("\n\nAcceptance criteria\n");
            for criterion in &self.criteria {
                objective.push_str("\n- ");
                objective.push_str(criterion.trim());
            }
        }
        objective
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalContinuationOwner {
    Managed,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalTurnAction {
    Continue,
    Review,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalTurnReport {
    pub evidence: String,
    pub action: GoalTurnAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalCheckpoint {
    pub ordinal: u32,
    pub run_id: String,
    pub context_id: String,
    pub next_action: String,
    pub report: Option<GoalTurnReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRevision {
    pub goal_id: String,
    pub id: String,
    pub definition: GoalDefinition,
    pub continuation_owner: GoalContinuationOwner,
    pub control_state: GoalControlState,
    pub provider: Provider,
    pub model: Option<String>,
    pub job: Job,
    pub checkpoint: Option<GoalCheckpoint>,
    pub verification: Option<GoalVerification>,
    pub provider_session_id: Option<String>,
    pub provider_goal_id: Option<String>,
}

impl Store {
    pub async fn start_conversation_goal(
        &self,
        context: JobContext,
        scope: SessionScope,
        task_id: Option<String>,
        definition: GoalDefinition,
        submitted_draft: Option<String>,
    ) -> Result<GoalRevision> {
        self.start_goal(context, scope, task_id, definition, submitted_draft, None)
            .await
    }

    pub async fn start_pipeline_goal(
        &self,
        stage: PipelineDispatch,
        context: JobContext,
        scope: SessionScope,
        task_id: Option<String>,
        definition: GoalDefinition,
    ) -> Result<GoalRevision> {
        self.start_goal(context, scope, task_id, definition, None, Some(stage))
            .await
    }

    async fn start_goal(
        &self,
        context: JobContext,
        scope: SessionScope,
        task_id: Option<String>,
        definition: GoalDefinition,
        submitted_draft: Option<String>,
        stage: Option<PipelineDispatch>,
    ) -> Result<GoalRevision> {
        definition.validate()?;
        ensure!(
            scope.executable.is_absolute() && !scope.runner.is_empty(),
            "Invalid CLI profile"
        );
        let goal = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = load_conversation(&tx, &context.conversation_id)?;
            let (project, root) = if let Some(stage) = &stage {
                tx.query_row(
                    "SELECT id FROM conversation_contexts WHERE conversation_id = ?1 AND id = ?2",
                    params![conversation.id, context.context_id], |row| row.get::<_, String>(0),
                ).context("The stage's goal context is unavailable")?;
                resolve_node_checkout(&tx, stage, &conversation.checkout_id)?
            } else {
                ensure!(conversation.context_id == context.context_id, "The goal's context changed");
                tx.query_row(
                    "SELECT project_id, root FROM checkouts WHERE id = ?1",
                    [&conversation.checkout_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?
            };
            ensure!(scope.checkout == PathBuf::from(root), "The goal's checkout changed");
            ensure!(
                !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1)",
                    [&conversation.id],
                    |row| row.get::<_, bool>(0),
                )?,
                "Wait until the conversation reaches a turn boundary before starting a goal"
            );
            ensure!(
                super::instructions::parked_owner(&tx, &conversation.id, &conversation.context_id)?.is_none(),
                "This context already has an owning continuation"
            );
            if let Some(draft) = submitted_draft {
                consume_matching_draft(&tx, &conversation.id, &draft)?;
            }
            let request = JobRequest {
                checkout_id: Some(conversation.checkout_id),
                task_id,
                mode: scope.mode,
                instructions: definition.instructions(),
                dependencies: Vec::new(),
                requires_bridge: true,
            };
            let origin = if stage.is_some() { JobOrigin::Pipeline } else { JobOrigin::Operator };
            let job = insert_job(
                &tx,
                project,
                Some(context.clone()),
                origin,
                request,
                0,
                stage.as_ref(),
            )?;
            if stage.is_some() {
                bind_pipeline_context(&tx, &job.id, &context.context_id, QuestionRoute::Native)?;
            }
            tx.execute(
                "UPDATE jobs SET operator_scope = ?1 WHERE id = ?2",
                params![protection::protect(&serde_json::to_vec(&scope)?)?, job.id],
            )?;
            let (goal_id, id) = bind_initial_goal(&tx, &job, &context.conversation_id, &definition, stage.as_ref())?;
            tx.commit()?;
            Ok(GoalRevision {
                goal_id, id, definition, continuation_owner: GoalContinuationOwner::Managed,
                control_state: GoalControlState::Active,
                provider: scope.provider, model: scope.model,
                job, checkpoint: None, verification: None, provider_session_id: None, provider_goal_id: None,
            })
        }).await?;
        self.job_signal.send_replace(());
        Ok(goal)
    }

    pub async fn active_conversation_goal(
        &self,
        conversation: String,
    ) -> Result<Option<GoalRevision>> {
        self.execute(move |db| {
            active_revision(db, &conversation, None)?
                .map(|id| load_revision(db, id))
                .transpose()
        })
        .await
    }

    pub async fn active_pipeline_goal(
        &self,
        project: String,
        pipeline_run: String,
        node_instance: String,
    ) -> Result<Option<GoalRevision>> {
        self.execute(move |db| {
            let id = db
                .query_row(
                    "SELECT a.revision_id FROM active_goals a JOIN goals g ON g.id = a.goal_id
                 WHERE g.project_id = ?1 AND g.pipeline_run_id = ?2 AND g.node_instance_id = ?3",
                    params![project, pipeline_run, node_instance],
                    |row| row.get(0),
                )
                .optional()?;
            id.map(|id| load_revision(db, id)).transpose()
        })
        .await
    }

    pub(crate) async fn record_goal_turn_report(
        &self,
        run: String,
        report: GoalTurnReport,
    ) -> Result<bool> {
        ensure!(
            !report.evidence.trim().is_empty() && report.evidence.len() <= 8192,
            "Invalid goal evidence"
        );
        let payload = protection::protect(&serde_json::to_vec(&report)?)?;
        let report_key = crate::providers::capabilities::revision(&report)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active: Option<bool> = tx
                .query_row(
                    "SELECT r.continuation_owner = 'managed' AND g.revision_id IS NOT NULL
                    AND controls.state = 'active' AND NOT j.cancel_requested
                    AND EXISTS (SELECT 1 FROM active_agent_attempts a WHERE a.run_id = j.run_id)
                    AND NOT EXISTS (SELECT 1 FROM native_goal_executors x WHERE x.run_id = j.run_id)
                 FROM job_attempts j JOIN goal_continuations c ON c.job_id = j.job_id
                 JOIN goal_revisions r ON r.id = c.revision_id
                 JOIN goal_revision_controls controls ON controls.revision_id = r.id
                 LEFT JOIN active_goals g ON g.revision_id = c.revision_id WHERE j.run_id = ?1",
                    [&run],
                    |row| row.get(0),
                )
                .optional()?;
            let active = active.context("The attempt does not belong to a goal")?;
            if !active {
                tx.execute(
                    "INSERT INTO late_goal_turn_reports VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
                    params![run, report_key, payload],
                )?;
                tx.commit()?;
                return Ok(false);
            }
            let old = tx
                .query_row(
                    "SELECT payload FROM goal_turn_reports WHERE run_id = ?1",
                    [&run],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(old) = decode_report(old)? {
                ensure!(
                    old == report,
                    "This turn already reported a different checkpoint"
                );
            } else {
                tx.execute(
                    "INSERT INTO goal_turn_reports VALUES (?1, ?2)",
                    params![run, payload],
                )?;
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    pub async fn archived_goal_reports(
        &self,
        project: String,
        run: String,
    ) -> Result<Vec<GoalTurnReport>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT report.payload FROM late_goal_turn_reports report
                 JOIN job_attempts a ON a.run_id = report.run_id JOIN jobs j ON j.id = a.job_id
                 WHERE report.run_id = ?1 AND j.project_id = ?2 ORDER BY report.report_key",
            )?;
            query
                .query_map(params![run, project], |row| row.get::<_, Vec<u8>>(0))?
                .map(|payload| Ok(serde_json::from_slice(&protection::unprotect(&payload?)?)?))
                .collect()
        })
        .await
    }
}

pub(super) fn load_revision(db: &Connection, id: String) -> Result<GoalRevision> {
    let (goal_id, project, contents, owner, profile): (String, String, Vec<u8>, String, Vec<u8>) =
        db.query_row(
            "SELECT g.id, g.project_id, r.definition, r.continuation_owner, j.operator_scope
                 FROM goal_revisions r JOIN goals g ON g.id = r.goal_id
                 JOIN jobs j ON j.id = r.initial_job_id WHERE r.id = ?1",
            [&id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    let profile: SessionScope = serde_json::from_slice(&protection::unprotect(&profile)?)?;
    let job_id: String = db.query_row(
                "SELECT job_id FROM goal_continuations WHERE revision_id = ?1 ORDER BY checkpoint_ordinal DESC LIMIT 1",
                [&id], |row| row.get(0),
            )?;
    let job = super::load_job(db, &project, &job_id)?.context("The goal's job is unavailable")?;
    let (provider_session_id, provider_goal_id) = db.query_row(
                "SELECT coalesce(n.provider_session_id, s.provider_session_id), n.provider_goal_id
                 FROM goal_continuations c JOIN job_attempts j ON j.job_id = c.job_id
                 JOIN agent_attempts a ON a.run_id = j.run_id JOIN agent_sessions s ON s.id = a.agent_session_id
                 LEFT JOIN native_goal_observations n ON n.run_id = a.run_id
                 WHERE c.revision_id = ?1 ORDER BY c.checkpoint_ordinal DESC, j.ordinal DESC LIMIT 1",
                [&id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?.unwrap_or_default();
    let checkpoint = db.query_row(
                "SELECT c.ordinal, c.run_id, c.context_id, c.next_action, r.payload FROM goal_checkpoints c
                 LEFT JOIN goal_turn_reports r ON r.run_id = c.run_id
                 WHERE c.revision_id = ?1 ORDER BY c.ordinal DESC LIMIT 1",
                [&id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, Option<Vec<u8>>>(4)?)),
            ).optional()?.map(|(ordinal, run_id, context_id, next_action, report)| -> Result<_> {
                Ok(GoalCheckpoint { ordinal, run_id, context_id, next_action, report: decode_report(report)? })
            }).transpose()?;
    let control_state: String = db.query_row(
        "SELECT state FROM goal_revision_controls WHERE revision_id = ?1",
        [&id],
        |row| row.get(0),
    )?;
    let verification = checkpoint
        .as_ref()
        .map(|checkpoint| load_goal_verification(db, &project, &checkpoint.run_id))
        .transpose()?
        .flatten();
    Ok(GoalRevision {
        goal_id,
        id,
        job,
        checkpoint,
        verification,
        provider_session_id,
        provider_goal_id,
        provider: profile.provider,
        model: profile.model,
        definition: serde_json::from_slice(&protection::unprotect(&contents)?)?,
        continuation_owner: serde_json::from_value(serde_json::Value::String(owner))?,
        control_state: GoalControlState::from_key(&control_state)?,
    })
}

pub(super) fn orchestrator_goal(db: &Connection, run: &str) -> Result<Option<GoalRevision>> {
    run_goal_membership(db, run)?
        .map(|(_, membership)| load_revision(db, membership.revision))
        .transpose()
}

pub(super) fn limit_blocker(
    db: &Connection,
    job: &str,
    native_candidate: bool,
) -> Result<Option<&'static str>> {
    let Some(goal) = goal_membership(db, job)? else {
        return Ok(None);
    };
    if let Some(reason) = goal.control_blocker() {
        return Ok(Some(reason));
    }
    if goal.deadline.is_some_and(|deadline| now_ms() >= deadline) {
        return Ok(Some("The goal reached its time limit"));
    }
    if goal.delegated {
        return delegation_blocker(db, job, goal.token_budget);
    }
    if goal.token_budget && goal.managed && !native_candidate {
        return Ok(Some(
            "The managed goal route cannot enforce this token budget",
        ));
    }
    if managed_turn_limit_reached(db, &goal.revision)? {
        return Ok(Some("The goal reached its turn limit"));
    }
    Ok(None)
}

struct GoalMembership {
    revision: String,
    deadline: Option<i64>,
    managed: bool,
    token_budget: bool,
    state: GoalControlState,
    delegated: bool,
    pausing_for_wait: bool,
}

impl GoalMembership {
    fn control_blocker(&self) -> Option<&'static str> {
        if self.delegated && self.pausing_for_wait && self.state == GoalControlState::Pausing {
            return None;
        }
        self.state.blocker()
    }
}

const GOAL_LINEAGE: &str = "WITH RECURSIVE lineage(job_id, origin_run_id) AS (
    SELECT id, origin_run_id FROM jobs WHERE id = ?1
    UNION ALL
    SELECT parent.id, parent.origin_run_id
    FROM lineage child JOIN job_attempts origin ON origin.run_id = child.origin_run_id
    JOIN jobs parent ON parent.id = origin.job_id
    WHERE NOT EXISTS (SELECT 1 FROM goal_continuations c WHERE c.job_id = child.job_id)
)";

fn goal_membership(db: &Connection, job: &str) -> Result<Option<GoalMembership>> {
    let row = db
        .query_row(
            &format!(
                "{GOAL_LINEAGE}
                SELECT r.id, r.deadline_ms, r.continuation_owner = 'managed',
                       r.token_budget_requested, controls.state, c.job_id <> ?1,
                       controls.resume_after_wait
                FROM lineage JOIN goal_continuations c ON c.job_id = lineage.job_id
                JOIN goal_revisions r ON r.id = c.revision_id
                JOIN goal_revision_controls controls ON controls.revision_id = r.id"
            ),
            [job],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, String>(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(revision, deadline, managed, token_budget, state, delegated, pausing_for_wait)| {
            Ok(GoalMembership {
                revision,
                deadline,
                managed,
                token_budget,
                state: GoalControlState::from_key(&state)?,
                delegated,
                pausing_for_wait,
            })
        },
    )
    .transpose()
}

fn delegation_blocker(
    db: &Connection,
    job: &str,
    token_budget: bool,
) -> Result<Option<&'static str>> {
    if token_budget {
        return Ok(Some(
            "Delegated work cannot enforce this goal's token budget",
        ));
    }
    let mut origins = db.prepare(&format!(
        "{GOAL_LINEAGE} SELECT origin_run_id FROM lineage WHERE origin_run_id IS NOT NULL"
    ))?;
    for origin in origins.query_map([job], |row| row.get::<_, String>(0))? {
        if super::policy::attempt_requirements(db, &origin?)?.is_err() {
            return Ok(Some("The goal's delegation authorization changed"));
        }
    }
    Ok(None)
}

pub(super) fn new_delegation_blocker(db: &Connection, run: &str) -> Result<Option<&'static str>> {
    let Some((job, goal)) = run_goal_membership(db, run)? else {
        return Ok(None);
    };
    if let Some(reason) = goal.control_blocker() {
        return Ok(Some(reason));
    }
    if goal.deadline.is_some_and(|deadline| now_ms() >= deadline) {
        return Ok(Some("The goal reached its time limit"));
    }
    if super::policy::attempt_requirements(db, run)?.is_err() {
        return Ok(Some("The goal's delegation authorization changed"));
    }
    delegation_blocker(db, &job, goal.token_budget)
}

fn managed_turn_limit_reached(db: &Connection, revision: &str) -> Result<bool> {
    let definition: Vec<u8> = db.query_row(
        "SELECT definition FROM goal_revisions WHERE id = ?1",
        [revision],
        |row| row.get(0),
    )?;
    let definition: GoalDefinition = serde_json::from_slice(&protection::unprotect(&definition)?)?;
    let Some(max_turns) = definition.max_turns else {
        return Ok(false);
    };
    // A prepared dispatch may have reached the CLI before a disconnect or restart.
    Ok(super::goal_requalification::total_turns(db, revision)? >= max_turns)
}

pub(crate) fn goal_dispatch_blocker(db: &Connection, run: &str) -> Result<Option<&'static str>> {
    let job = attempt_job_id(db, run)?;
    let Some(job) = job else {
        return Ok(None);
    };
    let deleted_checkpoint: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM goal_writer_attempts checkpoint
         JOIN history_deleted_attempt_diffs deleted ON deleted.run_id = checkpoint.checkpoint_run_id
         WHERE checkpoint.run_id = ?1)",
        [run], |row| row.get(0),
    )?;
    if deleted_checkpoint {
        return Ok(Some("The Goal worktree checkpoint was removed from history"));
    }
    // A token budget was qualified at admission and is rechecked at native activation.
    limit_blocker(db, &job, true)
}

fn attempt_job_id(db: &Connection, run: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT job_id FROM job_attempts WHERE run_id = ?1",
            [run],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

fn run_goal_membership(db: &Connection, run: &str) -> Result<Option<(String, GoalMembership)>> {
    let Some(job) = attempt_job_id(db, run)? else {
        return Ok(None);
    };
    Ok(goal_membership(db, &job)?.map(|goal| (job, goal)))
}

pub(super) fn goal_owns_run(db: &Connection, run: &str) -> Result<bool> {
    Ok(run_goal_membership(db, run)?.is_some())
}

pub(super) fn deactivate_cancelled_job(db: &Connection, job: &str) -> Result<()> {
    let cancelled: Option<String> = db.query_row(
        "SELECT revision_id FROM active_goals
         WHERE revision_id IN (
             SELECT c.revision_id FROM goal_continuations c
             JOIN goal_revisions r ON r.id = c.revision_id
             JOIN goal_revision_controls controls ON controls.revision_id = c.revision_id
             WHERE c.job_id = ?1 AND r.continuation_owner = 'managed'
               AND controls.state IN ('active','paused')
               AND NOT EXISTS (SELECT 1 FROM goal_continuations next
                               WHERE next.revision_id = c.revision_id AND next.checkpoint_ordinal > c.checkpoint_ordinal)
               AND NOT EXISTS (
                   SELECT 1 FROM goal_continuations member JOIN job_attempts a ON a.job_id = member.job_id
                   WHERE member.revision_id = c.revision_id AND (
                       EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = a.run_id)
                       OR EXISTS (SELECT 1 FROM job_writer_reservations WHERE run_id = a.run_id)
                       OR EXISTS (SELECT 1 FROM unconfirmed_agent_stops WHERE run_id = a.run_id)
                   )
               )
         )",
        [job],
        |row| row.get(0),
    ).optional()?;
    if let Some(revision) = cancelled {
        super::goal_control::cancel_parked_goal_for_history(db, &revision, true, None)?;
    }
    Ok(())
}

pub(crate) struct GoalExecution {
    deadline_ms: Option<i64>,
    pub(crate) delegated: bool,
}

impl GoalExecution {
    pub(crate) fn time_limit_reached(&self) -> bool {
        self.deadline_ms
            .is_some_and(|deadline| now_ms() >= deadline)
    }
}

impl Store {
    pub(crate) async fn goal_execution_for_run(
        &self,
        run: String,
    ) -> Result<Option<GoalExecution>> {
        self.execute(move |db| {
            Ok(
                run_goal_membership(db, &run)?.map(|(_, goal)| GoalExecution {
                    deadline_ms: goal.deadline,
                    delegated: goal.delegated,
                }),
            )
        })
        .await
    }

    pub(crate) async fn goal_delegation_revoked(&self, run: String) -> Result<bool> {
        self.execute(move |db| delegated_goal_authority_revoked(db, &run))
            .await
    }

    pub(crate) async fn activate_native_goal_executor(
        &self,
        run: String,
        evidence: String,
        token_budget: Option<u64>,
    ) -> Result<bool> {
        let token_budget = token_budget.map(i64::try_from).transpose()?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let execution: Option<(String, String)> = tx.query_row(
                "SELECT r.id, s.provider_session_id FROM active_agent_attempts active
                 JOIN job_attempts j ON j.run_id = active.run_id
                 JOIN goal_revisions r ON r.initial_job_id = j.job_id
                 JOIN active_goals current ON current.revision_id = r.id
                 JOIN goal_revision_controls controls ON controls.revision_id = r.id
                 JOIN agent_attempts a ON a.run_id = active.run_id
                 JOIN agent_sessions s ON s.id = a.agent_session_id
                 WHERE active.run_id = ?1 AND r.continuation_owner = 'managed'
                   AND controls.state = 'active' AND NOT j.cancel_requested
                   AND (r.deadline_ms IS NULL OR r.deadline_ms > ?2)
                   AND EXISTS (SELECT 1 FROM native_goal_context_receipts c
                     WHERE c.run_id = ?1 AND c.provider_session_id = s.provider_session_id)
                   AND EXISTS (SELECT 1 FROM native_goal_qualifications q WHERE q.evidence_id = ?3)",
                params![run, now_ms(), evidence], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((revision, session)) = execution else {
                return Ok(false);
            };
            if !super::policy::validate_attempt_policy(&tx, &run)? {
                tx.commit()?;
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO native_goal_executors(run_id, revision_id, provider_session_id, evidence_id, token_budget)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![run, revision, session, evidence, token_budget],
            )?;
            tx.execute("UPDATE goal_revisions SET continuation_owner = 'native' WHERE id = ?1", [&revision])?;
            tx.execute(
                "DELETE FROM native_goal_resume_checkpoints WHERE job_id = (SELECT job_id FROM job_attempts WHERE run_id = ?1)",
                [&run],
            )?;
            tx.commit()?;
            Ok(true)
        }).await
    }

    pub(crate) async fn record_native_goal_result(
        &self,
        run: String,
        result: &'static str,
    ) -> Result<()> {
        self.execute(move |db| {
            let changed = db.execute(
                "UPDATE native_goal_executors SET provider_result = ?2 WHERE run_id = ?1 AND provider_result IS NULL",
                params![run, result],
            )?;
            ensure!(changed == 1, "The native executor has no current result boundary");
            Ok(())
        }).await
    }
}

pub(crate) fn delegated_goal_authority_revoked(db: &Connection, run: &str) -> Result<bool> {
    let Some((job, goal)) = run_goal_membership(db, run)? else {
        return Ok(false);
    };
    // Cancellation follows the queue's explicit stop path.
    if !goal.delegated
        || matches!(
            goal.state,
            GoalControlState::Cancelling | GoalControlState::Cancelled
        )
    {
        return Ok(false);
    }
    Ok(goal.control_blocker().is_some()
        || delegation_blocker(db, &job, goal.token_budget)?.is_some()
        || !super::policy::validate_attempt_policy(db, run)?)
}

pub(crate) fn settle_native_goal_executor(
    db: &Connection,
    run: &str,
    confirmed: bool,
) -> Result<()> {
    if confirmed {
        db.execute(
            "UPDATE native_goal_executors SET stop_confirmed = 1 WHERE run_id = ?1",
            [run],
        )?;
    }
    Ok(())
}

pub(crate) fn recover_native_goal_executors(db: &Connection) -> Result<()> {
    // The old single-instance desktop owned the non-inheritable Job Object.
    // Its process tree stopped on exit. The provider goal still needs review.
    db.execute(
        "UPDATE native_goal_executors SET stop_confirmed = 1
         WHERE NOT stop_confirmed AND run_id IN (
           SELECT a.run_id FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
           WHERE json_extract(s.scope, '$.runner') = ?1
         )",
        [crate::runner::DirectWindowsRunner::IDENTITY],
    )?;
    Ok(())
}

pub(crate) fn record_native_goal(
    db: &Connection,
    run: &str,
    event_seq: i64,
    observed: &NativeGoalObservation,
) -> Result<()> {
    db.execute(
        "INSERT INTO native_goal_observations(run_id, event_seq, provider_goal_id, provider_session_id)
         SELECT j.run_id, ?2, ?3, ?4 FROM job_attempts j
         JOIN goal_continuations c ON c.job_id = j.job_id JOIN goal_revisions r ON r.id = c.revision_id
         JOIN agent_attempts a ON a.run_id = j.run_id JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE j.run_id = ?1 AND r.continuation_owner = 'native' AND s.provider_session_id = ?4
         ON CONFLICT(run_id) DO UPDATE SET event_seq = excluded.event_seq,
           provider_goal_id = coalesce(excluded.provider_goal_id, native_goal_observations.provider_goal_id)",
        params![run, event_seq, observed.provider_goal_id, observed.provider_session_id],
    )?;
    Ok(())
}

pub(crate) fn is_native_goal_run(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_goal_executors WHERE run_id = ?1
           UNION ALL SELECT 1 FROM job_attempts j
           JOIN goal_continuations c ON c.job_id = j.job_id
           JOIN goal_revisions r ON r.id = c.revision_id
           WHERE j.run_id = ?1 AND r.continuation_owner = 'native')",
        [run],
        |row| row.get(0),
    )?)
}

pub(crate) fn record_native_goal_turn(
    db: &Connection,
    run: &str,
    seq: i64,
    event: &ProviderEvent,
) -> Result<()> {
    if matches!(&event.data, ProviderEventData::GoalObserved { .. }) {
        // Goal notifications can refer to a turn that has already ended.
        return Ok(());
    }
    let turn = event.location.turn_id.as_deref();
    match &event.data {
        ProviderEventData::TurnStarted => {
            let turn = turn.context("Native goal turn has no turn ID")?;
            ensure!(
                event.location.session_id.is_some(),
                "Native goal turn has no session ID"
            );
            let inserted = db.execute(
                "INSERT INTO native_goal_turns(run_id, provider_turn_id, started_event_seq)
                 VALUES (?1, ?2, ?3) ON CONFLICT(run_id, provider_turn_id) DO NOTHING",
                params![run, turn, seq],
            )?;
            if inserted == 1 {
                db.execute(
                    "UPDATE agent_attempts SET provider_turn_id = ?2, provider_terminal = NULL
                     WHERE run_id = ?1",
                    params![run, turn],
                )?;
            }
        }
        ProviderEventData::TurnFinished { outcome } => {
            let turn = turn.context("Native goal turn has no turn ID")?;
            ensure!(
                event.location.session_id.is_some(),
                "Native goal turn has no session ID"
            );
            let state = match outcome {
                TurnOutcome::Completed => "completed",
                TurnOutcome::Failed => "failed",
                TurnOutcome::Interrupted => "interrupted",
            };
            let changed = db.execute(
                "UPDATE native_goal_turns SET finished_event_seq = ?3, outcome = ?4
                 WHERE run_id = ?1 AND provider_turn_id = ?2 AND outcome IS NULL",
                params![run, turn, seq, state],
            )?;
            if changed == 1 {
                db.execute(
                    "UPDATE agent_attempts SET provider_terminal = ?3
                     WHERE run_id = ?1 AND provider_turn_id = ?2",
                    params![run, turn, state],
                )?;
            } else {
                let known: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_goal_turns WHERE run_id = ?1 AND provider_turn_id = ?2)",
                    params![run, turn],
                    |row| row.get(0),
                )?;
                ensure!(known, "Native goal terminal event has no observed start");
            }
        }
        _ if let Some(turn) = turn => {
            let open: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_goal_turns WHERE run_id = ?1 AND provider_turn_id = ?2 AND outcome IS NULL)",
                params![run, turn],
                |row| row.get(0),
            )?;
            ensure!(
                open,
                "Provider event does not match the active native goal turn"
            );
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn accept_goal_checkpoint(db: &Connection, run: &str, context: &str) -> Result<()> {
    let source = db.query_row(
        "SELECT r.id, c.checkpoint_ordinal + 1, r.definition, r.deadline_ms, j.job_id,
                g.project_id, a.conversation_id, report.payload,
                x.run_id IS NOT NULL OR r.continuation_owner = 'native',
                CASE WHEN x.stop_confirmed THEN x.provider_result END
         FROM job_attempts j JOIN goal_continuations c ON c.job_id = j.job_id
         JOIN goal_revisions r ON r.id = c.revision_id JOIN active_goals active ON active.revision_id = r.id
         JOIN goals g ON g.id = r.goal_id JOIN agent_attempts a ON a.run_id = j.run_id
         JOIN runs completed ON completed.id = j.run_id
         JOIN goal_revision_controls controls ON controls.revision_id = r.id
         LEFT JOIN native_goal_executors x ON x.run_id = j.run_id
         LEFT JOIN goal_turn_reports report ON report.run_id = j.run_id
         WHERE j.run_id = ?1 AND j.cancel_requested = 0 AND a.provider_terminal = 'completed'
           AND completed.status = 'succeeded' AND controls.state = 'active' AND a.result_context_id = ?2
           AND NOT EXISTS (SELECT 1 FROM job_attempts later WHERE later.job_id = j.job_id AND later.ordinal > j.ordinal)
           AND EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                       WHERE s.run_id = ?1 AND d.ordinal = 1 AND d.delivery = 'submitted')",
        params![run, context], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, Vec<u8>>(2)?,
            row.get::<_, Option<i64>>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            row.get::<_, String>(6)?, row.get::<_, Option<Vec<u8>>>(7)?, row.get::<_, bool>(8)?,
            row.get::<_, Option<String>>(9)?)),
    ).optional()?;
    let Some((
        revision,
        ordinal,
        definition,
        deadline,
        source_job,
        project,
        conversation,
        report,
        native,
        native_result,
    )) = source
    else {
        return Ok(());
    };
    let recorded: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM goal_checkpoints WHERE run_id = ?1)", [run], |row| row.get(0))?;
    if recorded {
        return Ok(());
    }
    let definition: GoalDefinition = serde_json::from_slice(&protection::unprotect(&definition)?)?;
    let report = decode_report(report)?;
    let mut next = if native {
        match native_result.as_deref() {
            Some("blocked") => "blocked",
            Some("limit_reached") => "limit_reached",
            Some("completed") => "review",
            _ => return Ok(()),
        }
    } else {
        match report.as_ref().map(|report| report.action) {
            Some(GoalTurnAction::Blocked) => "blocked",
            Some(GoalTurnAction::Continue) => {
                if managed_turn_limit_reached(db, &revision)?
                    || deadline.is_some_and(|deadline| now_ms() >= deadline)
                    || definition.token_budget.is_some()
                {
                    "limit_reached"
                } else {
                    "continue"
                }
            }
            _ => "review",
        }
    };
    let mut continuation = None;
    if next == "continue" {
        let prior =
            super::load_job(db, &project, &source_job)?.context("The goal job is unavailable")?;
        let permitted_scope = if let Some(expected) = &prior.worktree_owner_task_id {
            let task = prior
                .task_id
                .as_deref()
                .context("The goal writer has no task")?;
            writer_owner(db, &project, task, &prior.checkout_id)?.as_ref() == Some(expected)
        } else {
            true
        };
        if permitted_scope && super::history::effective_release_blocker(db, &prior)?.is_none() {
            let (request, stage, input) = continuation_inputs(db, &prior, &definition)?;
            if let (Some(stage), Some(input)) = (&stage, &input) && (
                (prior.mode == SessionMode::ApprovedWrite && !confirmed_stage_worktree(db, &prior, run)?)
                || stage_job_input(db, &project, stage, &request, input).is_err()) {
                next = "blocked";
            } else {
                continuation = Some((prior, request, stage, input));
            }
        } else {
            next = "blocked";
        }
    }
    let inserted = db.execute(
        "INSERT INTO goal_checkpoints(revision_id, ordinal, run_id, context_id, next_action)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
        params![revision, ordinal, run, context, next],
    )?;
    if inserted == 1
        && let Some((prior, request, stage, input)) = continuation
    {
        let target = insert_job(
            db,
            project,
            Some(JobContext {
                conversation_id: conversation,
                context_id: context.into(),
            }),
            prior.origin,
            request,
            prior.priority,
            stage.as_ref(),
        )?;
        if let (Some(stage), Some(input)) = (&stage, &input) {
            bind_job_input(db, &target.id, stage, input)?;
            copy_workflow_goal_context(db, &source_job, &target.id)?;
        }
        db.execute(
            "UPDATE jobs SET operator_scope = (SELECT operator_scope FROM jobs WHERE id = ?1) WHERE id = ?2",
            params![source_job, target.id],
        )?;
        if load_binding(db, &source_job)?.is_some() {
            db.execute(
                "INSERT INTO job_history_bindings(job_id, context_id, pipeline_pin, structured_questions)
                 SELECT ?1, ?2, pipeline_pin, structured_questions FROM job_history_bindings WHERE job_id = ?3",
                params![target.id, context, source_job],
            )?;
        }
        db.execute(
            "INSERT INTO goal_continuations VALUES (?1, ?2, ?3)",
            params![revision, ordinal, target.id],
        )?;
    }
    Ok(())
}

pub(super) fn confirmed_stage_worktree(db: &Connection, job: &Job, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM job_worktree_starts s
             JOIN job_worktree_candidates c ON c.run_id = s.run_id
             JOIN job_attempts a ON a.run_id = s.run_id
             JOIN jobs source ON source.id = a.job_id
             LEFT JOIN job_worktree_scopes scope ON scope.job_id = source.id
             WHERE s.run_id = ?1 AND s.checkout_id = ?2 AND c.capture_confirmed
               AND NOT EXISTS (SELECT 1 FROM history_deleted_attempt_diffs deleted WHERE deleted.run_id = s.run_id)
               AND source.project_id = ?3 AND coalesce(scope.scope_task_id, source.task_id) = ?4)",
        params![run, job.checkout_id, job.project_id, job.task_id],
        |row| row.get(0),
    )?)
}

fn continuation_inputs(
    db: &Connection,
    prior: &Job,
    definition: &GoalDefinition,
) -> Result<(JobRequest, Option<PipelineDispatch>, Option<WorkflowJobInput>)> {
    let stage = prior.pipeline.as_ref().map(|origin| origin.binding());
    let workflow = load_workflow_job_input(db, &prior.id)?;
    let request = if workflow.is_some() {
        ensure!(stage.is_some(), "The goal job lost its workflow origin");
        let encoded: Vec<u8> = db.query_row(
            "SELECT request FROM jobs WHERE id = ?1 AND project_id = ?2",
            params![prior.id, prior.project_id],
            |row| row.get(0),
        )?;
        let request: JobRequest = serde_json::from_slice(&protection::unprotect(&encoded)?)?;
        ensure!(
            request.checkout_id.as_deref() == Some(prior.checkout_id.as_str())
                && request.task_id == prior.task_id && request.mode == prior.mode,
            "The goal job lost its approved scope"
        );
        request
    } else {
        JobRequest {
            checkout_id: Some(prior.checkout_id.clone()),
            task_id: prior.task_id.clone(),
            mode: prior.mode,
            instructions: definition.instructions(),
            dependencies: Vec::new(),
            requires_bridge: true,
        }
    };
    Ok((request, stage, workflow))
}

pub(crate) fn require_workflow_goal_release(db: &Connection, run: &str) -> Result<()> {
    let job = super::pipeline::load_attempt_job(db, run)?
        .context("The workflow goal attempt lost its scheduled job")?;
    ensure!(
        super::history::effective_release_blocker(db, &job)?.is_none(),
        "The workflow goal needs current approval for this result scope"
    );
    Ok(())
}

fn copy_workflow_goal_context(db: &Connection, source: &str, target: &str) -> Result<()> {
    for statement in [
        "INSERT INTO job_role_contexts(job_id, policy) SELECT ?2, policy FROM job_role_contexts WHERE job_id = ?1",
        "INSERT INTO job_repository_markdown(job_id, checkout_id, revision_id) SELECT ?2, checkout_id, revision_id FROM job_repository_markdown WHERE job_id = ?1",
        "INSERT INTO job_task_run_scopes(job_id, project_id, task_id, role, checklist_item_id) SELECT ?2, project_id, task_id, role, checklist_item_id FROM job_task_run_scopes WHERE job_id = ?1",
        "INSERT INTO job_checklist_modes(job_id, mode) SELECT ?2, mode FROM job_checklist_modes WHERE job_id = ?1",
        "INSERT INTO model_comparison_inputs(job_id, position, group_id, source_job_id, source_run_id, source_dispatch_id, response_revision_id)
         SELECT ?2, position, group_id, source_job_id, source_run_id, source_dispatch_id, response_revision_id FROM model_comparison_inputs WHERE job_id = ?1",
    ] {
        db.execute(statement, params![source, target])?;
    }
    Ok(())
}

fn decode_report(payload: Option<Vec<u8>>) -> Result<Option<GoalTurnReport>> {
    payload
        .map(|payload| Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?))
        .transpose()
}

pub(super) struct GoalBoundary {
    pub run: String,
    pub conversation: String,
    pub context: String,
}

pub(super) fn managed_boundary(db: &Connection, job: &str) -> Result<Option<GoalBoundary>> {
    Ok(db
        .query_row(
            "SELECT checkpoint.run_id, a.conversation_id, checkpoint.context_id
         FROM goal_continuations c JOIN active_goals g ON g.revision_id = c.revision_id
         JOIN goal_checkpoints checkpoint ON checkpoint.revision_id = c.revision_id
            AND checkpoint.ordinal = c.checkpoint_ordinal
         JOIN agent_attempts a ON a.run_id = checkpoint.run_id
         WHERE c.job_id = ?1 AND checkpoint.next_action = 'continue'
            AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = c.job_id)",
            [job],
            |row| {
                Ok(GoalBoundary {
                    run: row.get(0)?,
                    conversation: row.get(1)?,
                    context: row.get(2)?,
                })
            },
        )
        .optional()?)
}

pub(super) fn queued_boundary_owner(
    db: &Connection,
    conversation: &str,
    context: &str,
) -> Result<Option<String>> {
    // Releasing the process reservation does not settle the native goal's
    // continuation. Keep its context reserved until the owner is reconciled.
    let native_owner = db
        .query_row(
            "SELECT x.run_id FROM native_goal_executors x
         JOIN active_goals g ON g.revision_id = x.revision_id
         JOIN goal_revisions r ON r.id = g.revision_id
         JOIN agent_attempts a ON a.run_id = x.run_id
         WHERE a.conversation_id = ?1 AND coalesce(a.result_context_id, a.context_id) = ?2
           AND r.continuation_owner = 'native'
         ORDER BY a.ordinal DESC LIMIT 1",
            params![conversation, context],
            |row| row.get(0),
        )
        .optional()?;
    if native_owner.is_some() {
        return Ok(native_owner);
    }
    Ok(db
        .query_row(
            "SELECT checkpoint.run_id
         FROM goal_continuations c JOIN active_goals g ON g.revision_id = c.revision_id
         JOIN goal_revisions revision ON revision.id = c.revision_id
         JOIN goal_checkpoints checkpoint ON checkpoint.revision_id = c.revision_id
            AND checkpoint.ordinal = c.checkpoint_ordinal
         JOIN jobs j ON j.id = c.job_id
         WHERE j.conversation_id = ?1 AND j.context_id = ?2 AND j.queue_state <> 'cancelled'
           AND (revision.deadline_ms IS NULL OR revision.deadline_ms > ?3)
           AND checkpoint.next_action = 'continue'
           AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = c.job_id)
         ORDER BY j.sequence LIMIT 1",
            params![conversation, context, now_ms()],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn bind_initial_goal(
    db: &Connection,
    job: &Job,
    conversation: &str,
    definition: &GoalDefinition,
    stage: Option<&PipelineDispatch>,
) -> Result<(String, String)> {
    definition.validate()?;
    if let Some(active) = active_revision(db, conversation, stage)? {
        let goal = load_revision(db, active)?;
        ensure!(
            goal.verification.is_some_and(|verification| verification.status == GoalVerificationStatus::Completed),
            "This conversation or stage already has an active goal"
        );
        db.execute("DELETE FROM active_goals WHERE revision_id = ?1", [&goal.id])?;
    }
    let new_id = Uuid::new_v4().to_string();
    let goal_id: String = if let Some(stage) = stage {
        db.query_row(
            "INSERT INTO goals(id, project_id, conversation_id, pipeline_run_id, node_instance_id)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(pipeline_run_id, node_instance_id) WHERE pipeline_run_id IS NOT NULL
             DO UPDATE SET conversation_id = excluded.conversation_id RETURNING id",
            params![new_id, job.project_id, conversation, stage.pipeline_run_id, stage.node_instance_id],
            |row| row.get(0),
        )?
    } else {
        db.query_row(
            "INSERT INTO goals(id, project_id, conversation_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(conversation_id) WHERE pipeline_run_id IS NULL
             DO UPDATE SET id = goals.id RETURNING id",
            params![new_id, job.project_id, conversation],
            |row| row.get(0),
        )?
    };
    let revision = Uuid::new_v4().to_string();
    let started = now_ms();
    let deadline = definition.max_seconds.map(|seconds| started + i64::from(seconds) * 1000);
    db.execute(
        "INSERT INTO goal_revisions(id, goal_id, initial_job_id, definition, continuation_owner,
            created_ms, deadline_ms, token_budget_requested)
         VALUES (?1, ?2, ?3, ?4, 'managed', ?5, ?6, ?7)",
        params![revision, goal_id, job.id, protection::protect(&serde_json::to_vec(definition)?)?,
            started, deadline, definition.token_budget.is_some()],
    )?;
    db.execute("INSERT INTO active_goals VALUES (?1, ?2)", params![goal_id, revision])?;
    db.execute("INSERT INTO goal_revision_controls(revision_id, state) VALUES (?1, 'active')", [&revision])?;
    db.execute("INSERT INTO goal_continuations VALUES (?1, 0, ?2)", params![revision, job.id])?;
    Ok((goal_id, revision))
}

fn active_revision(
    db: &Connection,
    conversation: &str,
    stage: Option<&PipelineDispatch>,
) -> Result<Option<String>> {
    if let Some(stage) = stage {
        return Ok(db
            .query_row(
                "SELECT a.revision_id FROM active_goals a JOIN goals g ON g.id = a.goal_id
             WHERE g.pipeline_run_id = ?1 AND g.node_instance_id = ?2",
                params![stage.pipeline_run_id, stage.node_instance_id],
                |row| row.get(0),
            )
            .optional()?);
    }
    Ok(db
        .query_row(
            "SELECT a.revision_id FROM active_goals a JOIN goals g ON g.id = a.goal_id
         WHERE g.conversation_id = ?1 AND g.pipeline_run_id IS NULL",
            [conversation],
            |row| row.get(0),
        )
        .optional()?)
}
