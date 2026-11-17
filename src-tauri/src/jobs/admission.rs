use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    attachments::AttachmentPreview,
    conversations::{
        AttemptHistory, AttemptPlan, Conversation, RecordedAttempt, SessionScope,
        consume_matching_draft,
        continuation::{continuation_pin, pinned_continuation},
        insert_conversation, insert_downstream_attempt, insert_instruction_attempt,
        insert_job_attempt, insert_job_recovery_attempt, insert_recorded_job_attempt,
        load_conversation, recovery_checkpoint,
    },
    protection,
    providers::{Capability, SessionMode, questions::QuestionRoute, sandbox},
    repository::writers::{WriterConflict, writer_conflict, writer_owner},
    runner::{DirectWindowsRunner, Runner},
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

use super::{
    DispatchRejection, DownstreamWait, DownstreamWaitState, Job, JobContext, JobKind, JobOrigin,
    JobPermission, JobRequest, QueueState, RecoveryAction, ReleaseBlocker,
    command::{CommandAttempt, CommandInvocation, load_command},
    downstream::{continuation_prompt, record_continuation},
    goals::{self, GoalBoundary},
    history::{effective_release_blocker, load_binding, uses_recorded_checkpoint},
    insert_job,
    instruction_delivery::{self, InstructionClaim},
    instructions::{self, QueuedRevision},
    load_job,
    policy::{bind_attempt_policy, dispatch_rejection, effective_policy},
    question_answers::{self, QuestionContinuation, load_continuation},
    question_waits::{QuestionWaitSummary, has_open_questions, question_wait_summary},
    recovery::{RecoveryCheckpoint, pending_recovery, pending_writer_checkpoint, recovery_source},
    side_requests::{self, SideAttempt, SidePayload},
    subagents::{SubagentAllocation, job_subagent_allocation},
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
    InstructionOrder {
        job_id: String,
    },
    ContinuationOwner {
        run_id: String,
    },
    InstructionDeliveryUncertain {
        run_id: String,
    },
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
    SubagentQuota {
        conversation_id: String,
        active: u32,
        limit: u32,
    },
    SubagentAudit {
        missing: Vec<super::SubagentAuditField>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct JobInspection {
    pub job: Job,
    pub blockers: Vec<AdmissionBlocker>,
    pub recovery_actions: Vec<RecoveryAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub question_wait: Option<QuestionWaitSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub queued_question: Option<bool>,
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
    Side(Box<SideAttempt>),
}

impl ScheduledAttempt {
    fn run(&self) -> &RunRecord {
        match self {
            Self::Agent(plan) => &plan.run,
            Self::Command(attempt) => &attempt.run,
            Self::Side(attempt) => &attempt.run,
        }
    }
}

fn queue_conversation_instruction(
    db: &Connection,
    project: String,
    checkout_id: &str,
    context: JobContext,
    scope: &SessionScope,
    text: &str,
    attachments: Option<&instructions::FrozenAttachments>,
) -> Result<Job> {
    let mut request = JobRequest {
        checkout_id: Some(checkout_id.into()),
        task_id: None,
        mode: scope.mode,
        instructions: text.into(),
        dependencies: Vec::new(),
        requires_bridge: false,
    };
    request.normalize(checkout_id)?;
    let conversation_id = context.conversation_id.clone();
    let job = insert_job(
        db,
        project,
        Some(context),
        JobOrigin::Operator,
        request,
        0,
        None,
    )?;
    let operator_scope = protection::protect(&serde_json::to_vec(scope)?)?;
    db.execute(
        "UPDATE jobs SET operator_scope = ?1 WHERE id = ?2",
        params![operator_scope, job.id],
    )?;
    instructions::record_instruction(db, &job.id, &conversation_id, text, attachments)?;
    let revision = instructions::load_revision(db, &job.id)?
        .context("The queued instruction has no revision")?;
    crate::workflows::conversation_turns::launch_direct_turn(
        db,
        &job,
        scope,
        Some(&revision.revision_id),
    )?;
    Ok(job)
}

pub(crate) fn queue_conversation_continuation(
    db: &Connection,
    project: String,
    conversation: &Conversation,
    expected_context_id: &str,
    scope: &SessionScope,
    text: &str,
    reference_ids: &[String],
    attachments: Option<&instructions::FrozenAttachments>,
) -> Result<Job> {
    ensure!(
        instructions::preserves_destination(
            db,
            &conversation.id,
            expected_context_id,
            &conversation.context_id
        )?,
        "conversation context changed"
    );
    let context = JobContext {
        conversation_id: conversation.id.clone(),
        context_id: expected_context_id.into(),
    };
    let job = queue_conversation_instruction(
        db,
        project,
        &conversation.checkout_id,
        context,
        scope,
        text,
        attachments,
    )?;
    if !reference_ids.is_empty() {
        let revision = instructions::load_revision(db, &job.id)?
            .context("Queued conversation lost its revision")?;
        crate::conversations::reference_delivery::bind_references(
            db,
            &revision.revision_id,
            reference_ids,
        )?;
    }
    Ok(job)
}

impl Store {
    pub async fn enqueue_conversation_turn(
        &self,
        context: JobContext,
        scope: SessionScope,
        text: String,
    ) -> Result<Job> {
        self.enqueue_conversation_turn_with_attachments(context, scope, text, Vec::new())
            .await
    }

    pub async fn enqueue_conversation_turn_with_attachments(
        &self,
        context: JobContext,
        scope: SessionScope,
        text: String,
        previews: Vec<AttachmentPreview>,
    ) -> Result<Job> {
        ensure!(
            matches!(scope.mode, SessionMode::ReadOnly | SessionMode::Plan),
            "conversation turns require a read-only or planning profile"
        );
        ensure!(
            scope.executable.is_absolute() && !scope.runner.is_empty(),
            "invalid CLI profile"
        );
        let attachment_checkout = self
            .conversation(context.conversation_id.clone())
            .await?
            .checkout_id;
        let attachments = instructions::freeze_attachments(
            self,
            attachment_checkout.clone(),
            &scope.checkout,
            previews,
        )
        .await?;
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let conversation = load_conversation(&tx, &context.conversation_id)?;
                conversation.require_direct_turn()?;
                ensure!(
                    instructions::preserves_destination(
                        &tx,
                        &conversation.id,
                        &context.context_id,
                        &conversation.context_id
                    )?,
                    "conversation context changed"
                );
                let (project, root): (String, PathBuf) = tx.query_row(
                    "SELECT project_id, root FROM checkouts WHERE id = ?1",
                    [&conversation.checkout_id],
                    |row| Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?))),
                )?;
                ensure!(
                    conversation.checkout_id == attachment_checkout && root == scope.checkout,
                    "job checkout differs from the conversation checkout"
                );
                consume_matching_draft(&tx, &context.conversation_id, &text)?;
                let job = queue_conversation_instruction(
                    &tx,
                    project,
                    &conversation.checkout_id,
                    context,
                    &scope,
                    &text,
                    attachments.as_ref(),
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
            let (job_id, request): (String, Vec<u8>) = db
                .query_row(
                    "SELECT j.id, j.request FROM job_approvals p JOIN jobs j ON j.id = p.job_id
                 WHERE j.project_id = ?1 AND p.id = ?2 AND p.decision = 'pending'
                   AND j.queue_state IN ('queued', 'paused')",
                    params![project_id, approval_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .context("job approval is no longer pending in this project")?;
            let job = load_job(db, &project_id, &job_id)?.context("Job approval lost its job")?;
            instructions::current_request(db, &job, &request)
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
                "SELECT project_id, id FROM jobs j WHERE conversation_id = ?1 AND origin_run_id IS NULL
                 AND NOT EXISTS (SELECT 1 FROM job_history_bindings WHERE job_id = j.id)
                 ORDER BY EXISTS (
                    SELECT 1 FROM job_attempts a JOIN active_agent_attempts active ON active.run_id = a.run_id
                    WHERE a.job_id = j.id
                 ) DESC, sequence DESC LIMIT 1",
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
                    AND NOT EXISTS (SELECT 1 FROM borrowed_instruction_deliveries WHERE job_id = j.id)
                    AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
                         OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = j.id)
                         OR EXISTS (SELECT 1 FROM ready_question_continuations WHERE job_id = j.id)
                         OR EXISTS (SELECT 1 FROM job_downstream_waits w JOIN job_attempts a ON a.run_id = w.parent_run_id
                             WHERE a.job_id = j.id AND w.state = 'waiting'))
                    ORDER BY CASE (SELECT purpose FROM job_side_requests WHERE job_id = j.id)
                        WHEN 'suggestion' THEN 2 WHEN 'question' THEN 1 ELSE 0 END, priority DESC, sequence",
                )?;
                query
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (project, id) in candidates {
                let job = load_job(&tx, &project, &id)?.context("pending job disappeared")?;
                if !include_commands && job.kind != JobKind::Agent {
                    continue;
                }
                let inputs = load_inputs(&tx, &job)?;
                let (blockers, allocation) = evaluate(&tx, &job, &inputs, &env)?;
                if !blockers.is_empty() {
                    continue;
                }
                let checkpoint = job
                    .queued_recovery
                    .map(|_| {
                        recovery_source(&tx, &job)?
                            .context("The recovery checkpoint is no longer current")
                    })
                    .transpose()?;
                if checkpoint.is_some() {
                    crate::workflows::conversation_turns::reopen_direct_turn(&tx, &job)?;
                }
                let automatic_writer_checkpoint = if job.mode == SessionMode::ApprovedWrite
                    && let Some(goal) = &inputs.goal
                    && crate::workflows::load_workflow_job_input(&tx, &job.id)?.is_some()
                {
                    Some(goal.run.clone())
                } else {
                    None
                };
                let prior_scope = super::goal_requalification::initial_checkpoint(&tx, &job)?;
                let writer_checkpoint = pending_writer_checkpoint(&tx, &job)?.or(automatic_writer_checkpoint)
                    .or_else(|| prior_scope.as_ref().filter(|scope| scope.same_worktree).map(|scope| scope.run.clone()));
                let wait = job.downstream_wait.as_ref();
                let question_run = inputs.question.as_ref().map(|question| question.run.clone());
                let goal_run = inputs.goal.as_ref().map(|boundary| boundary.run.clone());
                let adopted = continuation_instruction(&tx, &job, &inputs, &env)?;
                let standalone = if job.latest_attempt.is_none() {
                    inputs.instruction.as_ref().map(|instruction| -> Result<_> {
                        Ok(InstructionClaim {
                            job_id: job.id.clone(),
                            revision_id: instruction.revision_id.clone(),
                            policy_key: effective_policy(&tx, &job)?.key,
                            route: "standalone",
                        })
                    }).transpose()?
                } else {
                    None
                };
                if let Some(claim) = adopted.as_ref().or(standalone.as_ref())
                    && let Some(context) = &job.context
                {
                    crate::workflows::conversation_turns::revise_direct_turn_input(
                        &tx, &job.project_id, &context.conversation_id, &claim.job_id, &claim.revision_id,
                    )?;
                }
                let mut plan = build_attempt(
                    &tx, &job, inputs, checkpoint.as_ref(), allocation,
                    adopted.as_ref().map(|claim| claim.revision_id.clone()),
                )?;
                let previous_run = question_run.as_deref()
                    .or_else(|| wait.map(|wait| wait.parent_run_id.as_str()))
                    .or_else(|| checkpoint.as_ref().map(RecoveryCheckpoint::run_id))
                    .or(goal_run.as_deref()).or_else(|| prior_scope.as_ref().map(|scope| scope.run.as_str()));
                if let Some(scope) = &prior_scope && let ScheduledAttempt::Agent(agent) = &mut plan {
                    agent.previous_run_id = Some(scope.run.clone());
                }
                let ordinal = job
                    .latest_attempt
                    .as_ref()
                    .map_or(1, |previous| previous.ordinal + 1);
                let kind = if wait.is_some() || question_run.is_some() { "resume" } else { job.queued_recovery.map_or("initial", RecoveryAction::key) };
                if checkpoint.is_some() {
                    tx.execute("DELETE FROM job_recoveries WHERE job_id = ?1", [&job.id])?;
                }
                tx.execute(
                    "INSERT INTO job_attempts(job_id, ordinal, run_id, previous_run_id, kind)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![job.id, ordinal, plan.run().id, previous_run, kind],
                )?;
                if let Some(writer_checkpoint) = writer_checkpoint {
                    tx.execute(
                        "INSERT INTO goal_writer_attempts(run_id, checkpoint_run_id) VALUES (?1, ?2)",
                        params![plan.run().id, writer_checkpoint],
                    )?;
                }
                bind_attempt_policy(&tx, &job, &plan.run().id)?;
                if let ScheduledAttempt::Agent(plan) = &plan {
                    if plan.native_goal_seed.is_none() {
                        tx.execute("DELETE FROM native_goal_resume_checkpoints WHERE job_id = ?1", [&job.id])?;
                    }
                    if let Some(claim) = adopted.or(standalone) {
                        claim.record(&tx, plan)?;
                    } else if let Some(checkpoint) = &checkpoint {
                        instruction_delivery::carry_unsent_instruction(&tx, checkpoint.run_id(), plan)?;
                    }
                }
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
        "SELECT count(*) FROM (
            SELECT a.run_id FROM job_attempts a JOIN runs r ON r.id = a.run_id
            WHERE r.status IN ('starting', 'running', 'waiting_for_input')
            UNION SELECT run_id FROM unconfirmed_agent_stops
         )",
        [],
        |row| row.get(0),
    )?)
}

