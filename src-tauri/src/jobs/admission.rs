use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    conversations::{
        AttemptHistory, AttemptPlan, SessionScope, consume_matching_draft, insert_conversation,
        insert_downstream_attempt, insert_job_attempt, insert_job_recovery_attempt,
        load_conversation, recovery_checkpoint,
    },
    protection,
    providers::{Capability, SessionMode},
    repository::writers::{WriterConflict, writer_conflict, writer_owner},
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

use super::{
    DispatchRejection, DownstreamWait, DownstreamWaitState, Job, JobContext, JobKind, JobOrigin,
    JobPermission, JobRequest, QueueState, RecoveryAction, ReleaseBlocker,
    command::{CommandAttempt, CommandInvocation, load_command},
    downstream::{continuation_prompt, record_continuation},
    insert_job, load_job,
    pipeline::release_blocker,
    policy::{bind_attempt_policy, dispatch_rejection, effective_policy},
    recovery::{RecoveryCheckpoint, pending_recovery, recovery_source},
};

#[derive(Debug, Clone)]
pub struct AdmissionEnvironment {
    pub runner: String,
    pub bridge_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSettings {
    pub paused: bool,
    pub capacity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/")]
pub enum AdmissionBlocker {
    QueuePaused,
    JobPaused,
    JobCancelled,
    Dependency {
        job_id: String,
        status: Option<RunStatus>,
        queue_state: QueueState,
    },
    Approval {
        approval_id: String,
    },
    Policy {
        reason: &'static str,
    },
    ContextChanged {
        context_id: String,
    },
    ConversationBusy {
        run_id: String,
    },
    Downstream {
        wait_id: String,
        pending: u32,
        needs_review: bool,
    },
    MissingCapability {
        capability: Capability,
    },
    DispatchRejected {
        rejection: DispatchRejection,
    },
    PipelineRelease {
        reason: ReleaseBlocker,
    },
    Writer {
        checkout_id: String,
        run_id: String,
        needs_review: bool,
    },
    AppWriter {
        checkout_id: String,
        operation_id: String,
    },
    PendingWorktreeChanges {
        checkout_id: String,
        scope_task_id: String,
        producing_run_id: Option<String>,
    },
    Capacity {
        active: u32,
        limit: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct JobInspection {
    pub job: Job,
    pub blockers: Vec<AdmissionBlocker>,
    pub recovery_actions: Vec<RecoveryAction>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectQueue {
    pub settings: QueueSettings,
    pub active: u32,
    pub jobs: Vec<JobInspection>,
}

pub(super) enum ScheduledAttempt {
    Agent(Box<AttemptPlan>),
    Command(CommandAttempt),
}

impl ScheduledAttempt {
    fn run(&self) -> &RunRecord {
        match self {
            Self::Agent(plan) => &plan.run,
            Self::Command(attempt) => &attempt.run,
        }
    }
}

impl Store {
    pub async fn enqueue_conversation_turn(
        &self,
        context: JobContext,
        scope: SessionScope,
        text: String,
    ) -> Result<Job> {
        ensure!(
            matches!(scope.mode, SessionMode::ReadOnly | SessionMode::Plan),
            "conversation turns require a read-only or planning profile"
        );
        ensure!(
            scope.executable.is_absolute() && !scope.runner.is_empty(),
            "invalid CLI profile"
        );
        let operator_scope = protection::protect(&serde_json::to_vec(&scope)?)?;
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let conversation = load_conversation(&tx, &context.conversation_id)?;
                ensure!(
                    conversation.context_id == context.context_id,
                    "conversation context changed"
                );
                let (project, root): (String, PathBuf) = tx.query_row(
                    "SELECT project_id, root FROM checkouts WHERE id = ?1",
                    [&conversation.checkout_id],
                    |row| Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?))),
                )?;
                ensure!(
                    root == scope.checkout,
                    "job checkout differs from the conversation checkout"
                );
                let already_pending: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs j WHERE j.conversation_id = ?1
                 AND j.origin_run_id IS NULL AND j.queue_state IN ('queued', 'paused')
                 AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
                      OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = j.id)
                      OR EXISTS (SELECT 1 FROM job_downstream_waits w JOIN job_attempts a ON a.run_id = w.parent_run_id
                          WHERE a.job_id = j.id AND w.state IN ('waiting', 'needs_review'))))",
                    [&context.conversation_id],
                    |row| row.get(0),
                )?;
                ensure!(
                    !already_pending,
                    "this conversation already has a pending job"
                );
                consume_matching_draft(&tx, &context.conversation_id, &text)?;
                let mut request = JobRequest {
                    checkout_id: Some(conversation.checkout_id.clone()),
                    task_id: None,
                    mode: scope.mode,
                    instructions: text,
                    dependencies: Vec::new(),
                    requires_bridge: false,
                };
                request.normalize(&conversation.checkout_id)?;
                let job = insert_job(
                    &tx,
                    project,
                    Some(context),
                    JobOrigin::Operator,
                    request,
                    0,
                    None,
                )?;
                tx.execute(
                    "UPDATE jobs SET operator_scope = ?1 WHERE id = ?2",
                    params![operator_scope, job.id],
                )?;
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }

    pub async fn job_queue_settings(&self) -> Result<QueueSettings> {
        self.execute(|db| queue_settings(db)).await
    }

    pub async fn set_job_queue_settings(&self, settings: QueueSettings) -> Result<()> {
        ensure!(settings.capacity > 0, "queue capacity must be positive");
        self.execute(move |db| {
            db.execute(
                "UPDATE job_queue_settings SET paused = ?1, capacity = ?2 WHERE id = 1",
                params![settings.paused, settings.capacity],
            )?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn set_job_queue_paused(&self, paused: bool) -> Result<QueueSettings> {
        let settings = self
            .execute(move |db| {
                Ok(db.query_row(
                    "UPDATE job_queue_settings SET paused = ?1 WHERE id = 1 RETURNING capacity",
                    [paused],
                    |row| {
                        Ok(QueueSettings {
                            paused,
                            capacity: row.get(0)?,
                        })
                    },
                )?)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(settings)
    }

    pub async fn set_job_queue_capacity(&self, capacity: u32) -> Result<QueueSettings> {
        ensure!(capacity > 0, "queue capacity must be positive");
        let settings = self
            .execute(move |db| {
                Ok(db.query_row(
                    "UPDATE job_queue_settings SET capacity = ?1 WHERE id = 1 RETURNING paused",
                    [capacity],
                    |row| {
                        Ok(QueueSettings {
                            paused: row.get(0)?,
                            capacity,
                        })
                    },
                )?)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(settings)
    }

    pub async fn pending_job_approval_request(
        &self,
        project_id: String,
        approval_id: String,
    ) -> Result<JobRequest> {
        self.execute(move |db| {
            let request: Vec<u8> = db
                .query_row(
                    "SELECT j.request FROM job_approvals p JOIN jobs j ON j.id = p.job_id
                 WHERE j.project_id = ?1 AND p.id = ?2 AND p.decision = 'pending'
                   AND j.queue_state IN ('queued', 'paused')",
                    params![project_id, approval_id],
                    |row| row.get(0),
                )
                .optional()?
                .context("job approval is no longer pending in this project")?;
            Ok(serde_json::from_slice(&protection::unprotect(&request)?)?)
        })
        .await
    }

    pub async fn decide_job_approval(
        &self,
        project_id: String,
        approval_id: String,
        approved: bool,
    ) -> Result<()> {
        self.execute(move |db| {
            let changed = db.execute(
                "UPDATE job_approvals SET decision = ?1 WHERE id = ?2 AND decision = 'pending'
                 AND EXISTS (SELECT 1 FROM jobs WHERE jobs.id = job_approvals.job_id AND project_id = ?3
                             AND queue_state IN ('queued', 'paused'))",
                params![if approved { "approved" } else { "denied" }, approval_id, project_id],
            )?;
            ensure!(changed == 1, "job approval is no longer pending in this project");
            Ok(())
        }).await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn inspect_job(
        &self,
        project: String,
        id: String,
        env: AdmissionEnvironment,
    ) -> Result<Option<JobInspection>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            load_job(&tx, &project, &id)?
                .map(|job| inspect(&tx, job, &env))
                .transpose()
        })
        .await
    }

    pub async fn inspect_project_queue(
        &self,
        project_id: String,
        env: AdmissionEnvironment,
    ) -> Result<ProjectQueue> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let settings = queue_settings(&tx)?;
            let active = occupancy(&tx)?;
            let mut statement = tx.prepare(
                "SELECT id FROM jobs WHERE project_id = ?1 ORDER BY priority DESC, sequence",
            )?;
            let jobs = statement
                .query_map([&project_id], |row| row.get::<_, String>(0))?
                .map(|id| {
                    let job = load_job(&tx, &project_id, &id?)?
                        .context("job disappeared from queue snapshot")?;
                    inspect(&tx, job, &env)
                })
                .collect::<Result<_>>()?;
            Ok(ProjectQueue {
                settings,
                active,
                jobs,
            })
        })
        .await
    }

    pub async fn latest_conversation_job(
        &self,
        conversation: String,
        env: AdmissionEnvironment,
    ) -> Result<Option<JobInspection>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let identity: Option<(String, String)> = tx.query_row(
                "SELECT project_id, id FROM jobs WHERE conversation_id = ?1 AND origin_run_id IS NULL
                 ORDER BY sequence DESC LIMIT 1",
                [conversation], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((project, id)) = identity else { return Ok(None) };
            load_job(&tx, &project, &id)?.map(|job| inspect(&tx, job, &env)).transpose()
        }).await
    }

    pub async fn claim_next_job(&self, env: AdmissionEnvironment) -> Result<Option<AttemptPlan>> {
        Ok(match self.claim_scheduled_attempt(env, false).await? {
            Some(ScheduledAttempt::Agent(plan)) => Some(*plan),
            _ => None,
        })
    }

    pub(super) async fn claim_scheduled_attempt(
        &self,
        env: AdmissionEnvironment,
        include_commands: bool,
    ) -> Result<Option<ScheduledAttempt>> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let settings = queue_settings(&tx)?;
            if settings.paused || occupancy(&tx)? >= settings.capacity {
                return Ok(None);
            }
            let candidates = {
                let mut query = tx.prepare(
                    "SELECT project_id, id FROM jobs j WHERE queue_state = 'queued'
                    AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
                         OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = j.id)
                         OR EXISTS (SELECT 1 FROM job_downstream_waits w JOIN job_attempts a ON a.run_id = w.parent_run_id
                             WHERE a.job_id = j.id AND w.state = 'waiting'))
                    ORDER BY priority DESC, sequence",
                )?;
                query
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (project, id) in candidates {
                let job = load_job(&tx, &project, &id)?.context("pending job disappeared")?;
                if !include_commands && job.kind == JobKind::Command {
                    continue;
                }
                let inputs = load_inputs(&tx, &job)?;
                if !evaluate(&tx, &job, &inputs, &env)?.is_empty() {
                    continue;
                }
                let checkpoint = job
                    .queued_recovery
                    .map(|_| {
                        recovery_source(&tx, &job)?
                            .context("The recovery checkpoint is no longer current")
                    })
                    .transpose()?;
                let plan = build_attempt(&tx, &job, inputs, checkpoint.as_ref())?;
                let wait = job.downstream_wait.as_ref();
                let previous_run = wait.map(|wait| wait.parent_run_id.as_str())
                    .or_else(|| checkpoint.as_ref().map(RecoveryCheckpoint::run_id));
                let ordinal = job
                    .latest_attempt
                    .as_ref()
                    .map_or(1, |previous| previous.ordinal + 1);
                let kind = if wait.is_some() { "resume" } else { job.queued_recovery.map_or("initial", RecoveryAction::key) };
                if checkpoint.is_some() {
                    tx.execute("DELETE FROM job_recoveries WHERE job_id = ?1", [&job.id])?;
                }
                tx.execute(
                    "INSERT INTO job_attempts(job_id, ordinal, run_id, previous_run_id, kind)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![job.id, ordinal, plan.run().id, previous_run, kind],
                )?;
                bind_attempt_policy(&tx, &job, &plan.run().id)?;
                if let Some(wait) = wait {
                    record_continuation(&tx, wait, &plan.run().id)?;
                }
                if job.mode == SessionMode::ApprovedWrite {
                    tx.execute(
                        "INSERT INTO job_writer_reservations VALUES (?1, ?2)",
                        params![job.checkout_id, plan.run().id],
                    )?;
                }
                tx.commit()?;
                return Ok(Some(plan));
            }
            Ok(None)
        })
        .await
    }

    pub async fn request_job_interrupt(
        &self,
        conversation_id: String,
        run_id: String,
    ) -> Result<()> {
        self.execute(move |db| {
            let changed = db.execute(
                "UPDATE job_attempts SET cancel_requested = 1 WHERE run_id = ?1
                 AND EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = ?1 AND conversation_id = ?2)",
                params![run_id, conversation_id],
            )?;
            ensure!(changed == 1, "job attempt is no longer active in this conversation");
            Ok(())
        }).await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn request_project_job_interrupt(
        &self,
        project_id: String,
        job_id: String,
        run_id: String,
    ) -> Result<()> {
        self.execute(move |db| {
            let changed = db.execute(
                "UPDATE job_attempts SET cancel_requested = 1 WHERE run_id = ?1 AND job_id = ?2
                 AND EXISTS (SELECT 1 FROM jobs WHERE id = ?2 AND project_id = ?3)
                 AND EXISTS (SELECT 1 FROM runs WHERE id = ?1 AND status IN ('starting', 'running', 'waiting_for_input'))",
                params![run_id, job_id, project_id],
            )?;
            ensure!(
                changed == 1,
                "job attempt is no longer active in this project"
            );
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }
}

pub(super) fn queue_settings(db: &Connection) -> Result<QueueSettings> {
    Ok(db.query_row(
        "SELECT paused, capacity FROM job_queue_settings WHERE id = 1",
        [],
        |row| {
            Ok(QueueSettings {
                paused: row.get(0)?,
                capacity: row.get(1)?,
            })
        },
    )?)
}

fn occupancy(db: &Connection) -> Result<u32> {
    Ok(db.query_row(
        "SELECT count(*) FROM job_attempts a JOIN runs r ON r.id = a.run_id
         WHERE r.status IN ('starting', 'running', 'waiting_for_input')",
        [],
        |row| row.get(0),
    )?)
}

struct Inputs {
    request: JobRequest,
    scope: Option<SessionScope>,
    command: Option<CommandInvocation>,
}

fn build_attempt(
    db: &Connection,
    job: &Job,
    inputs: Inputs,
    checkpoint: Option<&RecoveryCheckpoint>,
) -> Result<ScheduledAttempt> {
    if let Some(invocation) = inputs.command {
        let (run, _) = runs::insert_run(db, job.checkout_id.clone(), "command")?;
        return Ok(ScheduledAttempt::Command(CommandAttempt {
            run,
            invocation,
        }));
    }
    let scope = inputs
        .scope
        .context("Admitted job has no effective CLI profile")?;
    let mut plan = if let Some(wait) = &job.downstream_wait {
        let conversation =
            downstream_conversation(db, wait)?.context("The downstream wait context changed")?;
        let text = continuation_prompt(db, wait)?;
        let plan = insert_downstream_attempt(db, conversation, scope, &text)?;
        ensure!(
            plan.previous_run_id.as_ref() == Some(&wait.parent_run_id),
            "The continuation lost its waiting attempt"
        );
        plan
    } else {
        match checkpoint {
            Some(RecoveryCheckpoint::Agent(checkpoint)) => {
                let conversation = load_conversation(db, &checkpoint.conversation_id)?;
                let history = match job
                    .queued_recovery
                    .context("The recovery action is missing")?
                {
                    RecoveryAction::Retry => AttemptHistory::Retry {
                        input_context_id: checkpoint.input_context_id.clone(),
                    },
                    RecoveryAction::Resume => AttemptHistory::Resume {
                        native: checkpoint.native_resume(db, &scope)?,
                    },
                };
                let plan = insert_job_recovery_attempt(db, conversation, scope, history)?;
                ensure!(
                    plan.previous_run_id.as_ref() == Some(&checkpoint.source_run_id),
                    "The recovered attempt lost its lineage"
                );
                plan
            }
            Some(RecoveryCheckpoint::Command { .. }) => {
                anyhow::bail!("The job has a command checkpoint")
            }
            None => {
                let conversation = match (&job.origin, job.context.as_ref()) {
                    (JobOrigin::Operator, Some(context)) => {
                        load_conversation(db, &context.conversation_id)?
                    }
                    _ => insert_conversation(
                        db,
                        job.checkout_id.clone(),
                        "Background job".into(),
                        true,
                    )?,
                };
                insert_job_attempt(db, conversation, scope, &inputs.request.instructions)?
            }
        }
    };
    plan.requires_bridge = inputs.request.requires_bridge;
    Ok(ScheduledAttempt::Agent(Box::new(plan)))
}

fn downstream_conversation(
    db: &Connection,
    wait: &DownstreamWait,
) -> Result<Option<crate::conversations::Conversation>> {
    let Some(context) = wait.context_id.as_deref() else {
        return Ok(None);
    };
    let conversation: String = db.query_row(
        "SELECT conversation_id FROM agent_attempts WHERE run_id = ?1",
        [&wait.parent_run_id],
        |r| r.get(0),
    )?;
    if recovery_checkpoint(
        db,
        &conversation,
        &wait.parent_run_id,
        context,
        Some(context),
    )?
    .is_none()
    {
        return Ok(None);
    }
    let busy: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1)",
        [&conversation],
        |r| r.get(0),
    )?;
    if busy {
        return Ok(None);
    }
    Ok(Some(load_conversation(db, &conversation)?))
}

