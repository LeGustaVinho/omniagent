use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    conversations::SessionScope, protection, providers::SessionMode,
    repository::writers::writer_owner, runs::RunStatus, store::Store,
};

mod admission;
pub(crate) use admission::queue_conversation_continuation;
mod command;
mod downstream;
mod failures;
mod goal_control;
mod goal_inspection;
mod goal_transition;
mod goal_verification;
mod goals;
mod history;
mod inspection;
mod instruction_delivery;
mod instructions;
pub(crate) use instructions::{FrozenAttachments, freeze_attachments};
mod native_goal_evidence;
mod native_goal_qualification;
mod notices;
mod orchestration;
mod pipeline;
mod policy;
mod question_answers;
mod question_waits;
mod recovery;
mod scheduler;
pub(crate) mod side_requests;
mod steering_delivery;
pub use steering_delivery::InstructionDeliveryEvidence;
mod subagent_audit;
mod subagents;
pub use admission::{
    AdmissionBlocker, AdmissionEnvironment, JobInspection, ProjectQueue, QueueSettings,
};
pub(crate) use command::recover_commands;
pub use command::{CommandJobOutput, PipelineCommand};
pub(crate) use command::{CommandExchange, PinnedCommandScript, PreparedCommand, queue_pipeline_command, queue_pipeline_mcp};
pub use downstream::{DownstreamInput, DownstreamWait, DownstreamWaitState};
pub(crate) use downstream::{observe_child_run, recover_waiting_parent, settle_parent_wait};
pub(crate) use failures::record_job_failure;
pub use failures::{
    FailureCause, FailureObservation, FailurePhase, JobFailure, ObservedProcessExit,
};
pub use goal_control::{GoalControlState, GoalStopAction};
pub use goal_inspection::{ConversationGoalOverview, InspectedGoal};
pub(crate) use goal_transition::{conversation_goal_transition, native_goal_fork_is_safe};
pub use goal_transition::{GoalTransitionReview, GoalTransitionScope};
pub use goal_verification::{
    GoalCriterionEvidence, GoalCriterionStatus, GoalCriterionVerification, GoalEvidenceChoice,
    GoalVerification, GoalVerificationStatus,
};
pub(crate) use goal_verification::load_goal_verification;
pub(crate) use goal_control::{
    pause_revoked_goal, recover_managed_goal_stops, settle_goal_stop,
    settle_owned_goal_cancellations,
};
pub use goals::{
    GoalCheckpoint, GoalContinuationOwner, GoalDefinition, GoalRevision, GoalTurnAction,
    GoalTurnReport,
};
pub(crate) use goals::{
    GoalExecution, accept_goal_checkpoint, delegated_goal_authority_revoked, is_native_goal_run,
    record_native_goal, record_native_goal_turn, recover_native_goal_executors, require_workflow_goal_release,
    settle_native_goal_executor,
};
pub use history::{QueueHistoryPolicy, QueueHistorySettings};
pub(crate) use history::{apply_queue_history_policy, detached_question_context};
pub(crate) use inspection::{InspectionPolicy, InspectionQueue, load_inspection_queue};
pub(crate) use instruction_delivery::validate_revisions as validate_instruction_revisions;
pub(crate) use instructions::migrate_pending_instructions;
pub use instructions::{InstructionDelivery, PendingInstruction};
pub(crate) use native_goal_evidence::{
    late_native_goal_evidence, record_native_goal_control, recover_native_goal_output,
};
pub(crate) use native_goal_qualification::{NativeGoalSeed, candidate as native_goal_candidate, effective_qualification_matches};
pub use notices::{JobNotice, JobNoticeKind, JobNotices};
pub use orchestration::RunOrchestration;
pub use question_answers::ConversationQuestionTimelineEntry;
pub use pipeline::{
    PipelineAgentInput, PipelineDispatch, PipelineJobOrigin, PipelineNodeKind, PipelineRelease,
    ReleaseBlocker,
};
pub(crate) use pipeline::{PipelineReleaseScope, issue_pipeline_release};
pub(crate) use policy::validate_attempt_dispatch;
pub use policy::{
    DispatchRejection, JobPermission, JobRequirements, JobRole, JobRule, JobWorkspace,
    RequiredCustomization,
};
pub(crate) use question_waits::{
    QuestionTimer, archive_saved_question, close_live_question, now_ms, open_native_question,
    recover_question_stop_reservations, refresh_live_question_status, restart_question_waits,
    saved_checkpoint_is_current, settle_question_waits, stage_provider_expiry,
};
pub use question_waits::{QuestionWaitSummary, SavedConversationQuestion};
pub use recovery::RecoveryAction;
pub(crate) use recovery::{requeue_rejected_native_goal, requeue_unlaunched_job};
pub use scheduler::JobScheduler;
pub(crate) use scheduler::LaunchGate;
pub(crate) use side_requests::recover as recover_side_requests;
pub(crate) use side_requests::read_suggestion_mode;
pub use side_requests::{ComposerSuggestionMode, SavedSideRequest, SidePurpose, SideSubmission};
pub(crate) use steering_delivery::{
    SteeringPreparationRejected, accepted_revisions as accepted_steering_revisions,
    record_event as record_steering_event, settle_interrupted as settle_steering_at_stop,
};
pub use subagent_audit::{
    AttemptSubagentAudit, AttemptSubagentRoute, ConversationSubagentAudit, SubagentAuditField,
    SubagentAuditGap, SubagentAuditManagedReason, SubagentAuditPolicy, SubagentAuditPolicySource,
    SubagentAuditSource,
};
pub(crate) use subagent_audit::{
    SubagentAuditRoute, native_audit_fixture_key, pin_attempt_audit, settle_subagent_audit,
};
pub use subagents::{
    ConversationSubagentQuota, NativeSubagentCounts, NativeSubagentSession, ProjectSubagentPolicy,
    SubagentCounts, SubagentLimitMode,
};
pub(crate) use subagents::{
    SubagentAllocation, direct_subagent_allocation, migrate_subagent_observations,
    record_native_subagent, recover_native_subagents, reserve_native_subagents,
    settle_native_subagents,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobRequest {
    pub checkout_id: Option<String>,
    pub task_id: Option<String>,
    pub mode: SessionMode,
    pub instructions: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub requires_bridge: bool,
}

impl JobRequest {
    pub(crate) fn normalize(&mut self, origin_checkout: &str) -> Result<()> {
        ensure!(
            !self.instructions.trim().is_empty(),
            "job instructions are empty"
        );
        self.checkout_id
            .get_or_insert_with(|| origin_checkout.into());
        self.dependencies.sort_unstable();
        self.dependencies.dedup();
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct JobContext {
    pub conversation_id: String,
    pub context_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/")]
pub enum JobOrigin {
    Operator,
    Agent { run_id: String, request_id: String },
    Pipeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum JobKind {
    Agent,
    Command,
    Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum QueueState {
    Queued,
    Paused,
    Cancelled,
}

impl QueueState {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Paused => "paused",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_sql(state: &str) -> Result<Self> {
        match state {
            "queued" => Ok(Self::Queued),
            "paused" => Ok(Self::Paused),
            "cancelled" => Ok(Self::Cancelled),
            _ => anyhow::bail!("unknown job queue state"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct Job {
    pub id: String,
    pub project_id: String,
    pub checkout_id: String,
    pub task_id: Option<String>,
    pub worktree_owner_task_id: Option<String>,
    pub origin: JobOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub operator_requested: Option<bool>,
    pub kind: JobKind,
    pub pipeline: Option<PipelineJobOrigin>,
    pub context: Option<JobContext>,
    pub mode: SessionMode,
    pub priority: i32,
    pub queue_state: QueueState,
    pub latest_attempt: Option<JobAttempt>,
    pub queued_recovery: Option<RecoveryAction>,
    pub downstream_wait: Option<DownstreamWait>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct JobAttempt {
    pub ordinal: u32,
    pub run_id: String,
    pub conversation_id: Option<String>,
    pub status: RunStatus,
    pub cancel_requested: bool,
    pub previous_run_id: Option<String>,
    pub recovery: Option<RecoveryAction>,
}

#[derive(Debug)]
pub struct JobRequestConflict;

impl std::fmt::Display for JobRequestConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The originating run already used this request ID for a different job")
    }
}

impl std::error::Error for JobRequestConflict {}

impl Store {
    pub async fn submit_agent_job(
        &self,
        run_id: String,
        request_id: String,
        request: JobRequest,
    ) -> Result<Job> {
        self.submit_agent_job_with_scope(run_id, request_id, request, None)
            .await
    }

    async fn submit_agent_job_with_scope(
        &self,
        run_id: String,
        request_id: String,
        mut request: JobRequest,
        selected: Option<(String, SessionScope)>,
    ) -> Result<Job> {
        ensure!(
            !request_id.trim().is_empty() && request_id.len() <= 256,
            "job request ID must contain between 1 and 256 bytes"
        );
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let (project, checkout, context, inherited_task, active) = tx
                    .query_row(
                        "SELECT c.project_id, c.id, a.conversation_id, a.context_id,
                        coalesce(scope.scope_task_id, parent.task_id),
                        EXISTS(SELECT 1 FROM active_agent_attempts WHERE run_id = a.run_id)
                            AND NOT coalesce(attempt.cancel_requested, 0)
                 FROM agent_attempts a JOIN runs r ON r.id = a.run_id
                 JOIN checkouts c ON c.id = r.checkout_id
                 LEFT JOIN job_attempts attempt ON attempt.run_id = a.run_id
                 LEFT JOIN jobs parent ON parent.id = attempt.job_id
                 LEFT JOIN job_worktree_scopes scope ON scope.job_id = parent.id
                 WHERE a.run_id = ?1",
                        [&run_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                JobContext {
                                    conversation_id: row.get(2)?,
                                    context_id: row.get(3)?,
                                },
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, bool>(5)?,
                            ))
                        },
                    )
                    .optional()?
                    .context("job origin run is unknown")?;
                request.normalize(&checkout)?;
                request.task_id = request.task_id.or(inherited_task);
                let selected_profile = selected.as_ref().map(|(_, scope)| serde_json::to_vec(scope)).transpose()?;
                let prior: Option<(String, Vec<u8>, Option<Vec<u8>>)> = tx
                    .query_row(
                        "SELECT id, request, operator_scope FROM jobs WHERE origin_run_id = ?1 AND request_id = ?2",
                        params![run_id, request_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                if let Some((id, encrypted, prior_profile)) = prior {
                    let previous: JobRequest =
                        serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
                    let prior_profile = prior_profile.as_deref().map(protection::unprotect).transpose()?;
                    ensure!(previous == request && prior_profile == selected_profile, JobRequestConflict);
                    return load_job(&tx, &project, &id)?
                        .context("job request points to a missing job");
                }
                ensure!(active, "job origin run is no longer active");
                if let Some((selected_project, scope)) = &selected {
                    ensure!(*selected_project == project && request.checkout_id.as_deref() == Some(checkout.as_str()), "The selected parent is outside this project and checkout");
                    validate_operator_scope(&tx, &project, &checkout, scope)?;
                    if scope.mode == SessionMode::ApprovedWrite {
                        let mode: String = tx.query_row(
                            "SELECT json_extract(s.scope, '$.mode') FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id WHERE a.run_id = ?1",
                            [&run_id], |row| row.get(0),
                        )?;
                        ensure!(mode == "approved_write", "The parent profile cannot delegate repository writes");
                    }
                }
                if let Some(reason) = goals::new_delegation_blocker(&tx, &run_id)? {
                    anyhow::bail!(reason);
                }
                let pipeline = pipeline::load_attempt_job(&tx, &run_id)?
                    .and_then(|parent| parent.pipeline)
                    .map(|origin| origin.binding());
                let mut job = insert_job(
                    &tx,
                    project,
                    Some(context),
                    JobOrigin::Agent { run_id, request_id },
                    request,
                    0,
                    pipeline.as_ref(),
                )?;
                if let Some(profile) = selected_profile {
                    tx.execute("UPDATE jobs SET operator_scope = ?2 WHERE id = ?1", params![job.id, protection::protect(&profile)?])?;
                    job.operator_requested = Some(true);
                }
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }

    pub async fn enqueue_selected_agent_job(
        &self,
        project: String,
        checkout: String,
        parent: Option<String>,
        scope: SessionScope,
        instructions: String,
    ) -> Result<Job> {
        let mut request = JobRequest {
            checkout_id: Some(checkout.clone()),
            task_id: None,
            mode: scope.mode,
            instructions,
            dependencies: Vec::new(),
            requires_bridge: false,
        };
        request.normalize(&checkout)?;
        if let Some(parent) = parent {
            return self
                .submit_agent_job_with_scope(
                    parent,
                    Uuid::new_v4().to_string(),
                    request,
                    Some((project, scope)),
                )
                .await;
        }
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let job = insert_selected_operator_job(&tx, project, scope, request)?;
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }

    pub async fn job(&self, project_id: String, id: String) -> Result<Option<Job>> {
        self.execute(move |db| load_job(db, &project_id, &id)).await
    }

    pub(crate) async fn active_run_child_job(
        &self,
        run_id: String,
        id: String,
    ) -> Result<Option<Job>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let project: Option<String> = tx.query_row(
                "SELECT j.project_id FROM jobs j JOIN active_agent_attempts a ON a.run_id = j.origin_run_id
                 WHERE j.id = ?1 AND a.run_id = ?2",
                params![id, run_id],
                |row| row.get(0),
            ).optional()?;
            match project {
                Some(project) => load_job(&tx, &project, &id),
                None => Ok(None),
            }
        }).await
    }

    pub async fn set_pending_job_state(
        &self,
        project_id: String,
        id: String,
        state: QueueState,
    ) -> Result<Job> {
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let job = change_pending_job_state(&tx, &project_id, &id, state)?;
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }
}

fn change_pending_job_state(
    db: &Connection,
    project: &str,
    job_id: &str,
    state: QueueState,
) -> Result<Job> {
    let changed = db.execute(
        "UPDATE jobs SET queue_state = ?3 WHERE project_id = ?1 AND id = ?2
         AND queue_state IN ('queued', 'paused')
         AND NOT EXISTS (SELECT 1 FROM borrowed_instruction_deliveries WHERE job_id = ?2)
         AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = ?2)
              OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = ?2)
              OR EXISTS (SELECT 1 FROM open_agent_questions q JOIN job_attempts a ON a.run_id = q.run_id
                  WHERE a.job_id = ?2 AND q.state IN ('saved', 'needs_review'))
              OR EXISTS (SELECT 1 FROM job_downstream_waits w JOIN job_attempts a ON a.run_id = w.parent_run_id
                  WHERE a.job_id = ?2 AND w.state IN ('waiting', 'needs_review')))",
        params![project, job_id, state.as_sql()],
    )?;
    ensure!(changed == 1, "job is not pending in this project");
    if state == QueueState::Cancelled {
        db.execute("DELETE FROM job_recoveries WHERE job_id = ?1", [job_id])?;
        db.execute(
            "UPDATE job_downstream_waits SET state = 'cancelled'
             WHERE state IN ('waiting', 'needs_review') AND parent_run_id IN (
                 SELECT run_id FROM job_attempts WHERE job_id = ?1
             )",
            [job_id],
        )?;
    }
    let job = load_job(db, project, job_id)?.context("job no longer exists")?;
    if state == QueueState::Cancelled {
        goals::deactivate_cancelled_job(db, job_id)?;
        downstream::observe_cancelled_job(db, &job)?;
    }
    Ok(job)
}

pub(crate) fn insert_selected_operator_job(
    db: &Connection,
    project: String,
    scope: SessionScope,
    mut request: JobRequest,
) -> Result<Job> {
    let checkout = request.checkout_id.as_deref().context("job checkout was not selected")?;
    validate_operator_scope(db, &project, checkout, &scope)?;
    if scope.mode == SessionMode::ApprovedWrite {
        request.task_id = db.query_row(
            "SELECT task_id FROM managed_task_worktrees WHERE project_id = ?1 AND checkout_id = ?2",
            params![project, checkout], |row| row.get(0),
        ).optional()?;
    }
    let job = insert_job(db, project, None, JobOrigin::Operator, request, 0, None)?;
    db.execute(
        "UPDATE jobs SET operator_scope = ?2 WHERE id = ?1",
        params![job.id, protection::protect(&serde_json::to_vec(&scope)?)?],
    )?;
    Ok(job)
}

fn validate_operator_scope(
    db: &Connection,
    project: &str,
    checkout: &str,
    scope: &SessionScope,
) -> Result<()> {
    let root: String = db
        .query_row(
            "SELECT root FROM checkouts WHERE id = ?1 AND project_id = ?2",
            params![checkout, project],
            |row| row.get(0),
        )
        .context("The checkout is unavailable in this project")?;
    ensure!(
        scope.checkout == std::path::Path::new(&root)
            && scope.executable.is_absolute()
            && !scope.runner.is_empty(),
        "The selected CLI profile does not match this checkout"
    );
    Ok(())
}

fn mode_key(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::ReadOnly => "read_only",
        SessionMode::Plan => "plan",
        SessionMode::ApprovedWrite => "approved_write",
    }
}

fn read_mode(value: &str) -> Result<SessionMode> {
    match value {
        "read_only" => Ok(SessionMode::ReadOnly),
        "plan" => Ok(SessionMode::Plan),
        "approved_write" => Ok(SessionMode::ApprovedWrite),
        _ => anyhow::bail!("unknown job mode"),
    }
}

fn insert_job(
    db: &Connection,
    project_id: String,
    context: Option<JobContext>,
    origin: JobOrigin,
    request: JobRequest,
    priority: i32,
    pipeline_binding: Option<&PipelineDispatch>,
) -> Result<Job> {
    let encrypted = protection::protect(&serde_json::to_vec(&request)?)?;
    let checkout_id = request
        .checkout_id
        .context("job checkout was not resolved")?;
    let valid_checkout: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM checkouts WHERE project_id = ?1 AND id = ?2)",
        params![project_id, checkout_id],
        |row| row.get(0),
    )?;
    ensure!(valid_checkout, "job checkout is outside the project");
    if let Some(task_id) = &request.task_id {
        let valid_task: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_tasks WHERE project_id = ?1 AND id = ?2)",
            params![project_id, task_id],
            |row| row.get(0),
        )?;
        ensure!(valid_task, "job task is outside the project");
    }
    let writer_owner = if request.mode == SessionMode::ApprovedWrite {
        let scope = request
            .task_id
            .as_deref()
            .context("a repository writer requires a task")?;
        Some(
            writer_owner(db, &project_id, scope, &checkout_id)?
                .context("a repository-writing job requires an assigned managed worktree")?,
        )
    } else {
        None
    };
    let writer_checkout = writer_owner.as_ref().map(|_| &checkout_id);
    let stored_task = writer_owner.as_ref().or(request.task_id.as_ref());
    let (run_id, request_id) = match &origin {
        JobOrigin::Operator | JobOrigin::Pipeline => (None, None),
        JobOrigin::Agent { run_id, request_id } => (Some(run_id), Some(request_id)),
    };
    let id = Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO jobs(id, project_id, checkout_id, task_id, writer_checkout_id, conversation_id,
             context_id, origin_run_id, request_id, mode, request, priority)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![id, project_id, checkout_id, stored_task, writer_checkout,
            context.as_ref().map(|context| &context.conversation_id), context.as_ref().map(|context| &context.context_id),
            run_id, request_id, mode_key(request.mode), encrypted, priority],
    )?;
    if let Some(run) = run_id {
        subagents::record_delegation_owner(db, &id, run)?;
    }
    if let Some(binding) = pipeline_binding {
        pipeline::bind_job(db, &project_id, &id, binding)?;
    }
    if let (Some(owner), Some(scope)) = (&writer_owner, &request.task_id)
        && owner != scope
    {
        db.execute(
            "INSERT INTO job_worktree_scopes VALUES (?1, ?2, ?3)",
            params![id, project_id, scope],
        )?;
    }
    for dependency in request.dependencies {
        let exists = load_job(db, &project_id, &dependency)?.is_some();
        ensure!(exists, "job dependency is outside the project");
        db.execute(
            "INSERT INTO job_dependencies VALUES (?1, ?2, ?3)",
            params![project_id, id, dependency],
        )?;
    }
    let job = load_job(db, &project_id, &id)?.context("inserted job could not be loaded")?;
    policy::reconcile_approval(db, &job)?;
    Ok(job)
}

