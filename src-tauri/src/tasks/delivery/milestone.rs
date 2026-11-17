use std::{os::windows::process::CommandExt, process::Command};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    protection,
    repository::{
        git_path,
        verified_git::load_verified_delivery_commit,
        verified_merge::{VerifiedMerge, load_verified_delivery_merge},
    },
    store::Store,
    workflows::load_delivery_units,
};

use super::{TaskDeliveryEvidence, insert_delivery};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCompletionMilestone {
    #[default]
    VerifiedCommit,
    VerifiedMerge,
}

impl DeliveryCompletionMilestone {
    fn key(self) -> &'static str {
        match self {
            Self::VerifiedCommit => "verified_commit",
            Self::VerifiedMerge => "verified_merge",
        }
    }

    fn stored(value: &str) -> Result<Self> {
        match value {
            "verified_commit" => Ok(Self::VerifiedCommit),
            "verified_merge" => Ok(Self::VerifiedMerge),
            _ => anyhow::bail!("unknown task delivery milestone"),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryMilestonePolicy {
    pub milestone: DeliveryCompletionMilestone,
    pub destination_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTaskDeliveryMerge {
    pub workflow_run_id: String,
    pub plan_revision: String,
    pub unit_id: String,
    pub commit_evidence_id: String,
    pub commit_evidence_ids: Vec<String>,
    pub source_branch: String,
    pub source_sha: String,
    pub destination_branch: Option<String>,
}

impl PendingTaskDeliveryMerge {
    fn completion_evidence(self) -> TaskDeliveryEvidence {
        TaskDeliveryEvidence::VerifiedCommit {
            workflow_run_id: self.workflow_run_id,
            plan_revision: self.plan_revision,
            unit_evidence_ids: self.commit_evidence_ids,
            completion_sha: self.source_sha,
        }
    }
}

impl Store {
    pub async fn delivery_milestone_policy(
        &self,
        project: String,
    ) -> Result<DeliveryMilestonePolicy> {
        self.execute(move |db| load_delivery_milestone_policy(db, &project))
            .await
    }

    pub async fn set_delivery_milestone_policy(
        &self,
        project: String,
        mut policy: DeliveryMilestonePolicy,
    ) -> Result<DeliveryMilestonePolicy> {
        ensure!(
            policy.milestone != DeliveryCompletionMilestone::VerifiedMerge
                || policy.destination_branch.is_some(),
            "verified merge requires a configured destination branch"
        );
        if let Some(branch) = policy.destination_branch.take() {
            policy.destination_branch =
                Some(tokio::task::spawn_blocking(move || verify_branch_name(&branch)).await??);
        }
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_delivery_milestone_policies (project_id, milestone, destination_branch)
                 VALUES (?1, ?2, ?3) ON CONFLICT (project_id) DO UPDATE SET
                     milestone = excluded.milestone, destination_branch = excluded.destination_branch",
                params![project, policy.milestone.key(), policy.destination_branch],
            )?;
            Ok(policy)
        })
        .await
    }
}

fn verify_branch_name(branch: &str) -> Result<String> {
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let verified = Command::new(git_path()?)
        .args(["check-ref-format", "--branch", branch])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("Git is required to validate the destination branch")?;
    ensure!(
        verified.status.success()
            && String::from_utf8(verified.stdout)?.trim_end_matches(['\r', '\n']) == branch,
        "enter a valid destination branch name"
    );
    Ok(branch.to_owned())
}

