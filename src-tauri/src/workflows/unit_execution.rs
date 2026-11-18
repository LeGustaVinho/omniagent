use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::protection;

use super::{ArtifactValue, NodeConfiguration, WorkflowArtifact, WorkflowDeliveryUnit};

pub(crate) fn validate_unit_acceptance(
    db: &Connection,
    unit: &WorkflowDeliveryUnit,
    acceptance_id: &str,
    artifact_id: &str,
) -> Result<()> {
    let payload: Vec<u8> = db.query_row(
        "SELECT payload FROM workflow_artifacts WHERE pipeline_run_id = ?1 AND unit_id = ?2 AND kind = 'acceptance' AND id = ?3",
        params![unit.workflow_run_id, unit.unit.id, artifact_id], |row| row.get(0),
    ).optional()?.context("candidate acceptance artifact is unavailable in this unit")?;
    let artifact: WorkflowArtifact = serde_json::from_slice(&protection::unprotect(&payload)?)?;
    ensure!(
        matches!(artifact.value, ArtifactValue::Acceptance(reference)
        if reference.evidence.id == acceptance_id && reference.evidence.task.project_id == unit.project_id
        && reference.evidence.task.task_id == unit.task_id && reference.completion_criteria == unit.unit.completion_criteria),
        "candidate acceptance lacks the exact workflow review and unit criteria"
    );
    if super::automation::enabled(db, &unit.workflow_run_id)?
        && super::runtime::load_pinned_node(
            db,
            &unit.project_id,
            &unit.workflow_run_id,
            &artifact.node_instance_id,
        )?
        .is_some_and(|node| {
            matches!(
                node.instance.configuration,
                NodeConfiguration::CandidateAcceptance(_)
            )
        })
    {
        let writer: String = db.query_row(
            "SELECT j.job_id FROM candidate_acceptances a JOIN selected_candidate_revisions c ON c.id = a.candidate_id
             JOIN job_attempts j ON j.run_id = c.capture_run_id
             WHERE a.id = ?1 AND c.project_id = ?2 AND c.task_id = ?3 AND c.unit_id = ?4",
            params![acceptance_id, unit.project_id, unit.task_id, unit.unit.id],
            |row| row.get(0),
        )?;
        crate::jobs::load_job(db, &unit.project_id, &writer)?
            .context("The accepted candidate lost its approved writer job")?
            .require_workflow_writer_release(db)?;
    }
    Ok(())
}
