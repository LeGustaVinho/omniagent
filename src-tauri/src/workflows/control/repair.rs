use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::{
    jobs::PipelineDispatch,
    protection,
    repository::verification::{
        CandidateVerification, load_scoped_candidate_verification, load_selected_candidate,
        require_repair_check_policy,
    },
    store::Store,
};

use super::{
    super::{
        ArtifactKind, ArtifactValue, BuiltinNode, Endpoint, NodeConfiguration, NodeInputs,
        PinnedNode, WorkflowDefinition, WorkflowDeliveryUnit, WorkflowRun,
        artifacts::load_artifact,
        runtime::{load_definition, load_workflow_run},
        units::validate_unit_capture,
    },
    ConditionPredicate, ControlStageState, recorded_control,
};

const DIAGNOSTIC_BYTE_BUDGET: usize = 24_000;

fn lookup<'a>(definition: &'a WorkflowDefinition, id: &str) -> Option<&'a PinnedNode> {
    definition.nodes.iter().find(|node| node.instance.id == id)
}

fn checks_condition(definition: &WorkflowDefinition, id: &str) -> bool {
    lookup(definition, id).is_some_and(|node| {
        matches!(
            node.instance.configuration,
            NodeConfiguration::Condition {
                predicate: ConditionPredicate::CandidateChecksPassed
            }
        )
    })
}

fn verifies_repair(definition: &WorkflowDefinition, node: &str) -> bool {
    definition.wires.iter().any(|wire| {
        wire.from.node == node
            && wire.from.port == "checks"
            && wire.to.port == "value"
            && checks_condition(definition, &wire.to.node)
    })
}

impl Store {
    pub(in crate::workflows) async fn workflow_candidate_check_is_repair(
        &self,
        project: String,
        run: String,
        node: String,
    ) -> Result<bool> {
        self.execute(move |db| {
            let (_, definition) = run_definition(db, &project, &run)?;
            Ok(verifies_repair(&definition, &node))
        })
        .await
    }
}

fn run_definition(
    db: &Connection,
    project: &str,
    run: &str,
) -> Result<(WorkflowRun, WorkflowDefinition)> {
    let run = load_workflow_run(db, project, run)?
        .context("The pinned repair workflow is unavailable")?;
    let definition = load_definition(db, project, &run.version)?
        .context("The pinned repair definition is unavailable")?;
    Ok((run, definition))
}

pub(in crate::workflows) fn repair_instruction(
    db: &Connection,
    binding: &PipelineDispatch,
    unit: &WorkflowDeliveryUnit,
) -> Result<Option<String>> {
    let (run, definition) = run_definition(db, &unit.project_id, &binding.pipeline_run_id)?;
    for node in definition
        .wires
        .iter()
        .filter(|wire| {
            wire.from.node == binding.node_instance_id
                && wire.from.port == "candidate"
                && wire.to.port == "candidate"
        })
        .filter_map(|wire| lookup(&definition, &wire.to.node))
        .filter(|node| {
            node.instance.kind == BuiltinNode::VerifyCandidate
                && verifies_repair(&definition, &node.instance.id)
        })
    {
        let NodeConfiguration::RequiredChecks { names } = &node.instance.configuration else {
            anyhow::bail!("The pinned repair verification lost its check configuration");
        };
        require_repair_check_policy(db, &unit.project_id, names.clone())?;
    }
    let source = |node: &str, port: &str| {
        definition
            .wires
            .iter()
            .find(|wire| wire.to.node == node && wire.to.port == port)
            .map(|wire| &wire.from)
    };
    let Some(ready) = source(&binding.node_instance_id, "ready")
        .filter(|ready| ready.port == "unmatched" && checks_condition(&definition, &ready.node))
    else {
        return Ok(None);
    };
    let Some(checks) = source(&ready.node, "value").filter(|checks| {
        checks.port == "checks"
            && lookup(&definition, &checks.node)
                .is_some_and(|node| node.instance.kind == BuiltinNode::VerifyCandidate)
    }) else {
        return Ok(None);
    };
    let previous_writer = source(&checks.node, "candidate")
        .filter(|writer| {
            writer.port == "candidate"
                && lookup(&definition, &writer.node)
                    .is_some_and(|node| node.instance.kind == BuiltinNode::Implement)
        })
        .context("Repair feedback has no pinned prior writer")?;
    let receipt = recorded_control(db, &run)?
        .into_iter()
        .find(|receipt| {
            receipt.node_id == ready.node
                && receipt.unit_id.as_deref() == Some(&unit.unit.id)
                && receipt.plan_revision.as_deref() == Some(&unit.plan_revision)
                && receipt.state == ControlStageState::Completed
                && receipt.selected_ports == ["unmatched"]
        })
        .context("Repair feedback requires the prior check condition's unmatched outcome")?;
    let checks_artifact = receipt
        .dependencies
        .into_iter()
        .find(|dependency| {
            dependency.selected && dependency.to_port == "value" && dependency.from == *checks
        })
        .and_then(|dependency| dependency.artifact_id)
        .context("The unmatched condition lost its exact candidate check artifact")?;
    let verification =
        prior_verification(db, binding, unit, checks, previous_writer, &checks_artifact)?;
    let diagnostics = repair_diagnostics(&verification)?;
    Ok(Some(serde_json::to_string(&json!({
        "conditionNode": ready.node,
        "checksArtifactId": checks_artifact,
        "verificationId": verification.id,
        "candidateId": verification.candidate_id,
        "policyRevision": verification.policy_revision,
        "unitId": unit.unit.id,
        "releaseId": binding.release_id,
        "gates": diagnostics,
    }))?))
}