struct Inputs {
    request: JobRequest,
    scope: Option<SessionScope>,
    command: Option<CommandInvocation>,
    side: Option<SidePayload>,
    question: Option<QuestionContinuation>,
    instruction: Option<QueuedRevision>,
    goal: Option<GoalBoundary>,
    native_seed: Option<super::NativeGoalSeed>,
}

fn build_attempt(
    db: &Connection,
    job: &Job,
    inputs: Inputs,
    checkpoint: Option<&RecoveryCheckpoint>,
    allocation: SubagentAllocation,
    instruction_revision: Option<String>,
) -> Result<ScheduledAttempt> {
    if let Some(payload) = inputs.side {
        let (run, _) = runs::insert_run(db, job.checkout_id.clone(), "agent")?;
        return Ok(ScheduledAttempt::Side(Box::new(SideAttempt {
            run,
            job_id: job.id.clone(),
            payload,
        })));
    }
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
    let mut plan = if let Some(question) = &inputs.question {
        let downstream = job
            .downstream_wait
            .as_ref()
            .map(|wait| continuation_prompt(db, wait))
            .transpose()?;
        question_answers::insert_attempt(
            db,
            question,
            scope,
            downstream,
            instruction_revision,
            allocation,
        )?
    } else if let Some(wait) = &job.downstream_wait {
        let (conversation, recorded) = downstream_conversation(db, job, wait)?
            .context("The downstream wait context changed")?;
        let text = continuation_prompt(db, wait)?;
        let plan = insert_downstream_attempt(
            db,
            conversation,
            scope,
            recorded,
            &text,
            instruction_revision,
            allocation,
        )?;
        ensure!(
            plan.previous_run_id.as_ref() == Some(&wait.parent_run_id),
            "The continuation lost its waiting attempt"
        );
        plan
    } else if let Some(boundary) = inputs.goal {
        let conversation = load_conversation(db, &boundary.conversation)?;
        let recorded = load_binding(db, &job.id)?.map(|binding| RecordedAttempt {
            context_id: boundary.context,
            previous_run_id: Some(boundary.run.clone()),
            question_route: binding.question_route,
        });
        let plan = insert_downstream_attempt(
            db,
            conversation,
            scope,
            recorded,
            &inputs.request.instructions,
            instruction_revision,
            allocation,
        )?;
        ensure!(
            plan.previous_run_id.as_deref() == Some(&boundary.run),
            "The goal checkpoint is no longer current"
        );
        plan
    } else {
        match checkpoint {
            Some(RecoveryCheckpoint::Agent(checkpoint)) => {
                let conversation = load_conversation(db, &checkpoint.conversation_id)?;
                let question_route = load_binding(db, &job.id)?
                    .map_or(QuestionRoute::Native, |binding| binding.question_route);
                let recorded = checkpoint.recorded.then(|| RecordedAttempt {
                    context_id: checkpoint.result_context_id.clone(),
                    previous_run_id: Some(checkpoint.source_run_id.clone()),
                    question_route,
                });
                let history = match job
                    .queued_recovery
                    .context("The recovery action is missing")?
                {
                    RecoveryAction::Retry => AttemptHistory::Retry {
                        input_context_id: checkpoint.input_context_id.clone(),
                    },
                    RecoveryAction::Resume => AttemptHistory::Resume {
                        native: if recorded.is_some()
                            || pending_writer_checkpoint(db, job)?.is_some()
                        {
                            None
                        } else {
                            checkpoint.native_resume(db, &scope)?
                        },
                    },
                };
                let plan = insert_job_recovery_attempt(
                    db,
                    conversation,
                    scope,
                    history,
                    recorded,
                    allocation,
                )?;
                ensure!(
                    plan.previous_run_id.as_ref() == Some(&checkpoint.source_run_id),
                    "The recovered attempt lost its lineage"
                );
                plan
            }
            Some(RecoveryCheckpoint::Command { .. } | RecoveryCheckpoint::Side { .. }) => {
                anyhow::bail!("The job does not have an agent checkpoint")
            }
            None => {
                if let Some(instruction) = inputs.instruction {
                    let context = job
                        .context
                        .as_ref()
                        .context("Instruction has no conversation destination")?;
                    let conversation = load_conversation(db, &context.conversation_id)?;
                    let pin = continuation_pin(db, &job.id)?;
                    let recorded = load_binding(db, &job.id)?.map(|binding| RecordedAttempt {
                        context_id: binding.context.context_id,
                        previous_run_id: None,
                        question_route: binding.question_route,
                    });
                    if let Some(pin) = &pin {
                        ensure!(
                            recorded.is_none()
                                && conversation.context_id == context.context_id
                                && pin.history_is_current(db, &conversation)?
                                && pin.native_is_available(
                                    db,
                                    &conversation,
                                    &scope,
                                    &allocation
                                )?,
                            "The continuation changed before admission",
                        );
                    }
                    insert_instruction_attempt(
                        db,
                        conversation,
                        scope,
                        instruction.revision_id,
                        recorded,
                        allocation,
                        pin.is_some_and(|pin| pin.native_session_id.is_none()),
                    )?
                } else {
                    match (
                        &job.origin,
                        job.context.as_ref(),
                        load_binding(db, &job.id)?,
                    ) {
                        (JobOrigin::Operator | JobOrigin::Pipeline, _, Some(binding)) => {
                            let conversation =
                                load_conversation(db, &binding.context.conversation_id)?;
                            insert_recorded_job_attempt(
                                db,
                                conversation,
                                scope,
                                RecordedAttempt {
                                    context_id: binding.context.context_id,
                                    previous_run_id: None,
                                    question_route: binding.question_route,
                                },
                                &inputs.request.instructions,
                                allocation,
                            )?
                        }
                        (JobOrigin::Operator, Some(context), None) => {
                            let conversation = load_conversation(db, &context.conversation_id)?;
                            crate::conversations::insert_job_with_history(
                                db,
                                conversation,
                                scope,
                                &inputs.request.instructions,
                                allocation,
                                if inputs.native_seed.is_some() {
                                    AttemptHistory::Fresh
                                } else {
                                    AttemptHistory::Automatic
                                },
                            )?
                        }
                        _ => {
                            let conversation = insert_conversation(
                                db,
                                job.checkout_id.clone(),
                                "Background job".into(),
                                true,
                            )?;
                            insert_job_attempt(
                                db,
                                conversation,
                                scope,
                                &inputs.request.instructions,
                                allocation,
                            )?
                        }
                    }
                }
            }
        }
    };
    plan.requires_bridge = inputs.request.requires_bridge;
    if inputs.native_seed.is_some() {
        ensure!(
            matches!(plan.intent, crate::providers::SessionIntent::Start),
            "Native goals require a fresh session"
        );
        plan.native_goal_seed = inputs.native_seed;
    }
    Ok(ScheduledAttempt::Agent(Box::new(plan)))
}

