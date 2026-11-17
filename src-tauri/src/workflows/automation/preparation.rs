use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{diagnostics, store::Store};

use super::super::{
    ArtifactValue, NodeInputs, WorkflowArtifact, WorkflowHostRequest, WorkflowHostSubmission,
    WorkflowRun,
};

pub(crate) struct PendingPreparation {
    project: String,
    run: String,
    node: String,
    decision: String,
    task: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticPreparationWait {
    node_id: String,
    reason: Option<String>,
}

pub(crate) fn note_ready_preparation(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    decision: &WorkflowArtifact,
    task: &WorkflowArtifact,
) -> Result<()> {
    let enabled: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM workflow_automatic_runs WHERE pipeline_run_id = ?1)",
        [&run.id],
        |row| row.get(0),
    )?;
    if !enabled {
        return Ok(());
    }
    let ArtifactValue::Approval(approval) = &decision.value else {
        anyhow::bail!("The preparation lost its selected approval");
    };
    ensure!(
        approval.task_artifact_id == task.id,
        "Preparation needs the approved task"
    );
    db.execute(
        "INSERT INTO workflow_automatic_preparations (pipeline_run_id, node_instance_id, decision_id, task_artifact_id, plan_key)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
        params![run.id, node, decision.id, task.id, approval.plan_revision],
    )?;
    Ok(())
}

pub(crate) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<AutomaticPreparationWait>> {
    let mut statement = db.prepare(
        "SELECT p.node_instance_id, p.reason FROM workflow_automatic_preparations p
         JOIN workflow_delivery_heads h ON h.pipeline_run_id = p.pipeline_run_id AND h.decision_artifact_id = p.decision_id
         WHERE p.pipeline_run_id = ?1 AND p.plan_key = ?2 AND p.worktree_artifact_id IS NULL
         ORDER BY p.node_instance_id",
    )?;
    let waits = statement
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or_default()],
            |row| {
                Ok(AutomaticPreparationWait {
                    node_id: row.get(0)?,
                    reason: row.get(1)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(waits)
}

impl Store {
    pub(crate) async fn next_workflow_preparation(&self) -> Result<Option<PendingPreparation>> {
        self.execute(|db| {
            Ok(db.query_row(
                "SELECT h.project_id, p.pipeline_run_id, p.node_instance_id, p.decision_id, p.task_artifact_id
                 FROM workflow_automatic_preparations p
                 JOIN workflow_delivery_heads h ON h.pipeline_run_id = p.pipeline_run_id AND h.decision_artifact_id = p.decision_id
                 JOIN job_pipeline_runs r ON r.id = p.pipeline_run_id AND r.plan_revision = p.plan_key
                 WHERE p.worktree_artifact_id IS NULL AND p.retry_after <= unixepoch()
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY p.retry_after, p.pipeline_run_id, p.node_instance_id LIMIT 1",
                [], |row| Ok(PendingPreparation {
                    project: row.get(0)?, run: row.get(1)?, node: row.get(2)?, decision: row.get(3)?, task: row.get(4)?,
                }),
            ).optional()?)
        }).await
    }
}

impl PendingPreparation {
    pub(crate) async fn execute(
        &self,
        store: &Store,
        default_root: PathBuf,
    ) -> Result<WorkflowArtifact> {
        let result = store
            .execute_workflow_host(WorkflowHostSubmission {
                project_id: self.project.clone(),
                run_id: self.run.clone(),
                node_id: self.node.clone(),
                inputs: NodeInputs::from([
                    ("decision".into(), self.decision.clone()),
                    ("task".into(), self.task.clone()),
                ]),
                request: WorkflowHostRequest::PrepareWorktree { default_root },
            })
            .await;
        let reason = result
            .as_ref()
            .err()
            .map(|error| diagnostics::preview(&error.to_string()));
        let output = result.as_ref().ok().map(|artifact| artifact.id.clone());
        let (run, node, decision) = (self.run.clone(), self.node.clone(), self.decision.clone());
        store
            .execute(move |db| {
                db.execute(
                "UPDATE workflow_automatic_preparations SET worktree_artifact_id = ?4, reason = ?5,
                   retry_after = unixepoch() + 30
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND decision_id = ?3
                   AND worktree_artifact_id IS NULL",
                params![run, node, decision, output, reason],
            )?;
                Ok(())
            })
            .await
            .context("Could not record the automatic preparation result")?;
        result
    }
}