pub(crate) fn load_delivery_milestone_policy(
    db: &Connection,
    project: &str,
) -> Result<DeliveryMilestonePolicy> {
    let policy: Option<(String, Option<String>)> = db.query_row(
        "SELECT milestone, destination_branch FROM project_delivery_milestone_policies WHERE project_id = ?1",
        [project],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((milestone, destination_branch)) = policy else {
        return Ok(DeliveryMilestonePolicy::default());
    };
    Ok(DeliveryMilestonePolicy {
        milestone: DeliveryCompletionMilestone::stored(&milestone)?,
        destination_branch,
    })
}

pub(crate) fn load_workflow_milestone_policy(
    db: &Connection,
    project: &str,
    workflow_run: &str,
) -> Result<DeliveryMilestonePolicy> {
    let destination_branch = db
        .query_row(
            "SELECT task.destination_branch FROM workflow_backlog_task_runs task
         JOIN workflow_run_pins pin ON pin.pipeline_run_id = task.child_run_id
         WHERE pin.project_id = ?1 AND task.child_run_id = ?2",
            params![project, workflow_run],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(destination_branch) = match destination_branch {
        Some(branch) => Some(branch),
        None => {
            crate::workflows::github::workflow_github_merge_destination(db, project, workflow_run)?
        }
    } {
        return Ok(DeliveryMilestonePolicy {
            milestone: DeliveryCompletionMilestone::VerifiedMerge,
            destination_branch: Some(destination_branch),
        });
    }
    load_delivery_milestone_policy(db, project)
}

pub(crate) fn record_verified_task_delivery(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
    plan_revision: &str,
) -> Result<()> {
    if task_delivery_recorded(db, project, task, workflow_run, plan_revision)?
        || milestone_pin(db, project, task, workflow_run, plan_revision)?.is_some()
    {
        return Ok(());
    }
    ensure!(
        active_plan(db, project, task, workflow_run)?.as_deref() == Some(plan_revision),
        "this workflow plan is no longer active for this task"
    );
    let policy = load_workflow_milestone_policy(db, project, workflow_run)?;
    let candidate = complete_commit_chain(
        db,
        project,
        task,
        workflow_run,
        plan_revision,
        policy.destination_branch,
    )?;
    db.execute(
        "INSERT INTO task_delivery_milestone_pins
         (project_id, task_id, workflow_run_id, plan_revision, milestone, candidate)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            project,
            task,
            workflow_run,
            plan_revision,
            policy.milestone.key(),
            protection::protect(&serde_json::to_vec(&candidate)?)?
        ],
    )?;
    if policy.milestone == DeliveryCompletionMilestone::VerifiedCommit {
        insert_delivery(db, project, task, candidate.completion_evidence())?;
    }
    Ok(())
}

pub(crate) fn delivery_merge_candidate(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
) -> Result<Option<PendingTaskDeliveryMerge>> {
    Ok(current_merge_candidate(db, project, task, workflow_run)?.map(|(_, candidate)| candidate))
}

pub(super) fn pending_merge(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
) -> Result<Option<(DeliveryCompletionMilestone, PendingTaskDeliveryMerge)>> {
    let Some((milestone, candidate)) = current_merge_candidate(db, project, task, workflow_run)?
    else {
        return Ok(None);
    };
    if load_verified_delivery_merge(db, project, task, &candidate.commit_evidence_id)?.is_some() {
        return Ok(None);
    }
    Ok(Some((milestone, candidate)))
}

pub(crate) fn record_verified_task_merge(db: &Connection, evidence: &VerifiedMerge) -> Result<()> {
    let project = &evidence.project_id;
    let task = &evidence.task_id;
    let workflow_run = &evidence.workflow_run_id;
    ensure!(
        load_verified_delivery_merge(db, project, task, &evidence.commit_evidence_id)?.as_ref()
            == Some(evidence),
        "merge evidence must be saved before it can complete a task"
    );
    let (milestone, candidate) = current_merge_candidate(db, project, task, workflow_run)?
        .context("merge evidence requires a complete commit chain from the active delivery")?;
    let configured_destination = match &candidate.destination_branch {
        Some(branch) => Some(branch.clone()),
        None => load_delivery_milestone_policy(db, project)?.destination_branch,
    };
    ensure!(
        candidate.unit_id == evidence.unit_id
            && candidate.commit_evidence_id == evidence.commit_evidence_id
            && candidate.commit_evidence_ids == evidence.commit_evidence_ids
            && candidate.source_sha == evidence.source_sha
            && candidate.source_branch == evidence.source_branch
            && configured_destination
                .as_deref()
                .is_none_or(|branch| branch == evidence.destination_branch),
        "verified merge does not match the active delivery and its destination"
    );
    if milestone == DeliveryCompletionMilestone::VerifiedMerge
        && !task_delivery_recorded(db, project, task, workflow_run, &candidate.plan_revision)?
    {
        insert_delivery(db, project, task, candidate.completion_evidence())?;
    }
    Ok(())
}

fn current_merge_candidate(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
) -> Result<Option<(DeliveryCompletionMilestone, PendingTaskDeliveryMerge)>> {
    let Some(plan) = active_plan(db, project, task, workflow_run)? else {
        return Ok(None);
    };
    if let Some(pin) = milestone_pin(db, project, task, workflow_run, &plan)? {
        return Ok(Some(pin));
    }
    // Older verified-commit deliveries have a final receipt but no milestone pin.
    if task_delivery_recorded(db, project, task, workflow_run, &plan)? {
        let candidate = complete_commit_chain(db, project, task, workflow_run, &plan, None)?;
        return Ok(Some((
            DeliveryCompletionMilestone::VerifiedCommit,
            candidate,
        )));
    }
    Ok(None)
}

fn active_plan(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT head.plan_revision FROM workflow_delivery_heads head
         JOIN job_pipeline_runs run ON run.id = head.pipeline_run_id AND run.plan_revision = head.plan_revision
         WHERE head.project_id = ?1 AND head.task_id = ?2 AND head.pipeline_run_id = ?3",
        params![project, task, workflow_run],
        |row| row.get(0),
    ).optional()?)
}

