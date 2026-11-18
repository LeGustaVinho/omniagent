use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{jobs::PipelineDispatch, store::Store};

use super::{ArtifactKind, ArtifactValue, artifacts::load_artifact};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanRevisionRequest {
    pub base_revision_id: String,
    pub expected_plan_revision: Option<String>,
    pub feedback: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanHistory {
    pub request: Option<WorkflowPlanRevisionRequest>,
    pub versions: Vec<WorkflowPlanVersion>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanVersion {
    pub revision_id: String,
    pub source_run_id: String,
    pub event_sequence: String,
    pub text: String,
    pub current: bool,
    pub approval_artifact_id: Option<String>,
    pub release_id: Option<String>,
    pub release_revoked: Option<bool>,
}

impl Store {
    pub async fn workflow_plan_history(
        &self,
        project: String,
        run_id: String,
    ) -> Result<Option<WorkflowPlanHistory>> {
        let Some(attempt) = self
            .workflow_attempt(project.clone(), run_id.clone())
            .await?
        else {
            return Ok(None);
        };
        let workflow = attempt.pin.workflow_run_id;
        let selected = self
            .workflow_run(project.clone(), workflow.clone())
            .await?
            .context("The recorded workflow is unavailable")?
            .plan_revision;
        let request = self
            .workflow_attempt_input(run_id.clone())
            .await?
            .and_then(|input| input.plan_revision_request);
        let artifacts = self
            .workflow_run_artifacts(project.clone(), workflow.clone())
            .await?;
        let active_outputs = self
            .current_workflow_host_outputs(project.clone(), workflow.clone())
            .await?;
        let attempt_decision = artifacts.iter().find_map(|artifact| match &artifact.value {
            ArtifactValue::Release(reference)
                if attempt.pin.release_id.as_ref() == Some(&reference.release.id) =>
            {
                Some(&reference.decision_artifact_id)
            }
            _ => None,
        });
        let mut versions = Vec::new();
        for artifact in &artifacts {
            let ArtifactValue::Plan {
                run_id: source,
                event_sequence,
            } = &artifact.value
            else {
                continue;
            };
            if source != &run_id
                && attempt.pin.plan_revision.as_ref() != Some(&artifact.id)
                && request.as_ref().map(|request| &request.base_revision_id) != Some(&artifact.id)
            {
                continue;
            }
            let mut approvals = artifacts.iter().filter(|item| {
                matches!(&item.value, ArtifactValue::Approval(decision) if decision.plan_revision == artifact.id)
            });
            let approval = approvals
                .clone()
                .find(|item| Some(&item.id) == attempt_decision)
                .or_else(|| {
                    approvals
                        .clone()
                        .find(|item| active_outputs.values().any(|id| id == &item.id))
                })
                .or_else(|| approvals.next());
            let release = approval.and_then(|approval| {
                artifacts.iter().find_map(|item| match &item.value {
                    ArtifactValue::Release(reference)
                        if reference.decision_artifact_id == approval.id =>
                    {
                        Some(&reference.release)
                    }
                    _ => None,
                })
            });
            let release_revoked = if let Some(release) = release {
                let (project, workflow, id, revision) = (
                    project.clone(),
                    workflow.clone(),
                    release.id.clone(),
                    artifact.id.clone(),
                );
                Some(self.execute(move |db| Ok(db.query_row(
                    "SELECT revoked FROM job_pipeline_releases
                     WHERE project_id = ?1 AND pipeline_run_id = ?2 AND id = ?3 AND plan_revision = ?4",
                    params![project, workflow, id, revision], |row| row.get::<_, bool>(0),
                )?)).await?)
            } else {
                None
            };
            versions.push((
                *event_sequence,
                WorkflowPlanVersion {
                    revision_id: artifact.id.clone(),
                    source_run_id: source.clone(),
                    event_sequence: event_sequence.to_string(),
                    text: self
                        .workflow_plan_text(source.clone(), *event_sequence)
                        .await?,
                    current: selected.as_ref() == Some(&artifact.id),
                    approval_artifact_id: approval.map(|approval| approval.id.clone()),
                    release_revoked,
                    release_id: release.map(|release| release.id.clone()),
                },
            ));
        }
        versions.sort_unstable_by_key(|(sequence, _)| *sequence);
        Ok(
            (request.is_some() || !versions.is_empty()).then(|| WorkflowPlanHistory {
                request,
                versions: versions.into_iter().map(|(_, version)| version).collect(),
            }),
        )
    }
}

pub(super) fn check_revision_admission(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
    revision: &WorkflowPlanRevisionRequest,
) -> Result<()> {
    let selected: Option<String> = db.query_row(
        "SELECT plan_revision FROM job_pipeline_runs WHERE project_id = ?1 AND id = ?2",
        params![project, binding.pipeline_run_id],
        |row| row.get(0),
    )?;
    ensure!(
        selected == revision.expected_plan_revision,
        "The selected plan changed before the revision was queued"
    );
    load_artifact(
        db,
        project,
        &binding.pipeline_run_id,
        &revision.base_revision_id,
        ArtifactKind::Plan,
    )?
    .context("The base plan is unavailable in this workflow")?;
    let pending: bool = db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM job_pipeline_origins o JOIN jobs j ON j.id = o.job_id
            WHERE o.project_id = ?1 AND o.pipeline_run_id = ?2 AND o.node_instance_id = ?3
              AND ((j.queue_state <> 'cancelled' AND NOT EXISTS (SELECT 1 FROM job_attempts a WHERE a.job_id = j.id))
                OR EXISTS (SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id
                           WHERE a.job_id = j.id AND r.status IN ('starting', 'running', 'waiting_for_input'))
                OR EXISTS (SELECT 1 FROM job_recoveries retry WHERE retry.job_id = j.id))
        )",
        params![project, binding.pipeline_run_id, binding.node_instance_id],
        |row| row.get(0),
    )?;
    ensure!(!pending, "A planning attempt is already queued or running");
    Ok(())
}
