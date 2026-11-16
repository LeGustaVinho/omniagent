use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    jobs::{JobRequest, PipelineDispatch},
    protection,
    repository::verification::{SelectedCandidate, require_current},
    store::Store,
};

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, NodeInputs,
    approval::require_current_plan,
    artifacts::load_artifact,
    runtime::load_pinned_node,
    units::{load_delivery_unit, load_delivery_units},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkflowJobInput {
    pub unit_id: Option<String>,
    pub artifacts: NodeInputs,
}

pub(crate) fn stage_job_input(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
    request: &JobRequest,
    input: &WorkflowJobInput,
) -> Result<()> {
    let pinned = load_pinned_node(
        db,
        project,
        &binding.pipeline_run_id,
        &binding.node_instance_id,
    )?
    .context("the scheduled workflow node is unavailable")?;
    ensure!(
        pinned.definition.kind.per_unit() == input.unit_id.is_some(),
        "workflow node has the wrong logical unit scope"
    );
    if let Some(plan) = &binding.plan_revision {
        require_current_plan(db, project, &binding.pipeline_run_id, plan)?;
    }
    if let Some(unit_id) = &input.unit_id {
        let task = request
            .task_id
            .as_deref()
            .context("logical unit jobs require the assigned task")?;
        let unit = load_delivery_unit(db, project, task, unit_id)?
            .context("logical unit is outside the active delivery")?;
        ensure!(
            unit.workflow_run_id == binding.pipeline_run_id
                && binding.plan_revision.as_ref() == Some(&unit.plan_revision)
                && request.checkout_id.as_ref() == Some(&unit.checkout_id),
            "scheduled job differs from its approved logical unit scope"
        );
        ensure!(
            unit.commit_evidence_id.is_none(),
            "logical unit already has a verified commit"
        );
        if pinned.definition.kind == super::BuiltinNode::Implement {
            let units = load_delivery_units(db, project, task, &binding.pipeline_run_id)?;
            ensure!(
                units
                    .iter()
                    .take_while(|other| other.unit.ordinal < unit.unit.ordinal)
                    .all(|other| other.commit_evidence_id.is_some()),
                "earlier logical units must have verified commits"
            );
            let open_writer: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_job_inputs i JOIN jobs j ON j.id = i.job_id
                 WHERE i.unit_id = ?1 AND j.mode = 'approved_write'
                 AND ((j.queue_state = 'queued' AND NOT EXISTS(SELECT 1 FROM job_attempts seen WHERE seen.job_id = j.id)) OR EXISTS(
                   SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id
                   WHERE a.job_id = j.id AND r.status IN ('starting', 'running', 'waiting_for_input')
                 )))",
                [unit_id], |row| row.get(0),
            )?;
            ensure!(
                !open_writer,
                "logical unit already has an active implementation job"
            );
        }
    }
    db.execute(
        "INSERT INTO workflow_job_admissions VALUES (?1, ?2)",
        params![binding.pipeline_run_id, binding.node_instance_id],
    )?;
    Ok(())
}

pub(crate) fn bind_job_input(
    db: &Connection,
    job: &str,
    binding: &PipelineDispatch,
    input: &WorkflowJobInput,
) -> Result<()> {
    db.execute(
        "INSERT INTO workflow_job_inputs VALUES (?1, ?2, ?3)",
        params![
            job,
            input.unit_id,
            protection::protect(&serde_json::to_vec(&input)?)?
        ],
    )?;
    db.execute(
        "DELETE FROM workflow_job_admissions WHERE pipeline_run_id = ?1 AND node_instance_id = ?2",
        params![binding.pipeline_run_id, binding.node_instance_id],
    )?;
    Ok(())
}

pub(super) fn load_attempt_input(db: &Connection, run: &str) -> Result<Option<WorkflowJobInput>> {
    let payload: Option<Vec<u8>> = db.query_row(
        "SELECT i.payload FROM workflow_job_inputs i JOIN job_attempts a ON a.job_id = i.job_id WHERE a.run_id = ?1",
        [run], |row| row.get(0),
    ).optional()?;
    payload
        .map(|payload| Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?))
        .transpose()
}

impl Store {
    pub(super) async fn workflow_attempt_input(
        &self,
        run: String,
    ) -> Result<Option<WorkflowJobInput>> {
        self.execute(move |db| load_attempt_input(db, &run)).await
    }

    pub(crate) async fn workflow_candidate_for_review_attempt(
        &self,
        run: String,
    ) -> Result<Option<SelectedCandidate>> {
        self.execute(move |db| {
            let Some(input) = load_attempt_input(db, &run)? else { return Ok(None); };
            let origin: (String, String, String, String) = db.query_row(
                "SELECT o.project_id, o.pipeline_run_id, o.node_instance_id, j.checkout_id
                 FROM job_pipeline_origins o JOIN job_attempts a ON a.job_id = o.job_id JOIN jobs j ON j.id = o.job_id WHERE a.run_id = ?1",
                [&run], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            let (project, workflow, node, checkout) = origin;
            let pinned = load_pinned_node(db, &project, &workflow, &node)?.context("workflow review node is unavailable")?;
            if pinned.definition.kind != BuiltinNode::ReviewCandidate { return Ok(None); }
            let id = input.artifacts.get("candidate").context("the candidate review was scheduled without a candidate")?;
            let artifact = load_artifact(db, &project, &workflow, id, ArtifactKind::Candidate)?.context("the review's selected candidate is unavailable")?;
            let ArtifactValue::Candidate(candidate) = artifact.value else { anyhow::bail!("the review needs a selected candidate"); };
            ensure!(input.unit_id.as_deref() == Some(&candidate.target.unit_id) && candidate.checkout_id == checkout,
                "candidate review cannot change its checkout or unit");
            let unit = load_delivery_unit(db, &project, &candidate.target.task_id, &candidate.target.unit_id)?
                .context("review unit is outside the active delivery")?;
            ensure!(unit.workflow_run_id == workflow && unit.commit_evidence_id.is_none(), "candidate review's delivery changed");
            require_current(db, &candidate)?;
            Ok(Some(candidate))
        }).await
    }
}