pub(super) fn recovery_scope(
    db: &Connection,
    job: &Job,
) -> Result<super::recovery::RecoveryProfile> {
    use super::recovery::RecoveryProfile;
    let inputs = load_inputs(db, job)?;
    if job.mode == SessionMode::ApprovedWrite {
        let task = job
            .task_id
            .as_deref()
            .context("The write scope is unavailable")?;
        ensure!(
            writer_owner(db, &job.project_id, task, &job.checkout_id)?
                == job.worktree_owner_task_id
                && job.worktree_owner_task_id.is_some(),
            "The task no longer owns this worktree"
        );
    }
    match inputs.command {
        Some(invocation) => Ok(RecoveryProfile::Command(invocation)),
        None => Ok(RecoveryProfile::Agent(
            inputs
                .scope
                .context("The job needs an eligible CLI profile")?,
        )),
    }
}

fn load_inputs(db: &Connection, job: &Job) -> Result<Inputs> {
    let (request, operator_scope): (Vec<u8>, Option<Vec<u8>>) = db.query_row(
        "SELECT request, operator_scope FROM jobs WHERE id = ?1",
        [&job.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let request: JobRequest = serde_json::from_slice(&protection::unprotect(&request)?)?;
    let scope: Option<SessionScope> = match &job.origin {
        JobOrigin::Operator | JobOrigin::Pipeline => operator_scope
            .map(|bytes| {
                Ok::<_, anyhow::Error>(serde_json::from_slice(&protection::unprotect(&bytes)?)?)
            })
            .transpose()?,
        JobOrigin::Agent { run_id, .. } => {
            let serialized: String = db.query_row(
                "SELECT s.scope FROM agent_attempts a
                JOIN agent_sessions s ON s.id = a.agent_session_id WHERE a.run_id = ?1",
                [run_id],
                |row| row.get(0),
            )?;
            let target: String = db.query_row(
                "SELECT root FROM checkouts WHERE id = ?1",
                [&job.checkout_id],
                |row| row.get(0),
            )?;
            let mut scope: SessionScope = serde_json::from_str(&serialized)?;
            // An inherited profile may reduce its permissions. Expanding to a writer needs a write profile.
            if request.mode == SessionMode::ApprovedWrite
                && scope.mode != SessionMode::ApprovedWrite
            {
                None
            } else {
                scope.mode = request.mode;
                scope.checkout = PathBuf::from(target);
                Some(scope)
            }
        }
    };
    let command = if job.kind == JobKind::Command {
        Some(load_command(db, &job.id)?.context("The command invocation is missing")?)
    } else {
        None
    };
    Ok(Inputs {
        request,
        scope,
        command,
    })
}

pub(super) fn inspect(
    db: &Connection,
    job: Job,
    env: &AdmissionEnvironment,
) -> Result<JobInspection> {
    let awaiting_recovery = job.queued_recovery.is_some() || job.downstream_wait.is_some();
    let blockers = if let Some(attempt) = job.latest_attempt.as_ref().filter(|_| !awaiting_recovery)
    {
        dispatch_rejection(db, &attempt.run_id)?
            .map(|rejection| AdmissionBlocker::DispatchRejected { rejection })
            .into_iter()
            .collect()
    } else {
        evaluate(db, &job, &load_inputs(db, &job)?, env)?
    };
    let recovery_actions = if awaiting_recovery {
        Vec::new()
    } else {
        recovery_source(db, &job)?
            .map(|source| source.actions(job.mode))
            .unwrap_or_default()
    };
    Ok(JobInspection {
        job,
        blockers,
        recovery_actions,
    })
}

fn evaluate(
    db: &Connection,
    job: &Job,
    inputs: &Inputs,
    env: &AdmissionEnvironment,
) -> Result<Vec<AdmissionBlocker>> {
    use AdmissionBlocker::*;
    let mut blockers = Vec::new();
    let settings = queue_settings(db)?;
    if settings.paused {
        blockers.push(QueuePaused);
    }
    match job.queue_state {
        QueueState::Queued => {}
        QueueState::Paused => blockers.push(JobPaused),
        QueueState::Cancelled => return Ok(vec![JobCancelled]),
    }
    for id in &inputs.request.dependencies {
        let dependency =
            load_job(db, &job.project_id, id)?.context("job dependency disappeared")?;
        let status = if dependency.downstream_wait.is_some() {
            None
        } else {
            dependency.latest_attempt.map(|attempt| attempt.status)
        };
        if status != Some(RunStatus::Succeeded) {
            blockers.push(Dependency {
                job_id: id.clone(),
                status,
                queue_state: dependency.queue_state,
            });
        }
    }
    let rule = effective_policy(db, job)?;
    let approval: Option<(String, String, String)> = db
        .query_row(
            "SELECT id, decision, policy_key FROM job_approvals WHERE job_id = ?1",
            [&job.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    match approval {
        Some((_, decision, _)) if decision == "denied" => blockers.push(Policy {
            reason: "An operator declined this job",
        }),
        _ if rule.permission == JobPermission::Deny => blockers.push(Policy {
            reason: "A project rule denies this role, operation, and workspace",
        }),
        Some((approval_id, decision, key))
            if rule.permission == JobPermission::Review && key == rule.key =>
        {
            if decision == "pending" {
                blockers.push(Approval { approval_id });
            }
        }
        _ if rule.permission == JobPermission::Review => blockers.push(Policy {
            reason: "The current project rule requires review",
        }),
        _ => {}
    }
    match (&inputs.scope, &inputs.command) {
        (None, None) => blockers.push(Policy {
            reason: "The job needs an eligible CLI permission profile",
        }),
        (Some(scope), _) if scope.runner != env.runner || !scope.executable.is_file() => blockers
            .push(Policy {
                reason: "The job's selected CLI or runner is unavailable",
            }),
        (_, Some(command)) if command.runner != env.runner || !command.executable.is_file() => {
            blockers.push(Policy {
                reason: "The registered command or runner is unavailable",
            });
        }
        _ => {}
    }
    if let Some(wait) = &job.downstream_wait {
        if !wait.ready() {
            blockers.push(Downstream {
                wait_id: wait.id.clone(),
                pending: wait
                    .inputs
                    .iter()
                    .filter(|input| input.status.is_none())
                    .count() as u32,
                needs_review: wait.state == DownstreamWaitState::NeedsReview,
            });
        } else if downstream_conversation(db, wait)?.is_none() {
            blockers.push(ContextChanged {
                context_id: wait
                    .context_id
                    .clone()
                    .context("A waiting job has no recorded context")?,
            });
        }
    } else if let Some(action) = job.queued_recovery {
        let (source_run, context_id) =
            pending_recovery(db, job)?.context("Missing queued recovery")?;
        let matching = recovery_source(db, job)?.is_some_and(|source| {
            source.run_id() == source_run
                && source.context_id() == context_id.as_deref()
                && source.actions(job.mode).contains(&action)
        });
        if !matching {
            blockers.push(match context_id {
                Some(context_id) => ContextChanged { context_id },
                None => Policy {
                    reason: "The command recovery checkpoint changed",
                },
            });
        }
    } else if let (JobOrigin::Operator, Some(context)) = (&job.origin, &job.context) {
        let conversation = load_conversation(db, &context.conversation_id)?;
        if conversation.context_id != context.context_id {
            blockers.push(ContextChanged {
                context_id: context.context_id.clone(),
            });
        }
        if let Some(run_id) = db
            .query_row(
                "SELECT run_id FROM active_agent_attempts WHERE conversation_id = ?1",
                [&context.conversation_id],
                |row| row.get(0),
            )
            .optional()?
        {
            blockers.push(ConversationBusy { run_id });
        }
    }
    if inputs.request.requires_bridge && !env.bridge_available {
        blockers.push(MissingCapability {
            capability: Capability::RunScopedMcp,
        });
    }
    if job.mode == SessionMode::ApprovedWrite {
        if let Some(reason) = release_blocker(db, job)? {
            blockers.push(PipelineRelease { reason });
        }
        let scope = job
            .task_id
            .as_deref()
            .context("the writer has no work scope")?;
        let owner = writer_owner(db, &job.project_id, scope, &job.checkout_id)?;
        if owner.is_none() || owner != job.worktree_owner_task_id {
            blockers.push(Policy {
                reason: "The worktree is no longer assigned to the job's scope",
            });
        }
        let checkout_id = job.checkout_id.clone();
        match writer_conflict(db, &checkout_id, scope)? {
            Some(WriterConflict::Run { id, needs_review }) => blockers.push(Writer {
                checkout_id,
                run_id: id,
                needs_review,
            }),
            Some(WriterConflict::App(operation_id)) => blockers.push(AppWriter {
                checkout_id,
                operation_id,
            }),
            Some(WriterConflict::Pending { scope, run }) => blockers.push(PendingWorktreeChanges {
                checkout_id,
                scope_task_id: scope,
                producing_run_id: run,
            }),
            None => {}
        }
    }
    let active = occupancy(db)?;
    if active >= settings.capacity {
        blockers.push(Capacity {
            active,
            limit: settings.capacity,
        });
    }
    Ok(blockers)
}
