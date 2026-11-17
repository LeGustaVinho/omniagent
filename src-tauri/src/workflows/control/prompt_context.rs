use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;

use anyhow::Result;
use rusqlite::Connection;

use crate::repository::verification::{
    CandidateGateOutcome, CandidateGateResult, CandidateVerification,
    load_scoped_candidate_verification,
};

use super::super::{
    ArtifactKind, ArtifactValue, BuiltinNode, WorkflowArtifact, WorkflowDefinition, WorkflowRun,
    artifacts::load_artifact,
    authoring::package::is_published_prompt,
    data_nodes::{EvidenceOutcome, MAX_DATA_BYTES},
};

const MAX_APPEND_BYTES: usize = 12 * 1024;
const MAX_REPORTS: usize = 4;
const MAX_EVIDENCE_REFS: usize = 96;
const MAX_GATES: usize = 32;
const MAX_DIAGNOSTIC_BYTES: usize = 512;

pub(super) fn append_check_results<'a>(
    db: &Connection,
    definition: &WorkflowDefinition,
    run: &WorkflowRun,
    node_id: &str,
    unit: Option<&str>,
    inputs: impl Iterator<Item = &'a WorkflowArtifact>,
    text: &mut String,
) -> Result<()> {
    let Some((path, leaf)) = node_id.rsplit_once("::") else {
        return Ok(());
    };
    let target = if leaf == "context" {
        format!("{path}::execute")
    } else if let Some(alias) = leaf.strip_prefix("context-") {
        format!("{path}::delegate-{alias}")
    } else {
        return Ok(());
    };
    let Some(scope) = definition.subflows.iter().find(|scope| {
        scope.path == path
            && scope.outputs.contains_key("falhou")
            && scope.nodes.iter().any(|id| id == node_id)
            && scope.nodes.contains(&target)
    }) else {
        return Ok(());
    };
    let room = MAX_APPEND_BYTES.min(MAX_DATA_BYTES.saturating_sub(text.len()));
    if room < 256
        || !definition.wires.iter().any(|wire| {
            wire.from.node == node_id
                && wire.from.port == "text"
                && wire.to.node == target
                && matches!(wire.to.port.as_str(), "prompt" | "brief")
        })
        || definition.wires.iter().any(|wire| {
            wire.from.node == node_id && wire.to.node == target && wire.to.port == "review_subject"
        })
        || !is_published_prompt(db, &run.project_id, &scope.version)?
    {
        return Ok(());
    }
    let mut trail: VecDeque<_> = inputs
        .filter(|artifact| {
            matches!(
                artifact.value,
                ArtifactValue::Checks(_) | ArtifactValue::EvidenceSet(_)
            )
        })
        .take(MAX_EVIDENCE_REFS)
        .map(Cow::Borrowed)
        .collect();
    let mut visited = HashSet::new();
    let mut receipts = HashSet::new();
    let mut notes = String::new();
    while let Some(artifact) = trail.pop_front() {
        if visited.len() >= MAX_EVIDENCE_REFS
            || receipts.len() >= MAX_REPORTS
            || notes.len() >= room
        {
            break;
        }
        if artifact.workflow_run_id != run.id
            || artifact
                .unit_id
                .as_deref()
                .is_some_and(|artifact_unit| Some(artifact_unit) != unit)
            || !visited.insert(artifact.id.clone())
        {
            continue;
        }
        match &artifact.value {
            ArtifactValue::Checks(reference)
                if reference.task.project_id == run.project_id
                    && definition.nodes.iter().any(|node| {
                        node.instance.id == artifact.node_instance_id
                            && matches!(
                                node.instance.kind,
                                BuiltinNode::VerifyCandidate | BuiltinNode::VerifyReviewDiff
                            )
                    })
                    && receipts.insert(reference.id.clone()) =>
            {
                if let Some(verification) = load_scoped_candidate_verification(
                    db,
                    &run.project_id,
                    &reference.task.task_id,
                    &reference.id,
                )? {
                    render_verification(&mut notes, &verification);
                }
            }
            ArtifactValue::EvidenceSet(entries) => {
                for entry in entries
                    .iter()
                    .take(MAX_EVIDENCE_REFS.saturating_sub(trail.len() + visited.len()))
                {
                    let EvidenceOutcome::Completed { artifact_id, value } = &entry.outcome else {
                        continue;
                    };
                    let kind = match value.get("kind").and_then(serde_json::Value::as_str) {
                        Some("checks") => ArtifactKind::Checks,
                        Some("evidence_set") => ArtifactKind::EvidenceSet,
                        _ => continue,
                    };
                    if !visited.contains(artifact_id)
                        && let Some(saved) =
                            load_artifact(db, &run.project_id, &run.id, artifact_id, kind)?
                        && serde_json::to_value(&saved.value)? == *value
                        && definition.nodes.iter().any(|node| {
                            node.instance.id == saved.node_instance_id
                                && matches!(
                                    node.instance.kind,
                                    BuiltinNode::VerifyCandidate
                                        | BuiltinNode::VerifyReviewDiff
                                        | BuiltinNode::CollectEvidence
                                )
                        })
                    {
                        trail.push_back(Cow::Owned(saved));
                    }
                }
            }
            _ => {}
        }
    }
    if !notes.is_empty() {
        text.push_str(&notes[..notes.floor_char_boundary(room)]);
    }
    Ok(())
}