fn task_delivery_recorded(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
    plan_revision: &str,
) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_delivery_events WHERE project_id = ?1 AND task_id = ?2
          AND workflow_run_id = ?3 AND workflow_plan_revision = ?4)",
        params![project, task, workflow_run, plan_revision],
        |row| row.get(0),
    )?)
}

fn milestone_pin(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
    plan_revision: &str,
) -> Result<Option<(DeliveryCompletionMilestone, PendingTaskDeliveryMerge)>> {
    let row: Option<(String, Vec<u8>)> = db.query_row(
        "SELECT milestone, candidate FROM task_delivery_milestone_pins WHERE project_id = ?1 AND task_id = ?2
          AND workflow_run_id = ?3 AND plan_revision = ?4",
        params![project, task, workflow_run, plan_revision],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    row.map(|(milestone, candidate)| {
        Ok((
            DeliveryCompletionMilestone::stored(&milestone)?,
            serde_json::from_slice(&protection::unprotect(&candidate)?)?,
        ))
    })
    .transpose()
}

fn complete_commit_chain(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
    plan_revision: &str,
    destination_branch: Option<String>,
) -> Result<PendingTaskDeliveryMerge> {
    let units = load_delivery_units(db, project, task, workflow_run)?;
    let mut previous: Option<crate::repository::verified_git::VerifiedCommit> = None;
    let mut commit_evidence_ids = Vec::with_capacity(units.len());
    for unit in units {
        let id = unit
            .commit_evidence_id
            .context("a planned unit is still waiting for a verified commit")?;
        let evidence = load_verified_delivery_commit(db, project, task, &id)?
            .context("unit commit evidence is missing")?;
        ensure!(
            unit.plan_revision == plan_revision
                && evidence.unit_id == unit.unit.id
                && evidence.purpose == unit.unit.purpose
                && evidence.checkout_id == unit.checkout_id
                && previous
                    .as_ref()
                    .is_none_or(|commit| commit.sha == evidence.base_commit
                        && commit.repository == evidence.repository),
            "unit commit evidence does not match the planned delivery chain"
        );
        commit_evidence_ids.push(id);
        previous = Some(evidence);
    }
    let last = previous.context("delivery requires at least one planned unit")?;
    Ok(PendingTaskDeliveryMerge {
        workflow_run_id: workflow_run.into(),
        plan_revision: plan_revision.into(),
        unit_id: last.unit_id,
        commit_evidence_id: last.id,
        commit_evidence_ids,
        source_branch: last.branch,
        source_sha: last.sha,
        destination_branch,
    })
}
