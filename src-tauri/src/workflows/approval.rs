use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    jobs::{PipelineReleaseScope, issue_pipeline_release},
    repository::ManagedWorktree,
    store::Store,
};

use super::{
    ApprovalReference, ArtifactKind, ArtifactValue, TaskReference, WorkflowArtifact,
    WorkflowPlanUnit,
    artifacts::{insert_artifact, load_artifact},
    units::record_delivery_units,
};

mod rules;
#[cfg(test)]
mod tests;
pub(super) use rules::activate_selected_approval;
pub use rules::{
    ApprovalFacts, ApprovalRuleEvidence, ApprovalRuleOutcome, ApprovalRuleSettings, UnmetApproval,
    WorkflowApprovalPreview, WorkflowApprovalStage, WorkflowApprovalSubmission,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionSource {
    Operator,
    AutomaticRule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecision {
    pub workflow_run_id: String,
    pub node_instance_id: String,
    pub plan_revision: String,
    pub task: TaskReference,
    pub task_artifact_id: String,
    pub source_checkout_id: String,
    pub source: ApprovalDecisionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_evidence: Option<ApprovalRuleEvidence>,
}

impl Store {
    pub async fn workflow_release_artifact(
        &self,
        project: String,
        run: String,
        decision_id: String,
    ) -> Result<Option<WorkflowArtifact>> {
        let artifact_id: Option<String> = self.execute(move |db| {
            Ok(db.query_row("SELECT release_artifact_id FROM workflow_release_bindings WHERE decision_artifact_id = ?1",
                [&decision_id], |row| row.get(0)).optional()?)
        }).await?;
        let Some(artifact_id) = artifact_id else {
            return Ok(None);
        };
        self.resolve_workflow_artifact(project, run, artifact_id, ArtifactKind::Release)
            .await
    }

    pub async fn workflow_approval_decision(
        &self,
        project: String,
        run: String,
        node: String,
        plan: String,
    ) -> Result<Option<ApprovalDecision>> {
        self.execute(move |db| {
            let id: Option<String> = db
                .query_row(
                    "SELECT decision_artifact_id FROM workflow_approval_decisions
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_revision = ?3
                 ORDER BY generation DESC LIMIT 1",
                    params![run, node, plan],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = id else {
                return Ok(None);
            };
            let artifact = load_artifact(db, &project, &run, &id, ArtifactKind::Approval)?
                .context("saved approval decision is unavailable")?;
            let ArtifactValue::Approval(decision) = artifact.value else {
                anyhow::bail!("saved decision has the wrong type");
            };
            Ok(Some(decision))
        })
        .await
    }

    pub(super) async fn workflow_plan_checkout(
        &self,
        project: &str,
        attempt: &str,
    ) -> Result<String> {
        let (project, attempt) = (project.to_owned(), attempt.to_owned());
        self.execute(move |db| load_plan_checkout(db, &project, &attempt))
            .await
    }
}

fn load_plan_checkout(db: &Connection, project: &str, attempt: &str) -> Result<String> {
    db.query_row(
        "SELECT j.checkout_id FROM jobs j JOIN job_attempts a ON a.job_id = j.id
         WHERE j.project_id = ?1 AND a.run_id = ?2",
        params![project, attempt],
        |row| row.get(0),
    )
    .optional()?
    .context("the plan's captured checkout is unavailable")
}

pub(crate) fn active_delivery_workflow(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<Option<String>> {
    Ok(db.query_row(
            "SELECT h.pipeline_run_id FROM workflow_delivery_heads h
             JOIN job_pipeline_runs p ON p.id = h.pipeline_run_id AND p.plan_revision = h.plan_revision
             WHERE h.project_id = ?1 AND h.task_id = ?2",
        params![project, task], |row| row.get(0),
    ).optional()?)
}

pub(crate) fn workflow_release_authorized(
    db: &Connection,
    project: &str,
    run: &str,
    release_id: &str,
) -> Result<Option<bool>> {
    let decision: Option<String> = db
        .query_row(
            "SELECT b.decision_artifact_id FROM workflow_release_bindings b
         JOIN job_pipeline_releases r ON r.id = b.release_id
         WHERE b.release_id = ?1 AND r.project_id = ?2 AND r.pipeline_run_id = ?3",
            params![release_id, project, run],
            |row| row.get(0),
        )
        .optional()?;
    let Some(decision) = decision else {
        let typed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_run_pins WHERE project_id = ?1 AND pipeline_run_id = ?2)",
            params![project, run], |row| row.get(0),
        )?;
        return Ok(typed.then_some(false));
    };
    let Some(workflow) = super::runtime::load_workflow_run(db, project, run)? else {
        return Ok(Some(false));
    };
    if super::authoring::package::ensure_package_repeat_guards(db, project, &workflow.version)
        .is_err()
    {
        return Ok(Some(false));
    }
    let saved = load_artifact(db, project, run, &decision, ArtifactKind::Approval)?;
    let Some(ArtifactValue::Approval(approval)) = saved.map(|artifact| artifact.value) else {
        return Ok(Some(false));
    };
    Ok(Some(decision_is_active(db, &approval, &decision)?))
}

pub(crate) fn detach_active_delivery_workflow(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<()> {
    db.execute(
        "UPDATE job_pipeline_releases SET revoked = 1 WHERE project_id = ?1 AND task_id = ?2
         AND pipeline_run_id = (SELECT pipeline_run_id FROM workflow_delivery_heads WHERE project_id = ?1 AND task_id = ?2)",
        params![project, task],
    )?;
    db.execute(
        "DELETE FROM workflow_delivery_heads WHERE project_id = ?1 AND task_id = ?2",
        params![project, task],
    )?;
    Ok(())
}

pub(super) fn record_decision(
    db: &Connection,
    decision: ApprovalDecision,
) -> Result<WorkflowArtifact> {
    let project = decision.task.project_id.clone();
    let task = decision.task.task_id.clone();
    let run = decision.workflow_run_id.clone();
    let node = decision.node_instance_id.clone();
    let plan = decision.plan_revision.clone();
    require_current_plan(db, &project, &run, &plan)?;
    validate_linked_task(db, &decision)?;
    crate::tasks::validate_repository_delivery_open(db, &project, &task)?;
    ensure!(
        rules::rule_authorizes(db, &decision)?,
        "the automatic approval rule no longer applies"
    );
    let rule_revision = if decision.source == ApprovalDecisionSource::AutomaticRule {
        decision
            .rule_evidence
            .as_ref()
            .map(|evidence| evidence.revision.clone())
    } else {
        None
    };
    let artifact = insert_artifact(
        db,
        &project,
        run.clone(),
        node.clone(),
        "decision".into(),
        ArtifactValue::Approval(decision),
    )?;
    db.execute(
        "INSERT INTO workflow_approval_decisions
            (pipeline_run_id, node_instance_id, plan_revision, decision_artifact_id, generation, rule_revision)
         SELECT ?1, ?2, ?3, ?4, coalesce(max(generation), 0) + 1, ?5
         FROM workflow_approval_decisions WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_revision = ?3",
        params![run, node, plan, artifact.id, rule_revision],
    )?;
    detach_active_delivery_workflow(db, &project, &task)?;
    db.execute(
        "INSERT INTO workflow_delivery_heads VALUES (?1, ?2, ?3, ?4, ?5)",
        params![project, task, run, plan, artifact.id],
    )?;
    Ok(artifact)
}

pub(super) fn validate_linked_task(db: &Connection, decision: &ApprovalDecision) -> Result<()> {
    let linked: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_host_completions WHERE pipeline_run_id = ?1 AND execution_key = ?2 AND artifact_id = ?3)",
        params![decision.workflow_run_id, decision.plan_revision, decision.task_artifact_id], |row| row.get(0),
    )?;
    ensure!(
        linked,
        "only the task linked after review of this plan can be approved"
    );
    Ok(())
}

pub(super) fn require_current_plan(
    db: &Connection,
    project: &str,
    run: &str,
    plan: &str,
) -> Result<()> {
    let current: Option<String> = db.query_row(
        "SELECT plan_revision FROM job_pipeline_runs WHERE id = ?1 AND project_id = ?2",
        params![run, project],
        |row| row.get(0),
    )?;
    ensure!(
        current.as_deref() == Some(plan),
        "the node requires the currently selected workflow plan"
    );
    Ok(())
}

pub(super) fn require_active_decision(
    db: &Connection,
    decision: &ApprovalDecision,
    decision_id: &str,
) -> Result<()> {
    ensure!(
        decision_is_active(db, decision, decision_id)?,
        "implementation approval has been superseded or revoked"
    );
    Ok(())
}

fn decision_is_active(
    db: &Connection,
    decision: &ApprovalDecision,
    decision_id: &str,
) -> Result<bool> {
    let active = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_delivery_heads h JOIN job_pipeline_runs r ON r.id = h.pipeline_run_id
         WHERE h.project_id = ?1 AND h.task_id = ?2 AND h.pipeline_run_id = ?3
           AND h.plan_revision = ?4 AND r.plan_revision = h.plan_revision AND h.decision_artifact_id = ?5)",
        params![decision.task.project_id, decision.task.task_id, decision.workflow_run_id, decision.plan_revision, decision_id],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(active && rules::rule_authorizes(db, decision)?)
}