pub(crate) fn load_job(db: &Connection, project_id: &str, id: &str) -> Result<Option<Job>> {
    let mut query = db.prepare(
        "SELECT j.checkout_id, coalesce(scope.scope_task_id, j.task_id),
                j.conversation_id, j.context_id, j.origin_run_id, j.request_id,
                j.mode, j.priority, j.queue_state,
                coalesce(a.ordinal, CASE WHEN instruction.run_id IS NOT NULL THEN 1 END),
                coalesce(a.run_id, instruction.run_id),
                CASE WHEN instruction.uncertain THEN 'interrupted' ELSE r.status END,
                coalesce(a.cancel_requested, 0),
                a.previous_run_id, coalesce(a.kind, 'initial'), recovery.action,
                CASE WHEN j.writer_checkout_id IS NOT NULL THEN j.task_id END, attempt.conversation_id,
                EXISTS(SELECT 1 FROM job_commands command WHERE command.job_id = j.id),
                instruction.run_id IS NOT NULL, j.origin_run_id IS NOT NULL AND j.operator_scope IS NOT NULL,
                EXISTS(SELECT 1 FROM job_side_requests side WHERE side.job_id = j.id)
         FROM jobs j
         LEFT JOIN job_worktree_scopes scope ON scope.job_id = j.id
         LEFT JOIN job_attempts a ON a.job_id = j.id AND a.ordinal = (
             SELECT max(ordinal) FROM job_attempts WHERE job_id = j.id
         )
         LEFT JOIN borrowed_instruction_deliveries instruction ON instruction.job_id = j.id
         LEFT JOIN runs r ON r.id = coalesce(a.run_id, instruction.run_id)
         LEFT JOIN agent_attempts attempt ON attempt.run_id = r.id
         LEFT JOIN job_recoveries recovery ON recovery.job_id = j.id
         WHERE j.project_id = ?1 AND j.id = ?2",
    )?;
    let mut rows = query.query(params![project_id, id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let pipeline = pipeline::load_origin(db, project_id, id)?;
    let origin = match row.get::<_, Option<String>>(4)? {
        Some(run_id) => JobOrigin::Agent {
            run_id,
            request_id: row.get(5)?,
        },
        None if pipeline.is_some() => JobOrigin::Pipeline,
        None => JobOrigin::Operator,
    };
    let context = match row.get::<_, Option<String>>(2)? {
        Some(conversation_id) => Some(JobContext {
            conversation_id,
            context_id: row.get(3)?,
        }),
        None => None,
    };
    let latest_attempt = match row.get::<_, Option<u32>>(9)? {
        Some(ordinal) => {
            let status = RunStatus::from_sql(row.get_ref(11)?.as_str()?)?;
            Some(JobAttempt {
                ordinal,
                run_id: row.get(10)?,
                conversation_id: row.get(17)?,
                status,
                cancel_requested: row.get::<_, bool>(12)? && !status.is_terminal(),
                previous_run_id: row.get(13)?,
                recovery: RecoveryAction::from_attempt_kind(row.get_ref(14)?.as_str()?)?,
            })
        }
        None => None,
    };
    let absorbed = row.get::<_, bool>(19)?;
    let downstream_wait = match latest_attempt.as_ref() {
        Some(attempt) if !absorbed => downstream::load_wait(db, &attempt.run_id)?.filter(|wait| {
            matches!(
                wait.state,
                DownstreamWaitState::Yielding
                    | DownstreamWaitState::Waiting
                    | DownstreamWaitState::NeedsReview
            )
        }),
        _ => None,
    };
    Ok(Some(Job {
        id: id.into(),
        project_id: project_id.into(),
        checkout_id: row.get(0)?,
        task_id: row.get(1)?,
        worktree_owner_task_id: row.get(16)?,
        origin,
        operator_requested: row.get::<_, bool>(20)?.then_some(true),
        kind: if row.get(21)? {
            JobKind::Side
        } else if row.get(18)? {
            JobKind::Command
        } else {
            JobKind::Agent
        },
        pipeline,
        context,
        mode: read_mode(row.get_ref(6)?.as_str()?)?,
        priority: row.get(7)?,
        queue_state: QueueState::from_sql(row.get_ref(8)?.as_str()?)?,
        latest_attempt,
        downstream_wait,
        queued_recovery: row
            .get::<_, Option<String>>(15)?
            .as_deref()
            .map(RecoveryAction::from_key)
            .transpose()?,
    }))
}

pub(crate) fn release_settled_writer(db: &Connection, run_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM job_writer_reservations WHERE run_id = ?1
         AND (EXISTS (SELECT 1 FROM job_attempts WHERE run_id = ?1 AND process_launch_requested = 0)
           OR EXISTS (SELECT 1 FROM job_worktree_candidates WHERE run_id = ?1 AND capture_confirmed = 1))",
        [run_id],
    )?;
    Ok(())
}

pub(crate) fn record_launch_intent(db: &Connection, run_id: &str) -> Result<Option<bool>> {
    let cancelled: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM job_attempts WHERE run_id = ?1 AND cancel_requested = 1)",
        [run_id],
        |row| row.get(0),
    )?;
    if cancelled {
        return Ok(Some(false));
    }
    if !validate_attempt_dispatch(db, run_id)? {
        return Ok(None);
    }
    record_process_launch(db, run_id)?;
    Ok(Some(true))
}

pub(crate) fn record_process_launch(db: &Connection, run_id: &str) -> Result<()> {
    db.execute(
        "UPDATE job_attempts SET process_launch_requested = 1 WHERE run_id = ?1",
        [run_id],
    )?;
    Ok(())
}