fn downstream_conversation(
    db: &Connection,
    job: &Job,
    wait: &DownstreamWait,
) -> Result<Option<(crate::conversations::Conversation, Option<RecordedAttempt>)>> {
    let Some(context) = wait.context_id.as_deref() else {
        return Ok(None);
    };
    let (conversation, sealed): (String, Option<String>) = db.query_row(
        "SELECT conversation_id, result_context_id FROM agent_attempts WHERE run_id = ?1",
        [&wait.parent_run_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let recorded = uses_recorded_checkpoint(db, job, &wait.parent_run_id, &conversation)?;
    let matching = if recorded {
        sealed.as_deref() == Some(context)
    } else {
        recovery_checkpoint(
            db,
            &conversation,
            &wait.parent_run_id,
            context,
            Some(context),
        )?
        .is_some()
    };
    if !matching {
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
    let question_route =
        load_binding(db, &job.id)?.map_or(QuestionRoute::Native, |binding| binding.question_route);
    Ok(Some((
        load_conversation(db, &conversation)?,
        recorded.then(|| RecordedAttempt {
            context_id: context.into(),
            previous_run_id: Some(wait.parent_run_id.clone()),
            question_route,
        }),
    )))
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
    if let Some(side) = side_requests::load(db, &job.id)? {
        return Ok(Inputs {
            request,
            scope: Some(SessionScope {
                mode: job.mode,
                ..side.scope.clone()
            }),
            command: None,
            side: Some(side),
            question: None,
            instruction: None,
            goal: None,
            native_seed: None,
        });
    }
    let scope: Option<SessionScope> = match (&job.origin, operator_scope) {
        (_, Some(bytes)) => Some(serde_json::from_slice(&protection::unprotect(&bytes)?)?),
        (JobOrigin::Operator | JobOrigin::Pipeline, None) => None,
        (JobOrigin::Agent { run_id, .. }, None) => {
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
    let native_seed = super::native_goal_qualification::native_goal_seed(db, job, scope.as_ref())?;
    Ok(Inputs {
        request,
        scope,
        command,
        side: None,
        question: load_continuation(db, job)?,
        instruction: instructions::load_revision(db, &job.id)?,
        goal: goals::managed_boundary(db, &job.id)?,
        native_seed,
    })
}

fn continuation_instruction(
    db: &Connection,
    owner: &Job,
    owner_inputs: &Inputs,
    env: &AdmissionEnvironment,
) -> Result<Option<InstructionClaim>> {
    let owner_run = owner_inputs
        .question
        .as_ref()
        .map(|question| question.run.as_str())
        .or_else(|| {
            owner
                .downstream_wait
                .as_ref()
                .map(|wait| wait.parent_run_id.as_str())
        });
    let boundary = if let Some(boundary) = &owner_inputs.goal {
        (
            boundary.run.clone(),
            boundary.conversation.clone(),
            boundary.context.clone(),
        )
    } else if let Some(run) = owner_run {
        let (conversation, context) = db.query_row(
            "SELECT conversation_id, result_context_id FROM agent_attempts WHERE run_id = ?1",
            [run],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        (run.to_owned(), conversation, context)
    } else {
        return Ok(None);
    };
    let (run, conversation, boundary) = boundary;
    let active = load_conversation(db, &conversation)?;
    let mut candidates = db.prepare(
        "SELECT j.id FROM jobs j JOIN queued_conversation_instructions i ON i.job_id = j.id
         WHERE j.project_id = ?1 AND j.conversation_id = ?2 AND j.queue_state <> 'cancelled'
           AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
           AND NOT EXISTS (SELECT 1 FROM instruction_delivery_attempts WHERE job_id = j.id AND superseded = 0)
         ORDER BY j.sequence",
    )?;
    for id in candidates.query_map(params![owner.project_id, conversation], |row| {
        row.get::<_, String>(0)
    })? {
        let candidate =
            load_job(db, &owner.project_id, &id?)?.context("Queued instruction disappeared")?;
        if pinned_continuation(db, &candidate.id)? {
            return Ok(None);
        }
        let binding = load_binding(db, &candidate.id)?;
        let destination = binding
            .as_ref()
            .map_or(&active.context_id, |binding| &binding.context.context_id);
        if instruction_owner(db, &conversation, destination)?.as_deref() != Some(run.as_str()) {
            continue;
        }
        let inputs = load_inputs(db, &candidate)?;
        if inputs.scope != owner_inputs.scope || candidate.checkout_id != owner.checkout_id {
            return Ok(None);
        }
        let (blockers, _) = evaluate(db, &candidate, &inputs, env)?;
        if !matches!(blockers.as_slice(), [AdmissionBlocker::ContinuationOwner { run_id }] if *run_id == run)
        {
            return Ok(None);
        }
        let revision = inputs
            .instruction
            .context("Instruction revision disappeared")?
            .revision_id;
        if !instructions::fits_continuation(db, &candidate, &conversation, &boundary, &revision)? {
            return Ok(None);
        }
        return Ok(Some(InstructionClaim {
            policy_key: effective_policy(db, &candidate)?.key,
            job_id: candidate.id,
            revision_id: revision,
            route: "continuation",
        }));
    }
    Ok(None)
}

fn instruction_owner(db: &Connection, conversation: &str, context: &str) -> Result<Option<String>> {
    if let Some(run) = goals::queued_boundary_owner(db, conversation, context)? {
        return Ok(Some(run));
    }
    instructions::parked_owner(db, conversation, context)
}

pub(super) fn inspect(
    db: &Connection,
    job: Job,
    env: &AdmissionEnvironment,
) -> Result<JobInspection> {
    let queued_question = load_continuation(db, &job)?.is_some();
    let awaiting_recovery =
        job.queued_recovery.is_some() || job.downstream_wait.is_some() || queued_question;
    let adopted = instruction_delivery::adopted_run(db, &job.id)?.is_some();
    let question_wait = job
        .latest_attempt
        .as_ref()
        .filter(|_| !adopted)
        .map(|attempt| question_wait_summary(db, &attempt.run_id))
        .transpose()?
        .flatten();
    let blockers = if let Some(attempt) = job.latest_attempt.as_ref().filter(|_| !awaiting_recovery)
    {
        dispatch_rejection(db, &attempt.run_id)?
            .map(|rejection| AdmissionBlocker::DispatchRejected { rejection })
            .into_iter()
            .collect()
    } else {
        evaluate(db, &job, &load_inputs(db, &job)?, env)?.0
    };
    let recovery_actions = if awaiting_recovery || question_wait.is_some() {
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
        question_wait,
        queued_question: queued_question.then_some(true),
    })
}

fn evaluate(
    db: &Connection,
    job: &Job,
    inputs: &Inputs,
    env: &AdmissionEnvironment,
) -> Result<(Vec<AdmissionBlocker>, SubagentAllocation)> {
    use AdmissionBlocker::*;
    let mut blockers = Vec::new();
    let continuation = continuation_pin(db, &job.id)?;
    let settings = queue_settings(db)?;
    if settings.paused {
        blockers.push(QueuePaused);
    }
    match job.queue_state {
        QueueState::Queued => {}
        QueueState::Paused => blockers.push(JobPaused),
        QueueState::Cancelled => return Ok((vec![JobCancelled], SubagentAllocation::default())),
    }
    if let Some(reason) = goals::limit_blocker(db, &job.id, inputs.native_seed.is_some())? {
        blockers.push(Policy { reason });
    }
    if job.kind == JobKind::Command
        && let Some(reason) = crate::workflows::mcp_tools::mcp_admission_blocker(db, job)?
    {
        blockers.push(Policy { reason });
    }
    for id in &inputs.request.dependencies {
        let dependency =
            load_job(db, &job.project_id, id)?.context("job dependency disappeared")?;
        let mut status = None;
        if dependency.downstream_wait.is_none()
            && let Some(attempt) = dependency.latest_attempt
            && !has_open_questions(db, &attempt.run_id)?
        {
            status = Some(attempt.status);
        }
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
    if rule
        .requirements
        .capabilities
        .contains(&Capability::OsCommandSandbox)
    {
        let direct = env.runner == DirectWindowsRunner::IDENTITY;
        let platform = direct
            .then(|| DirectWindowsRunner.execution_platform())
            .flatten();
        let reason = match inputs.scope.as_ref() {
            Some(scope) => sandbox::admission_blocker(scope, platform.as_ref(), direct),
            None if inputs.command.is_some() => {
                Some("This command cannot satisfy the rule's CLI sandbox requirement")
            }
            None => None,
        };
        if let Some(reason) = reason {
            blockers.push(Policy { reason });
        }
    }
    if let Some(question) = &inputs.question {
        if !question_answers::current(db, question, inputs.scope.as_ref())? {
            blockers.push(Policy {
                reason: "The saved question's context, profile or authorization changed",
            });
        }
        if let Some(run_id) = active_conversation_run(db, &question.conversation)? {
            blockers.push(ConversationBusy { run_id });
        } else if let Some(run_id) = instruction_delivery::unconfirmed_in_context(
            db,
            &question.conversation,
            &question.context,
        )? {
            blockers.push(InstructionDeliveryUncertain { run_id });
        }
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
        } else if inputs.question.is_none() && has_open_questions(db, &wait.parent_run_id)? {
            blockers.push(ConversationBusy {
                run_id: wait.parent_run_id.clone(),
            });
        } else if let Some((conversation, _)) = downstream_conversation(db, job, wait)? {
            if inputs.question.is_none()
                && let Some(context) = &wait.context_id
                && let Some(run_id) =
                    instruction_delivery::unconfirmed_in_context(db, &conversation.id, context)?
            {
                blockers.push(InstructionDeliveryUncertain { run_id });
            }
        } else {
            blockers.push(ContextChanged {
                context_id: wait
                    .context_id
                    .clone()
                    .context("A waiting job has no recorded context")?,
            });
        }
    } else if let (None, Some(action)) = (&inputs.question, job.queued_recovery) {
        let (source_run, context_id) =
            pending_recovery(db, job)?.context("Missing queued recovery")?;
        let writer_checkpoint = pending_writer_checkpoint(db, job)?;
        let matching = recovery_source(db, job)?.is_some_and(|source| {
            source.run_id() == source_run
                && source.context_id() == context_id.as_deref()
                && (source.actions(job.mode).contains(&action)
                    || (action == RecoveryAction::Resume
                        && writer_checkpoint.as_deref() == Some(source_run.as_str())))
        });
        if !matching {
            blockers.push(match context_id {
                Some(context_id) => ContextChanged { context_id },
                None => Policy {
                    reason: "The command recovery checkpoint changed",
                },
            });
        }
        if let Some(attempt) = &job.latest_attempt {
            if let Some(conversation) = &attempt.conversation_id {
                if let Some(run_id) = active_conversation_run(db, conversation)? {
                    blockers.push(ConversationBusy { run_id });
                }
            }
        }
    } else if let (None, JobOrigin::Operator | JobOrigin::Pipeline, Some(context)) =
        (&inputs.question, &job.origin, &job.context)
    {
        let binding = load_binding(db, &job.id)?;
        let conversation = load_conversation(db, &context.conversation_id)?;
        let destination_current = if inputs.instruction.is_some() {
            if let Some(job_id) = instructions::predecessor(db, job)? {
                blockers.push(InstructionOrder { job_id });
            }
            if let Some(pin) = &continuation {
                binding.is_none()
                    && conversation.context_id == context.context_id
                    && pin.history_is_current(db, &conversation)?
            } else {
                instructions::preserves_destination(
                    db,
                    &conversation.id,
                    &context.context_id,
                    &conversation.context_id,
                )?
            }
        } else {
            conversation.context_id == context.context_id
        };
        if (continuation.is_some() || binding.is_none()) && !destination_current {
            blockers.push(ContextChanged {
                context_id: context.context_id.clone(),
            });
        }
        let destination = binding
            .as_ref()
            .map_or(&conversation.context_id, |binding| {
                &binding.context.context_id
            });
        if let Some(run_id) = active_conversation_run(db, &context.conversation_id)? {
            blockers.push(ConversationBusy { run_id });
        } else if let Some(run_id) =
            instruction_delivery::unconfirmed_in_context(db, &context.conversation_id, destination)?
        {
            blockers.push(InstructionDeliveryUncertain { run_id });
        } else if inputs.instruction.is_some()
            && let Some(run_id) = instruction_owner(db, &context.conversation_id, destination)?
        {
            blockers.push(ContinuationOwner { run_id });
        } else if let Some(run_id) = saved_question_run(db, &context.conversation_id, destination)?
        {
            blockers.push(ConversationBusy { run_id });
        }
    }
    if inputs.request.requires_bridge && !env.bridge_available {
        blockers.push(MissingCapability {
            capability: Capability::RunScopedMcp,
        });
    }
    if let Some(reason) = effective_release_blocker(db, job)? {
        blockers.push(PipelineRelease { reason });
    }
    if job.mode == SessionMode::ApprovedWrite {
        if let Some(prior_scope) = super::goal_requalification::initial_checkpoint(db, job)?
            && prior_scope.same_worktree
            && !goals::confirmed_stage_worktree(db, job, &prior_scope.run)?
        {
            blockers.push(Policy {
                reason: "The requalified goal's worktree checkpoint is unavailable",
            });
        }
        if let Some(goal) = &inputs.goal
            && crate::workflows::load_workflow_job_input(db, &job.id)?.is_some()
            && !goals::confirmed_stage_worktree(db, job, &goal.run)?
        {
            blockers.push(Policy {
                reason: "The workflow goal's prior worktree checkpoint is unavailable",
            });
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
    if let Some(side) = &inputs.side {
        if !rule.requirements.customizations.is_empty()
            || rule
                .requirements
                .capabilities
                .iter()
                .any(|capability| *capability != Capability::SideRequest)
        {
            blockers.push(Policy { reason: "The tool-disabled side route cannot fulfill the project rule's capability and customization requirements" });
        }
        if !side.still_qualified(db)? {
            blockers.push(Policy {
                reason: "The qualified tool-disabled side profile changed",
            });
        }
        if !side_requests::suggestion_still_current(db, &job.id, side)? {
            blockers.push(Policy {
                reason: "The suggestion's source or composer is no longer current",
            });
        }
        return Ok((blockers, SubagentAllocation::default()));
    }
    let allocation = match job_subagent_allocation(db, job, inputs.scope.as_ref())? {
        Ok(allocation) => allocation,
        Err(blocker) => {
            blockers.push(blocker);
            SubagentAllocation::default()
        }
    };
    if let (Some(pin), Some(context), Some(scope)) = (&continuation, &job.context, &inputs.scope)
        && blockers.is_empty()
        && job.latest_attempt.is_none()
        && !pin.native_is_available(
            db,
            &load_conversation(db, &context.conversation_id)?,
            scope,
            &allocation,
        )?
    {
        blockers.push(Policy { reason: "The verified provider session changed. Review the continuation before sending again." });
    }
    if job.latest_attempt.is_none()
        && matches!(blockers.as_slice(), [] | [ContinuationOwner { .. }])
        && let Some(revision) = &inputs.instruction
        && let Some(reason) = instructions::attachment_blocker(db, job, revision)?
    {
        blockers.push(Policy { reason });
    }
    Ok((blockers, allocation))
}

fn active_conversation_run(db: &Connection, conversation: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT run_id FROM active_agent_attempts WHERE conversation_id = ?1
             UNION ALL SELECT run_id FROM unconfirmed_agent_stops WHERE conversation_id = ?1 LIMIT 1",
            [conversation],
            |row| row.get(0),
        )
        .optional()?)
}

fn saved_question_run(
    db: &Connection,
    conversation: &str,
    context: &str,
) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT run_id FROM open_agent_questions WHERE conversation_id = ?1 AND context_id = ?2
            AND state IN ('saved', 'needs_review') ORDER BY request_event_seq LIMIT 1",
            params![conversation, context],
            |row| row.get(0),
        )
        .optional()?)
}
