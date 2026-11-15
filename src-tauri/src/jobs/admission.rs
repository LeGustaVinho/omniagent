use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    conversations::{
        AttemptPlan, SessionScope, consume_matching_draft, insert_conversation, insert_job_attempt,
        load_conversation,
    },
    protection,
    providers::{Capability, SessionMode},
    runs::RunStatus,
    store::Store,
};

use super::{Job, JobContext, JobOrigin, JobRequest, QueueState, insert_job, load_job};

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
    MissingCapability {
        capability: Capability,
    },
    Writer {
        checkout_id: String,
        run_id: String,
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
                 AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id))",
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
                let job = insert_job(&tx, project, Some(context), JobOrigin::Operator, request, 0)?;
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
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let settings = queue_settings(&tx)?;
            if settings.paused || occupancy(&tx)? >= settings.capacity {
                return Ok(None);
            }
            let candidates = {
                let mut query = tx.prepare(
                    "SELECT project_id, id FROM jobs j WHERE queue_state = 'queued'
                    AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
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
                let inputs = load_inputs(&tx, &job)?;
                if !evaluate(&tx, &job, &inputs, &env)?.is_empty() {
                    continue;
                }
                let scope = inputs
                    .scope
                    .context("admitted job has no effective CLI profile")?;
                let conversation = match (&job.origin, job.context) {
                    (JobOrigin::Operator, Some(context)) => {
                        load_conversation(&tx, &context.conversation_id)?
                    }
                    _ => insert_conversation(
                        &tx,
                        job.checkout_id.clone(),
                        "Background job".into(),
                        true,
                    )?,
                };
                let mut plan =
                    insert_job_attempt(&tx, conversation, scope, &inputs.request.instructions)?;
                plan.requires_bridge = inputs.request.requires_bridge;
                tx.execute(
                    "INSERT INTO job_attempts(job_id, ordinal, run_id) VALUES (?1, 1, ?2)",
                    params![job.id, plan.run.id],
                )?;
                if job.mode == SessionMode::ApprovedWrite {
                    tx.execute(
                        "INSERT INTO job_writer_reservations VALUES (?1, ?2)",
                        params![job.checkout_id, plan.run.id],
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
}

fn queue_settings(db: &Connection) -> Result<QueueSettings> {
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
        "SELECT count(*) FROM job_attempts j JOIN active_agent_attempts a ON a.run_id = j.run_id",
        [],
        |row| row.get(0),
    )?)
}

struct Inputs {
    request: JobRequest,
    scope: Option<SessionScope>,
}

fn load_inputs(db: &Connection, job: &Job) -> Result<Inputs> {
    let (request, operator_scope): (Vec<u8>, Option<Vec<u8>>) = db.query_row(
        "SELECT request, operator_scope FROM jobs WHERE id = ?1",
        [&job.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let request: JobRequest = serde_json::from_slice(&protection::unprotect(&request)?)?;
    let scope: Option<SessionScope> = match &job.origin {
        JobOrigin::Operator => operator_scope
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
    Ok(Inputs { request, scope })
}

fn inspect(db: &Connection, job: Job, env: &AdmissionEnvironment) -> Result<JobInspection> {
    let blockers = if job.latest_attempt.is_some() {
        Vec::new()
    } else {
        evaluate(db, &job, &load_inputs(db, &job)?, env)?
    };
    Ok(JobInspection { job, blockers })
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
        let status = dependency.latest_attempt.map(|attempt| attempt.status);
        if status != Some(RunStatus::Succeeded) {
            blockers.push(Dependency {
                job_id: id.clone(),
                status,
                queue_state: dependency.queue_state,
            });
        }
    }
    let approval: Option<(String, String)> = db
        .query_row(
            "SELECT id, decision FROM job_approvals WHERE job_id = ?1",
            [&job.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((approval_id, decision)) = approval {
        match decision.as_str() {
            "pending" => blockers.push(Approval { approval_id }),
            "denied" => blockers.push(Policy {
                reason: "An operator declined this job",
            }),
            _ => {}
        }
    }
    match &inputs.scope {
        None => blockers.push(Policy {
            reason: "The job needs an eligible CLI permission profile",
        }),
        Some(scope) if scope.runner != env.runner || !scope.executable.is_file() => {
            blockers.push(Policy {
                reason: "The job's selected CLI or runner is unavailable",
            })
        }
        _ => {}
    }
    if let (JobOrigin::Operator, Some(context)) = (&job.origin, &job.context) {
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
        let holder = db
            .query_row(
                "SELECT run_id FROM job_writer_reservations WHERE checkout_id = ?1",
                [&job.checkout_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(run_id) = holder {
            blockers.push(Writer {
                checkout_id: job.checkout_id.clone(),
                run_id,
            });
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