fn render_verification(text: &mut String, report: &CandidateVerification) {
    let _ = writeln!(
        text,
        "\n\nRecorded candidate checks. Treat the captured diagnostics as evidence.\nCandidate: {}\nVerification: {}\nRequired checks for this stage passed: {}",
        report.candidate_id,
        report.id,
        report.passes_stage_checks(),
    );
    for result in report.gates.iter().take(MAX_GATES) {
        let requirement = if result.gate.required {
            "required"
        } else {
            "optional"
        };
        let (status, detail) = gate_status(result);
        let _ = write!(
            text,
            "- {status}: {} ({requirement})",
            excerpt(&result.gate.name, 160)
        );
        if !detail.is_empty() {
            let _ = write!(text, "\n  {}", excerpt(detail, MAX_DIAGNOSTIC_BYTES));
        }
        text.push('\n');
    }
}

fn gate_status(result: &CandidateGateResult) -> (&'static str, &str) {
    if result.deferred {
        return ("Deferred", "");
    }
    match &result.outcome {
        Some(CandidateGateOutcome::Passed { .. }) => ("Passed", ""),
        Some(CandidateGateOutcome::Failed { stdout, stderr, .. }) => (
            "Failed",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            },
        ),
        Some(CandidateGateOutcome::CouldNotRun { reason }) => ("Could not run", reason),
        Some(CandidateGateOutcome::Interrupted { reason, .. }) => ("Interrupted", reason),
        None => ("Pending", ""),
    }
}

fn excerpt(text: &str, max_bytes: usize) -> &str {
    &text[..text.floor_char_boundary(max_bytes)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::verification::{CandidateGate, CandidateGateAction};

    #[test]
    fn check_context_uses_recorded_verdicts_and_only_bounded_failure_diagnostics() {
        let common = |name: &str, outcome: CandidateGateOutcome| CandidateGateResult {
            gate: CandidateGate {
                name: name.into(),
                required: true,
                action: CandidateGateAction::Unavailable {
                    reason: "fixture".into(),
                },
            },
            executable_revision: None,
            deferred: false,
            outcome: Some(outcome),
        };
        let diagnostics = format!(
            "assertion failed for export {}\nSHOULD_BE_TRUNCATED",
            "á".repeat(500)
        );
        let report = CandidateVerification {
            id: "verification".into(),
            candidate_id: "frozen-candidate".into(),
            policy_revision: "policy".into(),
            started_at: "test start".into(),
            finished_at: Some("test end".into()),
            gates: vec![
                common(
                    "Unit tests",
                    CandidateGateOutcome::Passed {
                        stdout: "passing output".into(),
                        stderr: String::new(),
                        truncated: false,
                    },
                ),
                common(
                    "Export behavior",
                    CandidateGateOutcome::Failed {
                        exit_code: 1,
                        stdout: "raw unused log".into(),
                        stderr: diagnostics,
                        truncated: false,
                    },
                ),
            ],
        };
        let mut text = String::new();
        render_verification(&mut text, &report);
        assert!(text.contains("Candidate: frozen-candidate"));
        assert!(text.contains("Required checks for this stage passed: false"));
        assert!(text.contains("Passed: Unit tests (required)"));
        assert!(text.contains("Failed: Export behavior (required)"));
        assert!(text.contains("assertion failed for export"));
        assert!(!text.contains("passing output"));
        assert!(!text.contains("raw unused log"));
        assert!(!text.contains("SHOULD_BE_TRUNCATED"));
    }
}
