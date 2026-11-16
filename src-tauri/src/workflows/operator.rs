use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    jobs::Job,
    protection,
    providers::{ProviderEvent, ProviderEventData, SessionMode},
    repository::verification::load_current_candidate_acceptance,
    store::Store,
};

use super::{
    ArtifactValue, Executor, NodeInputs, WorkflowArtifact, WorkflowDeliveryUnit, WorkflowRevision,
    WorkflowRun,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowOperatorJob {
    pub node_id: String,
    pub unit_id: Option<String>,
    pub inputs: NodeInputs,
    pub job: Job,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanProposal {
    pub sequence: String,
    pub text: String,
}

pub struct WorkflowAgentTarget {
    pub checkout_id: String,
    pub task_id: Option<String>,
    pub mode: SessionMode,
    pub plan_revision: Option<String>,
    pub release_id: Option<String>,
}

pub(crate) fn waiting_for_delivery_commit(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<bool> {
    let mut statement = db.prepare(
        "SELECT a.payload FROM workflow_delivery_heads h
         JOIN job_pipeline_runs r ON r.id = h.pipeline_run_id AND r.plan_revision = h.plan_revision
         JOIN workflow_delivery_units u ON u.pipeline_run_id = h.pipeline_run_id
              AND u.plan_revision = h.plan_revision AND u.task_id = h.task_id AND u.commit_evidence_id IS NULL
         JOIN workflow_artifacts a ON a.pipeline_run_id = h.pipeline_run_id AND a.unit_id = u.unit_id
         WHERE h.project_id = ?1 AND h.task_id = ?2 AND a.kind = 'acceptance'",
    )?;
    let mut rows = statement.query(params![project, task])?;
    while let Some(row) = rows.next()? {
        let payload: Vec<u8> = row.get(0)?;
        let artifact: WorkflowArtifact = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        if let ArtifactValue::Acceptance(reference) = artifact.value
            && load_current_candidate_acceptance(db, project, task, &reference.evidence.id).is_ok()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Store {
    pub async fn workflow_operator_units(
        &self,
        project: String,
        task: String,
        run: String,
    ) -> Result<Vec<WorkflowDeliveryUnit>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT u.pipeline_run_id, u.plan_revision, u.project_id,
                    u.task_id, u.checkout_id, u.unit, u.commit_evidence_id
                 FROM workflow_delivery_units u
                 JOIN job_pipeline_runs r ON r.id = u.pipeline_run_id AND r.plan_revision = u.plan_revision
                 WHERE u.project_id = ?1 AND u.task_id = ?2 AND u.pipeline_run_id = ?3
                 ORDER BY u.ordinal",
            )?;
            let mut rows = statement.query(params![project, task, run])?;
            let mut units = Vec::new();
            while let Some(row) = rows.next()? {
                let protected: Vec<u8> = row.get(5)?;
                units.push(WorkflowDeliveryUnit {
                    workflow_run_id: row.get(0)?, plan_revision: row.get(1)?, project_id: row.get(2)?,
                    task_id: row.get(3)?, checkout_id: row.get(4)?,
                    unit: serde_json::from_slice(&protection::unprotect(&protected)?)?,
                    commit_evidence_id: row.get(6)?,
                });
            }
            Ok(units)
        }).await
    }

    pub async fn current_workflow_host_outputs(
        &self,
        project: String,
        run: String,
    ) -> Result<NodeInputs> {
        self.execute(move |db| {
            Ok(db
                .prepare(
                    "SELECT c.node_instance_id, c.artifact_id FROM workflow_host_completions c
                 JOIN job_pipeline_runs r ON r.id = c.pipeline_run_id
                 WHERE r.project_id = ?1 AND r.id = ?2
                   AND (c.execution_key = '' OR c.execution_key = r.plan_revision)
                   AND NOT EXISTS (
                       SELECT 1 FROM workflow_approval_decisions d
                       WHERE d.decision_artifact_id = c.artifact_id AND d.rule_revision IS NOT NULL
                         AND NOT EXISTS (SELECT 1 FROM workflow_approval_rule_heads h WHERE h.revision = d.rule_revision)
                   )
                 ORDER BY length(c.execution_key)",
                )?
                .query_map(params![project, run], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<NodeInputs>>()?)
        })
        .await
    }

    pub async fn list_workflow_runs(&self, project: String) -> Result<Vec<WorkflowRun>> {
        self.execute(move |db| {
            Ok(db
                .prepare(
                    "SELECT w.pipeline_run_id, w.workflow_id, w.workflow_revision, r.plan_revision
                 FROM workflow_run_pins w JOIN job_pipeline_runs r ON r.id = w.pipeline_run_id
                 WHERE w.project_id = ?1 ORDER BY w.pipeline_run_id",
                )?
                .query_map([&project], |row| {
                    Ok(WorkflowRun {
                        id: row.get(0)?,
                        project_id: project.clone(),
                        version: WorkflowRevision {
                            id: row.get(1)?,
                            revision: row.get(2)?,
                        },
                        plan_revision: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn workflow_run_artifacts(
        &self,
        project: String,
        run: String,
    ) -> Result<Vec<WorkflowArtifact>> {
        self.execute(move |db| {
            let saved = db
                .prepare(
                    "SELECT a.payload FROM workflow_artifacts a
                 JOIN workflow_run_pins p ON p.pipeline_run_id = a.pipeline_run_id
                 WHERE p.project_id = ?1 AND p.pipeline_run_id = ?2
                 ORDER BY a.node_instance_id, a.port_name, a.id",
                )?
                .query_map(params![project, run], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            saved
                .into_iter()
                .map(|payload| Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?))
                .collect()
        })
        .await
    }

    pub async fn workflow_operator_jobs(
        &self,
        project: String,
        run: String,
    ) -> Result<Vec<WorkflowOperatorJob>> {
        let identities = self
            .execute({
                let project = project.clone();
                move |db| {
                    Ok(db
                        .prepare(
                            "SELECT o.node_instance_id, i.unit_id, o.job_id, i.payload
                     FROM job_pipeline_origins o JOIN workflow_job_inputs i ON i.job_id = o.job_id
                     JOIN jobs j ON j.id = o.job_id
                     WHERE o.project_id = ?1 AND o.pipeline_run_id = ?2
                     ORDER BY j.sequence",
                        )?
                        .query_map(params![project, run], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, Vec<u8>>(3)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?)
                }
            })
            .await?;
        let mut jobs = Vec::with_capacity(identities.len());
        for (node_id, unit_id, id, protected) in identities {
            if let Some(job) = self.job(project.clone(), id).await? {
                let staged: super::WorkflowJobInput =
                    serde_json::from_slice(&protection::unprotect(&protected)?)?;
                jobs.push(WorkflowOperatorJob {
                    node_id,
                    unit_id,
                    inputs: staged.artifacts,
                    job,
                });
            }
        }
        Ok(jobs)
    }

    pub async fn workflow_plan_proposals(
        &self,
        project: String,
        workflow: String,
        run: String,
    ) -> Result<Vec<WorkflowPlanProposal>> {
        let attempt = self
            .workflow_attempt(project, run.clone())
            .await?
            .context("the planning attempt is outside this project")?;
        ensure!(
            attempt.pin.workflow_run_id == workflow && attempt.pin.node_instance_id == "plan",
            "the planning attempt belongs to a different workflow"
        );
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT seq, payload FROM agent_events WHERE run_id = ?1 AND source IN ('provider', 'application') ORDER BY seq",
            )?;
            let mut rows = statement.query([run])?;
            let mut proposals = Vec::new();
            while let Some(row) = rows.next()? {
                let protected: Vec<u8> = row.get(1)?;
                let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&protected)?)?;
                if event.location.parent_item_id.is_none()
                    && let ProviderEventData::PlanProposed { text, .. } = event.data
                {
                    proposals.push(WorkflowPlanProposal { sequence: row.get::<_, i64>(0)?.to_string(), text });
                }
            }
            Ok(proposals)
        }).await
    }

    pub async fn workflow_operator_agent_target(
        &self,
        project: &str,
        run: &str,
        node: &str,
        unit_id: Option<&str>,
        inputs: &NodeInputs,
    ) -> Result<WorkflowAgentTarget> {
        let (definition, artifacts) = self
            .workflow_execution_inputs(project, run, node, unit_id, inputs)
            .await?;
        let mode = match definition.definition.executor {
            Executor::PlanAgent => SessionMode::Plan,
            Executor::ReadOnlyAgent => SessionMode::ReadOnly,
            Executor::WriteAgent => SessionMode::ApprovedWrite,
            _ => anyhow::bail!("this node does not execute a CLI role"),
        };
        let plan = artifacts.get("plan");
        let unit = if let Some(id) = unit_id {
            let (project, run, id) = (project.to_owned(), run.to_owned(), id.to_owned());
            Some(self.execute(move |db| {
                let task: String = db.query_row(
                    "SELECT task_id FROM workflow_delivery_units WHERE project_id = ?1 AND pipeline_run_id = ?2 AND unit_id = ?3",
                    params![project, run, id], |row| row.get(0),
                ).optional()?.context("the logical unit is outside this workflow")?;
                super::load_delivery_unit(db, &project, &task, &id)?
                    .context("the logical unit is no longer in the active plan")
            }).await?)
        } else {
            None
        };
        let checkout_id = if let Some(unit) = &unit {
            unit.checkout_id.clone()
        } else if let Some(ArtifactValue::Markdown(source)) =
            artifacts.get("context").map(|artifact| &artifact.value)
        {
            source.checkout_id.clone()
        } else if let Some(ArtifactValue::Plan { run_id, .. }) =
            plan.map(|artifact| &artifact.value)
        {
            self.workflow_plan_checkout(project, run_id).await?
        } else {
            anyhow::bail!("the agent inputs do not identify a registered checkout");
        };
        let release_id = match artifacts.get("release").map(|artifact| &artifact.value) {
            Some(ArtifactValue::Release(reference)) => Some(reference.release.id.clone()),
            _ => None,
        };
        Ok(WorkflowAgentTarget {
            checkout_id,
            mode,
            release_id,
            plan_revision: plan
                .map(|artifact| artifact.id.clone())
                .or_else(|| unit.as_ref().map(|unit| unit.plan_revision.clone())),
            task_id: unit.map(|unit| unit.task_id),
        })
    }
}
