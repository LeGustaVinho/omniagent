use crate::workflows::ArchivedNode;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{jobs::PipelineDispatch, protection};

use super::{
    super::{
        BuiltinNode, PinnedNode, WorkflowArtifact, WorkflowJobInput, WorkflowRun,
        generic_agent::stage_job, runtime::load_pinned_node,
    },
    AutomaticAgentWait, enabled,
};

pub(in crate::workflows) fn instructions(kind: BuiltinNode) -> String {
    match kind {
        BuiltinNode::Archived(ArchivedNode::Implement) => {
            "Implement the pinned logical unit on the approved branch. Keep unrelated changes out of the unit. Leave the changes in the worktree for review, checks and commit approval."
        }
        BuiltinNode::Archived(ArchivedNode::ReviewCandidate) => {
            "Review the selected candidate tree for correctness and evaluate the pinned unit criteria."
        }
        BuiltinNode::Archived(ArchivedNode::ReviewDiffCandidate) => {
            "Review the frozen Git diff and report findings with evidence from the selected comparison."
        }
        _ => "",
    }.into()
}

pub(in crate::workflows) fn enabled_for(
    db: &Connection,
    run: &str,
    node: &PinnedNode,
) -> Result<bool> {
    Ok(matches!(
        node.definition.kind,
        BuiltinNode::Archived(ArchivedNode::Implement)
            | BuiltinNode::Archived(ArchivedNode::ReviewCandidate)
    ) && has_output(node, "failed")
        && (super::prompt_delegate(node) || enabled(db, run)?))
}

pub(crate) fn require_unique_admission(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
    input: &WorkflowJobInput,
) -> Result<()> {
    let node = load_pinned_node(
        db,
        project,
        &binding.pipeline_run_id,
        &binding.node_instance_id,
    )?
    .context("The delivery agent stage is unavailable")?;
    if !super::prompt_delegate(&node) && !enabled(db, &binding.pipeline_run_id)? {
        return Ok(());
    }
    if matches!(
        node.definition.kind,
        BuiltinNode::Archived(ArchivedNode::Implement)
            | BuiltinNode::Archived(ArchivedNode::ReviewCandidate)
    ) && has_output(&node, "failed")
        || super::prompt_native_stage(&node)
    {
        ensure!(
            stage_job(
                db,
                project,
                &binding.pipeline_run_id,
                &binding.node_instance_id,
                input.unit_id.as_deref(),
                &input.artifacts
            )?
            .is_none(),
            "This delivery stage already has a job for the selected inputs"
        );
    }
    Ok(())
}

fn has_output(node: &PinnedNode, name: &str) -> bool {
    node.definition.outputs.iter().any(|port| port.name == name)
}

pub(in crate::workflows) fn sealed_output(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    source_run: &str,
    port: &str,
) -> Result<Option<WorkflowArtifact>> {
    let bytes: Option<Vec<u8>> = db.query_row(
        "SELECT a.payload FROM workflow_automatic_delivery_outputs o
         JOIN workflow_automatic_delivery_results r USING (pipeline_run_id, node_instance_id, request_key, attempt_id)
         JOIN workflow_artifacts a ON a.id = o.artifact_id
         WHERE r.pipeline_run_id = ?1 AND r.node_instance_id = ?2 AND r.unit_id IS ?3
           AND r.attempt_id = ?4 AND o.port_name = ?5 AND r.plan_key = ?6 AND r.state = 'published'",
        params![run.id, node, unit, source_run, port, run.plan_revision], |row| row.get(0),
    ).optional()?;
    bytes
        .map(|bytes| Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?))
        .transpose()
}

pub(in crate::workflows) fn waits(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Vec<AutomaticAgentWait>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, unit_id, coalesce(diagnostic, 'Verifying delivery agent evidence')
         FROM workflow_automatic_delivery_results WHERE pipeline_run_id = ?1 AND plan_key = ?2
           AND state = 'pending' ORDER BY node_instance_id, unit_id",
    )?;
    Ok(query
        .query_map(params![run.id, run.plan_revision], |row| {
            Ok(AutomaticAgentWait {
                node_id: row.get(0)?,
                unit_id: Some(row.get(1)?),
                reason: Some(row.get(2)?),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}