fn prior_verification(
    db: &Connection,
    binding: &PipelineDispatch,
    unit: &WorkflowDeliveryUnit,
    checks: &Endpoint,
    writer: &Endpoint,
    id: &str,
) -> Result<CandidateVerification> {
    let project = &unit.project_id;
    let task = &unit.task_id;
    let run = &unit.workflow_run_id;
    let artifact = load_artifact(db, project, run, id, ArtifactKind::Checks)?
        .context("The saved repair verification artifact is unavailable")?;
    let ArtifactValue::Checks(reference) = artifact.value else {
        unreachable!()
    };
    ensure!(
        artifact.node_instance_id == checks.node
            && artifact.port == checks.port
            && artifact.unit_id.as_deref() == Some(&unit.unit.id)
            && reference.task.project_id == *project
            && reference.task.task_id == *task,
        "Repair feedback belongs to a different verification node or unit"
    );
    let receipt = db
        .prepare(
            "SELECT payload FROM workflow_completion_inputs
        WHERE output_artifact_id = ?1 AND execution_key = ?2 LIMIT 2",
        )?
        .query_map(params![id, unit.plan_revision], |row| {
            row.get::<_, Vec<u8>>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let [receipt] = receipt.as_slice() else {
        anyhow::bail!("Repair feedback has no unique completion receipt for this plan");
    };
    let inputs: NodeInputs = serde_json::from_slice(&protection::unprotect(receipt)?)?;
    let candidate = load_artifact(
        db,
        project,
        run,
        inputs
            .get("candidate")
            .context("The prior checks did not identify their candidate")?,
        ArtifactKind::Candidate,
    )?
    .context("The prior checks' candidate is unavailable")?;
    let ArtifactValue::Candidate(selection) = candidate.value else {
        unreachable!()
    };
    ensure!(
        candidate.node_instance_id == writer.node
            && candidate.port == writer.port
            && candidate.unit_id.as_deref() == Some(&unit.unit.id)
            && load_selected_candidate(db, project, task, &selection.id)? == selection,
        "Repair feedback does not belong to the prior writer's selected candidate"
    );
    validate_unit_capture(db, unit, &selection.target.run_id)?;
    let release = binding
        .release_id
        .as_deref()
        .context("Repair feedback needs a pinned implementation release")?;
    let same_release: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM job_attempts a JOIN job_pipeline_origins o ON o.job_id = a.job_id
        WHERE a.run_id = ?1 AND o.node_instance_id = ?2 AND o.release_id = ?3)",
        params![selection.target.run_id, writer.node, release], |row| row.get(0))?;
    ensure!(
        same_release,
        "The previous checks ran under a different implementation release"
    );
    let verification = load_scoped_candidate_verification(db, project, task, &reference.id)?
        .context("The exact previous check receipt is unavailable")?;
    ensure!(
        verification.candidate_id == selection.id
            && verification.finished_at.is_some()
            && !verification.passes_repair_checks(),
        "The previous verification did not record a failed repair for this candidate"
    );
    Ok(verification)
}

fn repair_diagnostics(verification: &CandidateVerification) -> Result<Vec<Value>> {
    let limit = DIAGNOSTIC_BYTE_BUDGET / verification.gates.len().saturating_mul(2).max(1);
    verification
        .gates
        .iter()
        .map(|gate| {
            let mut outcome = serde_json::to_value(&gate.outcome)?;
            let mut excerpt_truncated = false;
            for key in ["stdout", "stderr"] {
                if let Some(text) = outcome.get(key).and_then(Value::as_str)
                    && text.len() > limit
                {
                    outcome[key] = Value::String(text[..text.floor_char_boundary(limit)].into());
                    excerpt_truncated = true;
                }
            }
            Ok(
                json!({"name": gate.gate.name, "required": gate.gate.required, "outcome": outcome,
            "outputExcerptTruncated": excerpt_truncated}),
            )
        })
        .collect()
}
