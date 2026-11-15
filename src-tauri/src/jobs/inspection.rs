use std::collections::BTreeSet;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    providers::{Capability, capabilities::revision},
    runs::RunStatus,
};

use super::{
    AdmissionBlocker, AdmissionEnvironment, JobInspection, JobKind, JobPermission, JobRole,
    QueueSettings, QueueState,
    admission::{inspect, queue_settings},
    load_job,
    pipeline::load_attempt_job,
    policy::{effective_policy, rules},
};

const CHILD_LIMIT: usize = 6;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InspectionPolicy {
    role: JobRole,
    permission: JobPermission,
    admitted_revision: Option<String>,
    effective_revision: String,
    required_capabilities: Vec<Capability>,
    required_customizations: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InspectionQueue {
    pub settings: QueueSettings,
    pub project_policy_revision: String,
    pub current: Option<InspectionJob>,
    pub submitted: Vec<InspectionJob>,
    pub more_submitted: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InspectionJob {
    id: String,
    kind: JobKind,
    pub task_id: Option<String>,
    state: QueueState,
    run_status: Option<RunStatus>,
    blockers: BTreeSet<QueueBlockerKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum QueueBlockerKind {
    Paused,
    Cancelled,
    Dependency,
    Approval,
    Policy,
    Context,
    Capability,
    Dispatch,
    ImplementationRelease,
    Writer,
    Capacity,
}

impl From<AdmissionBlocker> for QueueBlockerKind {
    fn from(value: AdmissionBlocker) -> Self {
        match value {
            AdmissionBlocker::QueuePaused | AdmissionBlocker::JobPaused => Self::Paused,
            AdmissionBlocker::JobCancelled => Self::Cancelled,
            AdmissionBlocker::Dependency { .. } => Self::Dependency,
            AdmissionBlocker::Approval { .. } => Self::Approval,
            AdmissionBlocker::Policy { .. } => Self::Policy,
            AdmissionBlocker::ContextChanged { .. } | AdmissionBlocker::ConversationBusy { .. } => {
                Self::Context
            }
            AdmissionBlocker::MissingCapability { .. } => Self::Capability,
            AdmissionBlocker::DispatchRejected { .. } => Self::Dispatch,
            AdmissionBlocker::PipelineRelease { .. } => Self::ImplementationRelease,
            AdmissionBlocker::Writer { .. }
            | AdmissionBlocker::AppWriter { .. }
            | AdmissionBlocker::PendingWorktreeChanges { .. } => Self::Writer,
            AdmissionBlocker::Capacity { .. } => Self::Capacity,
        }
    }
}

impl From<JobInspection> for InspectionJob {
    fn from(value: JobInspection) -> Self {
        let job = value.job;
        Self {
            id: job.id,
            kind: job.kind,
            task_id: job.task_id,
            state: job.queue_state,
            run_status: job.latest_attempt.map(|attempt| attempt.status),
            blockers: value
                .blockers
                .into_iter()
                .map(QueueBlockerKind::from)
                .collect(),
        }
    }
}

pub(crate) fn load_inspection_queue(
    db: &Connection,
    project: &str,
    run: &str,
    env: &AdmissionEnvironment,
) -> Result<(InspectionQueue, Option<InspectionPolicy>)> {
    let job = load_attempt_job(db, run)?.filter(|job| job.project_id == project);
    let policy = job
        .as_ref()
        .map(|job| -> Result<_> {
            let effective = effective_policy(db, job)?;
            let admitted_revision = db
                .query_row(
                    "SELECT policy_key FROM job_attempt_policies WHERE run_id = ?1",
                    [run],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(InspectionPolicy {
                role: effective.role,
                permission: effective.permission,
                admitted_revision,
                effective_revision: effective.key,
                required_capabilities: effective
                    .requirements
                    .capabilities
                    .into_iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                required_customizations: effective.requirements.customizations.len(),
            })
        })
        .transpose()?;
    let current = job
        .map(|job| inspect(db, job, env).map(InspectionJob::from))
        .transpose()?;
    let mut children = db.prepare(
        "SELECT id FROM jobs WHERE project_id = ?1 AND origin_run_id = ?2 ORDER BY sequence DESC LIMIT ?3",
    )?.query_map(params![project, run, (CHILD_LIMIT + 1) as u32], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more_submitted = children.len() > CHILD_LIMIT;
    children.truncate(CHILD_LIMIT);
    let submitted = children
        .into_iter()
        .map(|id| {
            let job =
                load_job(db, project, &id)?.context("Visible job disappeared from the snapshot")?;
            Ok(inspect(db, job, env)?.into())
        })
        .collect::<Result<_>>()?;
    let queue = InspectionQueue {
        settings: queue_settings(db)?,
        project_policy_revision: revision(&rules(db, project)?)?,
        current,
        submitted,
        more_submitted,
    };
    Ok((queue, policy))
}
