use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    protection, providers::SessionMode, repository::writers::writer_owner, runs::RunStatus,
    store::Store,
};

mod admission;
mod command;
mod downstream;
mod failures;
mod history;
mod inspection;
mod notices;
mod pipeline;
mod policy;
mod recovery;
mod scheduler;
mod subagents;
pub use admission::{
    AdmissionBlocker, AdmissionEnvironment, JobInspection, ProjectQueue, QueueSettings,
};
pub(crate) use command::recover_commands;
pub use command::{CommandJobOutput, PipelineCommand};
pub use downstream::{DownstreamInput, DownstreamWait, DownstreamWaitState};
pub(crate) use downstream::{observe_child_run, recover_waiting_parent, settle_parent_wait};
pub(crate) use failures::record_job_failure;
pub use failures::{
    FailureCause, FailureObservation, FailurePhase, JobFailure, ObservedProcessExit,
};
pub use history::{QueueHistoryPolicy, QueueHistorySettings};
pub(crate) use history::{apply_queue_history_policy, detached_question_context};
pub(crate) use inspection::{InspectionPolicy, InspectionQueue, load_inspection_queue};
pub use notices::{JobNotice, JobNoticeKind, JobNotices};
pub use pipeline::{
    PipelineDispatch, PipelineJobOrigin, PipelineNodeKind, PipelineRelease, ReleaseBlocker,
};
pub(crate) use policy::validate_attempt_policy;
pub use policy::{
    DispatchRejection, JobPermission, JobRequirements, JobRole, JobRule, JobWorkspace,
    RequiredCustomization,
};
pub use recovery::RecoveryAction;
pub(crate) use recovery::requeue_unlaunched_job;
pub use scheduler::JobScheduler;
pub(crate) use scheduler::LaunchGate;
pub use subagents::{
    ConversationSubagentQuota, NativeSubagentCounts, ProjectSubagentPolicy, SubagentCounts,
    SubagentLimitMode,
};
pub(crate) use subagents::{
    check_conversation_subagent_controls, migrate_subagent_observations, record_native_subagent,
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
    fn normalize(&mut self, origin_checkout: &str) -> Result<()> {
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
        mut request: JobRequest,
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
                let prior: Option<(String, Vec<u8>)> = tx
                    .query_row(
                        "SELECT id, request FROM jobs WHERE origin_run_id = ?1 AND request_id = ?2",
                        params![run_id, request_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((id, encrypted)) = prior {
                    let previous: JobRequest =
                        serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
                    ensure!(previous == request, JobRequestConflict);
                    return load_job(&tx, &project, &id)?
                        .context("job request points to a missing job");
                }
                ensure!(active, "job origin run is no longer active");
                let pipeline = pipeline::load_attempt_job(&tx, &run_id)?
                    .and_then(|parent| parent.pipeline)
                    .map(|origin| origin.binding());
                let job = insert_job(
                    &tx,
                    project,
                    Some(context),
                    JobOrigin::Agent { run_id, request_id },
                    request,
                    0,
                    pipeline.as_ref(),
                )?;
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
         AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = ?2)
              OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = ?2)
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
        downstream::observe_cancelled_job(db, &job)?;
    }
    Ok(job)
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

fn load_job(db: &Connection, project_id: &str, id: &str) -> Result<Option<Job>> {
    let mut query = db.prepare(
        "SELECT j.checkout_id, coalesce(scope.scope_task_id, j.task_id),
                j.conversation_id, j.context_id, j.origin_run_id, j.request_id,
                j.mode, j.priority, j.queue_state, a.ordinal, a.run_id, r.status, a.cancel_requested,
                a.previous_run_id, a.kind, recovery.action,
                CASE WHEN j.writer_checkout_id IS NOT NULL THEN j.task_id END, attempt.conversation_id,
                EXISTS(SELECT 1 FROM job_commands command WHERE command.job_id = j.id)
         FROM jobs j
         LEFT JOIN job_worktree_scopes scope ON scope.job_id = j.id
         LEFT JOIN job_attempts a ON a.job_id = j.id AND a.ordinal = (
             SELECT max(ordinal) FROM job_attempts WHERE job_id = j.id
         )
         LEFT JOIN runs r ON r.id = a.run_id
         LEFT JOIN agent_attempts attempt ON attempt.run_id = a.run_id
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
    let downstream_wait = match latest_attempt.as_ref() {
        Some(attempt) => downstream::load_wait(db, &attempt.run_id)?.filter(|wait| {
            matches!(
                wait.state,
                DownstreamWaitState::Yielding
                    | DownstreamWaitState::Waiting
                    | DownstreamWaitState::NeedsReview
            )
        }),
        None => None,
    };
    Ok(Some(Job {
        id: id.into(),
        project_id: project_id.into(),
        checkout_id: row.get(0)?,
        task_id: row.get(1)?,
        worktree_owner_task_id: row.get(16)?,
        origin,
        kind: if row.get(18)? {
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
    if !validate_attempt_policy(db, run_id)? {
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