pub(super) fn active_approval(
    db: &Connection,
    target: &ApprovalDecision,
) -> Result<Option<String>> {
    let id: Option<String> = db.query_row(
        "SELECT decision_artifact_id FROM workflow_delivery_heads WHERE project_id = ?1 AND task_id = ?2
            AND pipeline_run_id = ?3 AND plan_revision = ?4",
        params![target.task.project_id, target.task.task_id, target.workflow_run_id, target.plan_revision],
        |row| row.get(0),
    ).optional()?;
    let Some(id) = id else {
        return Ok(None);
    };
    let saved = load_artifact(
        db,
        &target.task.project_id,
        &target.workflow_run_id,
        &id,
        ArtifactKind::Approval,
    )?;
    let Some(ArtifactValue::Approval(decision)) = saved.map(|artifact| artifact.value) else {
        return Ok(None);
    };
    Ok((decision.node_instance_id == target.node_instance_id
        && decision.task == target.task
        && decision.task_artifact_id == target.task_artifact_id
        && decision.source_checkout_id == target.source_checkout_id
        && decision_is_active(db, &decision, &id)?)
    .then_some(id))
}

pub(super) fn issue_prepared_release(
    db: &Connection,
    decision: ApprovalDecision,
    decision_artifact_id: String,
    preparation_node: String,
    worktree: ManagedWorktree,
    units: Vec<WorkflowPlanUnit>,
) -> Result<WorkflowArtifact> {
    require_active_decision(db, &decision, &decision_artifact_id)?;
    let project = decision.task.project_id.clone();
    let run = decision.workflow_run_id.clone();
    ensure!(
        worktree.project_id == project,
        "prepared checkout is outside the approved project"
    );
    let release = issue_pipeline_release(
        db,
        PipelineReleaseScope {
            project: &project,
            pipeline_run: &run,
            approval_node: &decision.node_instance_id,
            plan_revision: &decision.plan_revision,
            checkout: &worktree.checkout_id,
            task: &decision.task.task_id,
        },
    )?;
    let release_id = release.id.clone();
    let release_artifact = insert_artifact(
        db,
        &project,
        run.clone(),
        preparation_node.clone(),
        "release".into(),
        ArtifactValue::Release(ApprovalReference {
            workflow_run_id: run.clone(),
            decision_artifact_id: decision_artifact_id.clone(),
            release,
        }),
    )?;
    db.execute(
        "INSERT INTO workflow_release_bindings VALUES (?1, ?2, ?3)",
        params![release_id, decision_artifact_id, release_artifact.id],
    )?;
    record_delivery_units(db, &decision, &worktree.checkout_id, units)?;
    insert_artifact(
        db,
        &project,
        run,
        preparation_node,
        "worktree".into(),
        ArtifactValue::Worktree {
            checkout_id: worktree.checkout_id,
            task: decision.task,
        },
    )
}
