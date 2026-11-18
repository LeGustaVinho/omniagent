use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{
    ArtifactKind, ArtifactValue, TaskReference, WorkflowArtifact, artifacts::load_artifact,
};

mod rules;
pub use rules::{
    ApprovalFacts, ApprovalRuleEvidence, ApprovalRuleOutcome, ApprovalRuleSettings,
    BacklogApprovalScope, UnmetApproval,
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
    let scope: Option<(String, String, String)> = db.query_row(
        "SELECT approval_node_id, plan_revision, task_id FROM job_pipeline_releases
         WHERE id = ?1 AND project_id = ?2 AND pipeline_run_id = ?3",
        params![release_id, project, run],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    if let Some((node, revision, task)) = scope
        && super::write_scope::release_is_human(db, project, run, &node)?
    {
        return Ok(Some(super::write_scope::current_release(db, project, run, &node, &revision, &task)?));
    }
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
    if super::runtime::load_definition(db, project, &workflow.version)?
        .is_none_or(|definition| definition.unsupported_node().is_some())
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
