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
    ArtifactKind, ArtifactValue, BuiltinNode, NodeConfiguration, NodeInputs, WorkflowArtifact,
    WorkflowDeliveryUnit, WorkflowRun,
    artifacts::load_artifact,
    control::{ControlStageState, recorded_control},
    data_nodes::EvidenceOutcome,
    prompt::{PromptCallStatus, PromptDecision, PromptOutcome, stage_alias},
    runtime::{load_definition, load_node_attempt, load_pinned_node, load_workflow_run},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateReviewMode {
    #[default]
    Minimum,
    Selected,
}

impl CandidateReviewMode {
    fn is_minimum(value: &Self) -> bool {
        *value == Self::Minimum
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateAcceptanceConfiguration {
    pub build_check: String,
    pub minimum_reviews: u16,
    #[serde(default)]
    pub require_green: bool,
    #[serde(default, skip_serializing_if = "CandidateReviewMode::is_minimum")]
    pub review_mode: CandidateReviewMode,
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
        let payloads = receipts
            .query_map(params![quality.id, unit.plan_revision], |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let [payload] = payloads.as_slice() else {
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
        let selected = if self.review_mode == CandidateReviewMode::Selected {
            Some(
                inputs
                    .get("selected_reviews")
                    .context("Acceptance needs the recorded Prompt review selection")?,
            )
        } else {
            None
        };
        let mut witnessed_selection = false;
        let mut review_attempts = BTreeSet::new();
        let mut green = false;
        for entry in evidence {
            let EvidenceOutcome::Completed { artifact_id, value } = entry.outcome else {
                anyhow::bail!("The quality gate includes an incomplete result");
            };
            witnessed_selection |= selected.is_some_and(|outcome| outcome.id == artifact_id);
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
        if let Some(selected) = selected {
            ensure!(
                witnessed_selection,
                "The quality gate did not use the selected review outcome"
            );
            self.verify_selected_reviews(db, &run, unit, candidate, selected, inputs)?;
        } else {
            ensure!(
                review_attempts.len() >= usize::from(self.minimum_reviews),
                "The candidate needs more independent approved reviews"
            );
        }
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

    fn verify_selected_reviews(
        &self,
        db: &Connection,
        run: &WorkflowRun,
        unit: &WorkflowDeliveryUnit,
        candidate: &SelectedCandidate,
        selected: &WorkflowArtifact,
        inputs: &BTreeMap<String, WorkflowArtifact>,
    ) -> Result<()> {
        let project = &run.project_id;
        let unit_id = unit.unit.id.as_str();
        ensure!(
            selected.unit_id.as_deref() == Some(unit_id)
                && selected.port == "outcome"
                && load_pinned_node(db, project, &run.id, &selected.node_instance_id)?
                    .is_some_and(|node| node.instance.kind == BuiltinNode::PromptOutcome),
            "Acceptance needs a native Prompt outcome from this delivery unit"
        );
        let receipts = recorded_control(db, run)?;
        let receipt = |node: &str| {
            receipts.iter().find(|receipt| {
                receipt.node_id == node
                    && receipt.unit_id.as_deref() == Some(unit_id)
                    && receipt.plan_revision.as_deref() == Some(unit.plan_revision.as_str())
                    && receipt.state == ControlStageState::Completed
            })
        };
        ensure!(
            receipt(&selected.node_instance_id).is_some(),
            "The Prompt review card did not complete"
        );
        let ArtifactValue::JsonObject(outcome) = &selected.value else {
            anyhow::bail!("The Prompt review outcome has the wrong type");
        };
        let outcome: PromptOutcome = serde_json::from_value(Value::Object(outcome.clone()))?;
        ensure!(
            outcome.approved
                && outcome.diagnostic.is_none()
                && outcome.selected.len() >= usize::from(self.minimum_reviews),
            "The selected Prompt review set did not approve the candidate"
        );
        let mut query = db.prepare(
            "SELECT payload FROM workflow_completion_inputs WHERE output_artifact_id = ?1 AND execution_key = ?2",
        )?;
        let rows = query
            .query_map(params![selected.id, unit.plan_revision], |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let [completion] = rows.as_slice() else {
            anyhow::bail!("The Prompt outcome lost its input receipt");
        };
        let provenance: NodeInputs = serde_json::from_slice(&protection::unprotect(completion)?)?;
        let decision_id = provenance
            .get("decision")
            .context("The Prompt lost its selected review decision")?;
        let decision = load_artifact(db, project, &run.id, decision_id, ArtifactKind::JsonObject)?
            .context("The Prompt review decision is unavailable")?;
        let decision_stage = load_pinned_node(db, project, &run.id, &decision.node_instance_id)?
            .context("The Prompt review decision stage is unavailable")?;
        let NodeConfiguration::PromptDecision(contract) = decision_stage.instance.configuration
        else {
            anyhow::bail!("The selection did not come from a published Prompt decision");
        };
        let card = selected
            .node_instance_id
            .rsplit_once("::")
            .map(|(card, _)| card)
            .context("The review outcome is outside its compiled Prompt card")?;
        let same_card = |node: &str| {
            node.rsplit_once("::")
                .is_some_and(|(prefix, _)| prefix == card)
        };
        ensure!(
            same_card(&decision.node_instance_id)
                && decision.port == "decision"
                && decision.unit_id.as_deref() == Some(unit_id)
                && receipt(&decision.node_instance_id).is_some(),
            "Acceptance needs the decision from the same completed Prompt card"
        );
        let ArtifactValue::JsonObject(value) = decision.value else {
            unreachable!()
        };
        let decision: PromptDecision = serde_json::from_value(Value::Object(value))?;
        ensure!(
            decision.selected == outcome.selected
                && decision.route == outcome.route
                && includes_required_reviews(&contract.delegates, &decision.selected),
            "Acceptance requires the selected reviewers and a mandatory independent correctness role"
        );
        let implementation = load_node_attempt(db, project, &candidate.target.run_id)?
            .context("The candidate lost its implementation attempt")?;
        let implementer =
            load_pinned_node(db, project, &run.id, &implementation.pin.node_instance_id)?
                .context("The candidate lost its implementation stage")?;
        let NodeConfiguration::AgentRole { requirements, .. } = implementer.instance.configuration
        else {
            anyhow::bail!("The candidate has no pinned implementer");
        };
        let mut role_ids = BTreeSet::new();
        if let Some(role) = requirements.bound_role {
            role_ids.insert((role.destination, role.id));
        }
        let mut visited = BTreeSet::new();
        let mut attempts = BTreeSet::new();
        for call in &outcome.calls {
            let Some(focus) = decision.selected.get(&call.role) else {
                ensure!(
                    call.status == PromptCallStatus::Skipped,
                    "A dispensable review ran"
                );
                continue;
            };
            ensure!(
                call.status == PromptCallStatus::Completed
                    && call.focus.as_ref() == Some(focus)
                    && call.approved == Some(true)
                    && visited.insert(call.role.as_str())
                    && same_card(&call.stage_id)
                    && stage_alias(&call.stage_id) == Some(&call.role),
                "A selected review did not complete in its own Prompt stage"
            );
            let attempt = call
                .attempt_id
                .as_deref()
                .context("The selected review lost its attempt")?;
            ensure!(
                attempt != candidate.target.run_id
                    && attempts.insert(attempt)
                    && receipt(&call.stage_id)
                        .is_some_and(|finish| finish.source_run_id.as_deref() == Some(attempt)),
                "A selected review reused a different stage or attempt"
            );
            let reviewer = load_pinned_node(db, project, &run.id, &call.stage_id)?
                .context("A selected reviewer lost its stage")?;
            ensure!(
                reviewer.instance.kind == BuiltinNode::ReviewCandidate,
                "Acceptance needs native candidate reviewers"
            );
            let NodeConfiguration::AgentRole { requirements, .. } = reviewer.instance.configuration
            else {
                anyhow::bail!("A selected reviewer is missing its Role revision");
            };
            let role = requirements
                .bound_role
                .context("A selected reviewer is missing its Role revision")?;
            ensure!(
                role_ids.insert((role.destination, role.id)),
                "Candidate reviewers must have independent roles"
            );
            let report = super::automation::agents_delivery::sealed_output(
                db,
                run,
                &call.stage_id,
                Some(unit_id),
                attempt,
                "report",
            )?
            .context("A selected review has no sealed report")?;
            let ArtifactValue::JsonObject(value) = report.value else {
                unreachable!()
            };
            ensure!(
                call.artifact_id.as_deref() == Some(&report.id)
                    && approves_selected_tree(&value, &candidate.id, &candidate.tree, attempt),
                "A selected review did not approve the exact candidate tree"
            );
            crate::repository::verification::require_candidate_review_dispatch(
                db, attempt, candidate,
            )?;
            if call.role == "correctness" {
                let review = inputs
                    .get("review")
                    .context("The independent review is required")?;
                ensure!(
                    review.unit_id.as_deref() == Some(unit_id)
                        && review.node_instance_id == call.stage_id
                        && matches!(&review.value, ArtifactValue::Review {run_id} if run_id == attempt),
                    "Acceptance must use the independent correctness review"
                );
            }
        }
        ensure!(
            visited.len() == decision.selected.len(),
            "A selected review is missing"
        );
        Ok(())
    }
}

fn includes_required_reviews(
    roles: &BTreeMap<String, bool>,
    selected: &BTreeMap<String, String>,
) -> bool {
    roles.get("correctness") == Some(&true)
        && roles
            .iter()
            .all(|(name, required)| !required || selected.contains_key(name))
        && selected.keys().all(|name| roles.contains_key(name))
}

fn approves_selected_tree(
    report: &serde_json::Map<String, Value>,
    candidate_id: &str,
    tree: &str,
    attempt: &str,
) -> bool {
    let matches = |name: &str, expected: &str| {
        report
            .get("provenance")
            .and_then(|value| value.get(name))
            .and_then(Value::as_str)
            == Some(expected)
    };
    report.get("verdict").and_then(Value::as_str) == Some("approved")
        && matches("candidateId", candidate_id)
        && matches("tree", tree)
        && matches("runId", attempt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adaptive_gate_requires_correctness_and_non_optional_roles() {
        let mut roles = BTreeMap::from([
            ("correctness".into(), true),
            ("architecture".into(), true),
            ("performance".into(), false),
        ]);
        let mut selected = BTreeMap::from([("architecture".into(), "Check boundaries.".into())]);
        assert!(!includes_required_reviews(&roles, &selected));
        selected.insert("correctness".into(), "Check invariants.".into());
        assert!(includes_required_reviews(&roles, &selected));
        roles.insert("correctness".into(), false);
        assert!(!includes_required_reviews(&roles, &selected));
    }

    #[test]
    fn adaptive_gate_rejects_approval_from_a_different_candidate_tree_or_attempt() {
        let report = json!({
            "verdict": "approved",
            "provenance": {"candidateId":"candidate", "tree":"tree", "runId":"attempt"}
        });
        let report = report.as_object().unwrap();
        assert!(approves_selected_tree(
            report,
            "candidate",
            "tree",
            "attempt"
        ));
        assert!(!approves_selected_tree(report, "other", "tree", "attempt"));
        assert!(!approves_selected_tree(
            report,
            "candidate",
            "old-tree",
            "attempt"
        ));
        assert!(!approves_selected_tree(
            report,
            "candidate",
            "tree",
            "older-attempt"
        ));
    }
}
