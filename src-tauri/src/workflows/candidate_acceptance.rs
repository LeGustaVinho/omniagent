use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    protection,
    repository::verification::{
        CandidateGateOutcome, SelectedCandidate, load_scoped_candidate_verification,
        test_phase::{TestPhase, TestVerdict},
    },
};

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, NodeInputs, WorkflowArtifact, WorkflowDeliveryUnit,
    artifacts::load_artifact,
    control::recorded_control,
    data_nodes::EvidenceOutcome,
    runtime::{load_definition, load_pinned_node, load_workflow_run},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateAcceptanceConfiguration {
    pub build_check: String,
    pub minimum_reviews: u16,
    #[serde(default)]
    pub require_green: bool,
}

impl CandidateAcceptanceConfiguration {
    pub(super) fn valid(&self) -> bool {
        !self.build_check.trim().is_empty() && (1..=32).contains(&self.minimum_reviews)
    }

    pub(super) fn verify(
        &self,
        db: &Connection,
        unit: &WorkflowDeliveryUnit,
        candidate: &SelectedCandidate,
        inputs: &BTreeMap<String, WorkflowArtifact>,
    ) -> Result<String> {
        let project = &unit.project_id;
        let run = load_workflow_run(db, project, &unit.workflow_run_id)?
            .context("The acceptance workflow is unavailable")?;
        let quality = inputs
            .get("quality")
            .context("Acceptance needs an aggregate quality decision")?;
        let build = inputs
            .get("build")
            .context("Acceptance needs the exact candidate build result")?;
        for (artifact, expected_kind) in [
            (quality, BuiltinNode::QualityGate),
            (build, BuiltinNode::VerifyCandidate),
        ] {
            ensure!(
                artifact.unit_id.as_deref() == Some(unit.unit.id.as_str())
                    && load_pinned_node(db, project, &run.id, &artifact.node_instance_id)?
                        .is_some_and(|node| node.instance.kind == expected_kind),
                "Acceptance evidence is outside the required native producer or unit"
            );
        }
        ensure!(
            recorded_control(db, &run)?
                .iter()
                .any(|receipt| receipt.node_id == quality.node_instance_id
                    && receipt.unit_id.as_deref() == Some(unit.unit.id.as_str())
                    && receipt.plan_revision.as_deref() == Some(unit.plan_revision.as_str())
                    && receipt.selected_ports == ["passed"]),
            "The pinned quality gate did not pass"
        );
        let definition = load_definition(db, project, &run.version)?
            .context("The acceptance graph is unavailable")?;
        ensure!(
            definition
                .wires
                .iter()
                .any(|wire| wire.from.node == quality.node_instance_id
                    && wire.from.port == "passed"
                    && wire.to.node == build.node_instance_id
                    && wire.to.port == "ready"),
            "Connect the verified quality decision to the build readiness input"
        );

        let mut receipts = db.prepare(
            "SELECT payload FROM workflow_completion_inputs WHERE output_artifact_id = ?1 AND execution_key = ?2",
        )?;
        let inputs = receipts
            .query_map(params![quality.id, unit.plan_revision], |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let [payload] = inputs.as_slice() else {
            anyhow::bail!("The quality gate lost its exact input receipt");
        };
        let source: NodeInputs = serde_json::from_slice(&protection::unprotect(payload)?)?;
        let evidence_id = source
            .get("evidence")
            .context("The quality gate lost its evidence set")?;
        let artifact = load_artifact(db, project, &run.id, evidence_id, ArtifactKind::EvidenceSet)?
            .context("The pinned evidence set is unavailable")?;
        let ArtifactValue::EvidenceSet(evidence) = artifact.value else {
            unreachable!()
        };
        let mut review_attempts = BTreeSet::new();
        let mut green = false;
        for entry in evidence {
            let EvidenceOutcome::Completed { artifact_id, value } = entry.outcome else {
                anyhow::bail!("The quality gate includes an incomplete result");
            };
            match value.get("kind").and_then(Value::as_str) {
                Some("json_object") => {
                    let artifact = load_artifact(
                        db,
                        project,
                        &run.id,
                        &artifact_id,
                        ArtifactKind::JsonObject,
                    )?
                    .context("A review report is unavailable")?;
                    let review_producer =
                        load_pinned_node(db, project, &run.id, &artifact.node_instance_id)?
                            .is_some_and(|node| {
                                node.instance.kind == BuiltinNode::ReviewCandidate
                                    && artifact.port == "report"
                            });
                    if review_producer {
                        ensure!(
                            artifact.unit_id.as_deref() == Some(unit.unit.id.as_str())
                                && value.pointer("/evidence/verdict").and_then(Value::as_str)
                                    == Some("approved")
                                && value
                                    .pointer("/evidence/provenance/candidateId")
                                    .and_then(Value::as_str)
                                    == Some(&candidate.id)
                                && value
                                    .pointer("/evidence/provenance/tree")
                                    .and_then(Value::as_str)
                                    == Some(&candidate.tree),
                            "A review report does not approve the selected candidate tree"
                        );
                        let attempt = value
                            .pointer("/evidence/provenance/runId")
                            .and_then(Value::as_str)
                            .context("A review report lost its attempt identity")?;
                        review_attempts.insert(attempt.to_owned());
                    }
                }
                Some("test_phase_evidence") => {
                    let artifact = load_artifact(
                        db,
                        project,
                        &run.id,
                        &artifact_id,
                        ArtifactKind::TestPhaseEvidence,
                    )?
                    .context("The recorded test phase is unavailable")?;
                    let ArtifactValue::TestPhaseEvidence(test) = artifact.value else {
                        unreachable!()
                    };
                    if test.phase == TestPhase::Green {
                        ensure!(
                            artifact.unit_id.as_deref() == Some(unit.unit.id.as_str())
                                && test.candidate_id.as_ref() == Some(&candidate.id)
                                && test.result.tree.as_ref() == Some(&candidate.tree)
                                && test.result.verdict == TestVerdict::Verified,
                            "The green test phase does not verify the selected candidate tree"
                        );
                        green = true;
                    }
                }
                Some("checks") => {
                    let artifact =
                        load_artifact(db, project, &run.id, &artifact_id, ArtifactKind::Checks)?
                            .context("The recorded quality checks are unavailable")?;
                    let ArtifactValue::Checks(checks) = artifact.value else {
                        unreachable!()
                    };
                    let verification =
                        load_scoped_candidate_verification(db, project, &unit.task_id, &checks.id)?
                            .context("The quality check receipt is unavailable")?;
                    ensure!(
                        verification.candidate_id == candidate.id
                            && verification.passes_stage_checks(),
                        "The quality check did not pass on the selected candidate"
                    );
                }
                _ => {}
            }
        }
        ensure!(
            review_attempts.len() >= usize::from(self.minimum_reviews),
            "The candidate needs more independent approved reviews"
        );
        ensure!(
            !self.require_green || green,
            "The quality gate needs verified green test evidence for the candidate"
        );
        let ArtifactValue::Checks(build) = &build.value else {
            anyhow::bail!("The build needs a checks receipt");
        };
        let checks = load_scoped_candidate_verification(db, project, &unit.task_id, &build.id)?
            .context("The build verification is unavailable")?;
        ensure!(
            checks.candidate_id == candidate.id
                && checks.allows_acceptance()
                && checks
                    .gates
                    .iter()
                    .any(|gate| gate.gate.name == self.build_check
                        && gate.gate.required
                        && gate.executable_revision.is_some()
                        && matches!(gate.outcome, Some(CandidateGateOutcome::Passed { .. }))),
            "The required build did not pass on the selected candidate tree"
        );
        Ok(build.id.clone())
    }
}
