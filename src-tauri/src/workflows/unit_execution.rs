use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    protection,
    repository::verified_git::{DeliveryCommitOutcome, DeliveryCommitRequest},
    store::Store,
};

use super::{
    ArtifactValue, Executor, NodeConfiguration, NodeInputs, ScopedEvidence, TaskReference,
    WorkflowAcceptanceReference, WorkflowArtifact, WorkflowDeliveryUnit,
    artifacts::insert_scoped_artifact, units::load_delivery_unit,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowUnitRequest {
    VerifyCandidate,
    AcceptCandidate { completion_criteria: Vec<String> },
    VerifyCommit { external_sha: Option<String> },
    Result,
}

pub struct WorkflowUnitSubmission {
    pub project_id: String,
    pub run_id: String,
    pub node_id: String,
    pub unit_id: String,
    pub task_id: String,
    pub inputs: NodeInputs,
    pub request: WorkflowUnitRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkflowUnitOutcome {
    WaitingForCommit,
    Artifact { artifact: WorkflowArtifact },
}

impl Store {
    pub async fn execute_workflow_unit(
        &self,
        submission: WorkflowUnitSubmission,
    ) -> Result<WorkflowUnitOutcome> {
        let WorkflowUnitSubmission {
            project_id,
            run_id,
            node_id,
            unit_id,
            task_id,
            inputs,
            request,
        } = submission;
        let (node, inputs) = self
            .workflow_execution_inputs(&project_id, &run_id, &node_id, Some(&unit_id), &inputs)
            .await?;
        let unit = self
            .execute({
                let (project, task, unit) = (project_id.clone(), task_id.clone(), unit_id.clone());
                move |db| load_delivery_unit(db, &project, &task, &unit)
            })
            .await?
            .context("the logical unit is outside the active delivery")?;
        ensure!(
            unit.workflow_run_id == run_id,
            "logical unit belongs to a different delivery"
        );
        let task = TaskReference {
            project_id: project_id.clone(),
            task_id: task_id.clone(),
        };
        let candidate = match inputs.get("candidate").map(|artifact| &artifact.value) {
            Some(ArtifactValue::Candidate(candidate)) => {
                ensure!(
                    candidate.target.project_id == project_id
                        && candidate.target.task_id == task_id
                        && candidate.target.unit_id == unit_id
                        && candidate.checkout_id == unit.checkout_id,
                    "candidate does not belong to this logical unit and checkout"
                );
                Some(candidate)
            }
            _ => None,
        };
        let (port, value) = match (node.definition.executor, request) {
            (Executor::CheckCandidate, WorkflowUnitRequest::VerifyCandidate) => {
                ensure!(
                    unit.commit_evidence_id.is_none(),
                    "logical unit already has a verified commit"
                );
                let candidate = candidate.context("verification requires this unit's candidate")?;
                let NodeConfiguration::RequiredChecks { names } = node.instance.configuration
                else {
                    anyhow::bail!("check executor has no pinned checks");
                };
                let checks = self
                    .verify_selected_candidate_with_requirements(
                        project_id.clone(),
                        task_id.clone(),
                        candidate.id.clone(),
                        names,
                    )
                    .await?;
                (
                    "checks",
                    ArtifactValue::Checks(ScopedEvidence {
                        task: task.clone(),
                        id: checks.id,
                    }),
                )
            }
            (
                Executor::RecordCandidateAcceptance,
                WorkflowUnitRequest::AcceptCandidate {
                    completion_criteria,
                },
            ) => {
                ensure!(
                    unit.commit_evidence_id.is_none(),
                    "logical unit already has a verified commit"
                );
                ensure!(
                    completion_criteria == unit.unit.completion_criteria,
                    "approval must attest every criterion of the pinned logical unit"
                );
                let candidate = candidate.context("acceptance requires this unit's candidate")?;
                let review = &inputs["review"];
                let ArtifactValue::Review { run_id: review_run } = &review.value else {
                    anyhow::bail!("acceptance requires a review attempt");
                };
                let reviewed = self
                    .workflow_attempt_input(review_run.clone())
                    .await?
                    .context("review attempt input is unavailable")?;
                ensure!(
                    reviewed.unit_id.as_deref() == Some(&unit_id)
                        && reviewed.artifacts.get("candidate") == Some(&inputs["candidate"].id),
                    "review assessed a different candidate artifact"
                );
                self.execute({
                    let (review_run, candidate) = (review_run.clone(), candidate.clone());
                    move |db| {
                        crate::repository::verification::require_candidate_review_dispatch(
                            db,
                            &review_run,
                            &candidate,
                        )
                    }
                })
                .await?;
                let ArtifactValue::Checks(checks) = &inputs["checks"].value else {
                    anyhow::bail!("acceptance requires verified checks");
                };
                ensure!(
                    checks.task == task,
                    "check evidence belongs to a different task"
                );
                let acceptance = self
                    .accept_candidate_verification(
                        project_id.clone(),
                        task_id.clone(),
                        candidate.id.clone(),
                        checks.id.clone(),
                    )
                    .await?;
                (
                    "acceptance",
                    ArtifactValue::Acceptance(WorkflowAcceptanceReference {
                        evidence: ScopedEvidence {
                            task: task.clone(),
                            id: acceptance.id,
                        },
                        review_artifact_id: review.id.clone(),
                        completion_criteria,
                    }),
                )
            }
            (Executor::VerifyGitCommit, WorkflowUnitRequest::VerifyCommit { external_sha }) => {
                let candidate = candidate
                    .context("commit verification requires this unit's exact candidate")?;
                let ArtifactValue::Acceptance(reference) = &inputs["acceptance"].value else {
                    anyhow::bail!("commit verification requires accepted evidence");
                };
                let acceptance = self
                    .candidate_acceptance(
                        project_id.clone(),
                        task_id.clone(),
                        reference.evidence.id.clone(),
                    )
                    .await?
                    .context("candidate acceptance is unavailable")?;
                ensure!(
                    acceptance.candidate == *candidate,
                    "commit verification requires acceptance of the selected candidate"
                );
                let commit = self
                    .verify_delivery_commit(DeliveryCommitRequest {
                        project_id: project_id.clone(),
                        task_id: task_id.clone(),
                        unit_id: unit_id.clone(),
                        purpose: unit.unit.purpose.clone(),
                        acceptance_id: acceptance.id,
                        acceptance_artifact_id: inputs["acceptance"].id.clone(),
                        external_sha,
                    })
                    .await?;
                match commit {
                    DeliveryCommitOutcome::WaitingForCommit => {
                        return Ok(WorkflowUnitOutcome::WaitingForCommit);
                    }
                    DeliveryCommitOutcome::Verified { evidence } => (
                        "git",
                        ArtifactValue::Git(ScopedEvidence {
                            task: task.clone(),
                            id: evidence.id,
                        }),
                    ),
                }
            }
            (Executor::PublishResult, WorkflowUnitRequest::Result) => {
                let ArtifactValue::Task(linked) = &inputs["task"].value else {
                    anyhow::bail!("result requires linked work");
                };
                let ArtifactValue::Git(commit) = &inputs["git"].value else {
                    anyhow::bail!("result requires verified Git evidence");
                };
                let units = self
                    .workflow_delivery_units(project_id.clone(), task_id.clone(), run_id.clone())
                    .await?;
                ensure!(
                    linked == &task
                        && commit.task == task
                        && units.iter().all(|unit| unit.commit_evidence_id.is_some())
                        && units.last().is_some_and(|last| last.unit.id == unit_id
                            && last.commit_evidence_id.as_ref() == Some(&commit.id)),
                    "the result requires a complete ordered delivery and its final commit"
                );
                ("delivery", ArtifactValue::Git(commit.clone()))
            }
            _ => anyhow::bail!("the host request does not match the pinned unit executor"),
        };
        let artifact = self
            .execute(move |db| {
                let active = load_delivery_unit(db, &project_id, &task_id, &unit_id)?
                    .context("delivery was superseded before recording evidence")?;
                ensure!(
                    active.plan_revision == unit.plan_revision
                        && active.checkout_id == unit.checkout_id,
                    "delivery scope changed before recording evidence"
                );
                insert_scoped_artifact(
                    db,
                    &project_id,
                    run_id,
                    node_id,
                    port.into(),
                    Some(unit_id),
                    value,
                )
            })
            .await?;
        Ok(WorkflowUnitOutcome::Artifact { artifact })
    }
}

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
    Ok(())
}
