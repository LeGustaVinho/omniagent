use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    jobs::PipelineRelease,
    protection,
    providers::{ProviderEvent, ProviderEventData},
    repository::{RepositoryMarkdownRevision, verification::SelectedCandidate},
    runs::RunStatus,
    store::Store,
};

use super::{
    ApprovalDecision, ArtifactKind, BuiltinNode, PortType, digest, runtime::load_pinned_node,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskReference {
    pub project_id: String,
    pub task_id: String,
}

pub type CandidateReference = SelectedCandidate;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedEvidence {
    pub task: TaskReference,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMergeReference {
    pub task: TaskReference,
    pub id: String,
    pub commit_evidence_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAcceptanceReference {
    pub evidence: ScopedEvidence,
    pub review_artifact_id: String,
    pub completion_criteria: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalReference {
    pub workflow_run_id: String,
    pub decision_artifact_id: String,
    pub release: PipelineRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "evidence", rename_all = "snake_case")]
pub enum ArtifactValue {
    Markdown(RepositoryMarkdownRevision),
    Plan {
        run_id: String,
        event_sequence: i64,
    },
    Review {
        run_id: String,
    },
    Task(TaskReference),
    Approval(ApprovalDecision),
    Release(ApprovalReference),
    Worktree {
        checkout_id: String,
        task: TaskReference,
    },
    Candidate(CandidateReference),
    Checks(ScopedEvidence),
    Acceptance(WorkflowAcceptanceReference),
    Git(ScopedEvidence),
    Merge(WorkflowMergeReference),
}

impl ArtifactValue {
    pub fn kind(&self) -> ArtifactKind {
        match self {
            Self::Markdown(_) => ArtifactKind::Markdown,
            Self::Plan { .. } => ArtifactKind::Plan,
            Self::Review { .. } => ArtifactKind::Review,
            Self::Task(_) => ArtifactKind::Task,
            Self::Approval(_) => ArtifactKind::Approval,
            Self::Release(_) => ArtifactKind::Release,
            Self::Worktree { .. } => ArtifactKind::Worktree,
            Self::Candidate(_) => ArtifactKind::Candidate,
            Self::Checks(_) => ArtifactKind::Checks,
            Self::Acceptance(_) => ArtifactKind::Acceptance,
            Self::Git(_) => ArtifactKind::Git,
            Self::Merge(_) => ArtifactKind::Merge,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArtifact {
    pub id: String,
    pub workflow_run_id: String,
    pub node_instance_id: String,
    pub port: String,
    pub unit_id: Option<String>,
    pub value: ArtifactValue,
}

impl Store {
    pub async fn workflow_document_revision(
        &self,
        project: String,
        checkout: String,
        revision: String,
    ) -> Result<RepositoryMarkdownRevision> {
        self.check_workflow_checkout(&project, &checkout).await?;
        let document = self
            .repository_markdown(checkout, revision)
            .await?
            .context("the saved Markdown revision is no longer available")?;
        Ok(document.revision)
    }

    pub async fn workflow_task_reference(
        &self,
        project: String,
        task: String,
    ) -> Result<TaskReference> {
        let task = self
            .project_task(project, task)
            .await?
            .context("task evidence is outside this project")?;
        Ok(TaskReference {
            project_id: task.project_id,
            task_id: task.id,
        })
    }

    pub async fn publish_workflow_candidate(
        &self,
        project: String,
        workflow: String,
        node: String,
        task: String,
        selection_id: String,
    ) -> Result<WorkflowArtifact> {
        let selection = self
            .selected_candidate(project.clone(), task, selection_id)
            .await?
            .context("candidate selection is outside this project and task")?;
        self.publish_workflow_artifact(
            project,
            workflow,
            node,
            "candidate".into(),
            ArtifactValue::Candidate(selection),
        )
        .await
    }

    pub async fn workflow_approval_reference(
        &self,
        project: String,
        run: String,
        approval: String,
    ) -> Result<ApprovalReference> {
        self.execute(move |db| {
            let (release, decision_artifact_id) = db.query_row(
                "SELECT r.approval_node_id, r.plan_revision, r.checkout_id, r.task_id, r.revoked, e.decision_artifact_id
                 FROM job_pipeline_releases r JOIN job_pipeline_runs w ON w.id = r.pipeline_run_id
                 JOIN workflow_release_bindings e ON e.release_id = r.id
                 WHERE r.id = ?1 AND r.project_id = ?2 AND r.pipeline_run_id = ?3
                    AND w.plan_revision = r.plan_revision",
                params![approval, project, run],
                |row| Ok((PipelineRelease {
                    id: approval.clone(), approval_node_id: row.get(0)?, plan_revision: row.get(1)?,
                    checkout_id: row.get(2)?, task_id: row.get(3)?, revoked: row.get(4)?,
                }, row.get(5)?)),
            ).optional()?.context("approval does not apply to the current workflow plan")?;
            ensure!(!release.revoked, "workflow approval has been revoked");
            Ok(ApprovalReference {
                workflow_run_id: run,
                decision_artifact_id,
                release,
            })
        })
        .await
    }

    pub async fn publish_workflow_artifact(
        &self,
        project: String,
        run: String,
        node: String,
        port: String,
        value: ArtifactValue,
    ) -> Result<WorkflowArtifact> {
        ensure!(
            matches!(
                value,
                ArtifactValue::Plan { .. }
                    | ArtifactValue::Review { .. }
                    | ArtifactValue::Candidate(_)
            ),
            "host artifacts must be issued by their executor"
        );
        self.check_artifact_source(&project, &run, &node, &value)
            .await?;
        let attempt_id = match &value {
            ArtifactValue::Plan { run_id, .. } | ArtifactValue::Review { run_id } => run_id,
            ArtifactValue::Candidate(reference) => &reference.target.run_id,
            _ => unreachable!(),
        };
        let unit = self
            .workflow_attempt(project.clone(), attempt_id.clone())
            .await?
            .and_then(|attempt| attempt.pin.unit_id);
        self.execute(move |db| insert_scoped_artifact(db, &project, run, node, port, unit, value))
            .await
    }

    pub async fn resolve_workflow_artifact(
        &self,
        project: String,
        run: String,
        id: String,
        kind: ArtifactKind,
    ) -> Result<Option<WorkflowArtifact>> {
        let scope = (project.clone(), run.clone());
        let saved = self
            .execute(move |db| load_artifact(db, &scope.0, &scope.1, &id, kind))
            .await?;
        if let Some(artifact) = &saved {
            self.check_artifact_source(&project, &run, &artifact.node_instance_id, &artifact.value)
                .await?;
        }
        Ok(saved)
    }

    pub(super) async fn check_artifact_source(
        &self,
        project: &str,
        run: &str,
        node: &str,
        value: &ArtifactValue,
    ) -> Result<()> {
        match value {
            ArtifactValue::Markdown(reference) => {
                self.check_workflow_checkout(project, &reference.checkout_id)
                    .await?;
                let source = self
                    .repository_markdown(reference.checkout_id.clone(), reference.id.clone())
                    .await?
                    .context("Markdown revision is unavailable")?;
                ensure!(
                    source.revision == *reference,
                    "the saved Markdown revision differs from the reference"
                );
            }
            ArtifactValue::Plan {
                run_id,
                event_sequence,
            } => {
                self.check_artifact_attempt(project, run, node, run_id, &[BuiltinNode::Plan])
                    .await?;
                self.workflow_plan_text(run_id.clone(), *event_sequence)
                    .await?;
            }
            ArtifactValue::Review { run_id } => {
                self.check_artifact_attempt(
                    project,
                    run,
                    node,
                    run_id,
                    &[BuiltinNode::ReviewPlan, BuiltinNode::ReviewCandidate],
                )
                .await?;
            }
            ArtifactValue::Task(reference)
            | ArtifactValue::Worktree {
                task: reference, ..
            } => {
                let issued = self
                    .workflow_task_reference(project.into(), reference.task_id.clone())
                    .await?;
                ensure!(
                    issued == *reference,
                    "task reference does not match the owning project"
                );
                if let ArtifactValue::Worktree { checkout_id, .. } = value {
                    let scope = (
                        project.to_owned(),
                        reference.task_id.clone(),
                        checkout_id.clone(),
                    );
                    let managed = self
                        .execute(move |db| {
                            crate::repository::writers::writer_owner(
                                db, &scope.0, &scope.1, &scope.2,
                            )
                        })
                        .await?;
                    ensure!(
                        managed.is_some(),
                        "checkout is not assigned to this work scope"
                    );
                }
            }
            ArtifactValue::Release(reference) => {
                let issued = self
                    .workflow_approval_reference(
                        project.into(),
                        run.into(),
                        reference.release.id.clone(),
                    )
                    .await?;
                ensure!(
                    issued == *reference,
                    "approval belongs to a different pinned scope"
                );
            }
            ArtifactValue::Approval(decision) => {
                let issued = self
                    .workflow_approval_decision(
                        project.into(),
                        run.into(),
                        node.into(),
                        decision.plan_revision.clone(),
                    )
                    .await?;
                ensure!(
                    issued.as_ref() == Some(decision),
                    "approval decision is unavailable in this workflow"
                );
            }
            ArtifactValue::Candidate(reference) => {
                self.check_artifact_attempt(
                    project,
                    run,
                    node,
                    &reference.target.run_id,
                    &[BuiltinNode::Implement],
                )
                .await?;
                let issued = self
                    .selected_candidate(
                        project.into(),
                        reference.target.task_id.clone(),
                        reference.id.clone(),
                    )
                    .await?;
                ensure!(
                    issued.as_ref() == Some(reference),
                    "candidate revision does not match its owning selection"
                );
                let attempt = self
                    .workflow_attempt(project.into(), reference.target.run_id.clone())
                    .await?
                    .context("candidate attempt disappeared")?;
                ensure!(
                    attempt.pin.unit_id.as_deref() == Some(reference.target.unit_id.as_str()),
                    "candidate belongs to a different logical unit"
                );
            }
            ArtifactValue::Checks(reference) => {
                ensure!(
                    reference.task.project_id == project
                        && self
                            .candidate_verification(
                                project.into(),
                                reference.task.task_id.clone(),
                                reference.id.clone()
                            )
                            .await?
                            .is_some(),
                    "checks are unavailable in the owning task"
                );
            }
            ArtifactValue::Acceptance(reference) => {
                let evidence = &reference.evidence;
                ensure!(
                    evidence.task.project_id == project
                        && self
                            .candidate_acceptance(
                                project.into(),
                                evidence.task.task_id.clone(),
                                evidence.id.clone()
                            )
                            .await?
                            .is_some(),
                    "acceptance is unavailable in the owning task"
                );
            }
            ArtifactValue::Git(reference) => {
                ensure!(
                    reference.task.project_id == project
                        && self
                            .verified_delivery_commit(
                                project.into(),
                                reference.task.task_id.clone(),
                                reference.id.clone()
                            )
                            .await?
                            .is_some(),
                    "Git evidence is unavailable in the owning task"
                );
            }
            ArtifactValue::Merge(reference) => {
                ensure!(
                    reference.task.project_id == project
                        && self
                            .verified_delivery_merge(
                                project.into(),
                                reference.task.task_id.clone(),
                                reference.commit_evidence_id.clone(),
                            )
                            .await?
                            .is_some_and(|receipt| receipt.id == reference.id
                                && receipt.workflow_run_id == run),
                    "integration evidence is unavailable in the owning task and workflow"
                );
            }
        }
        Ok(())
    }

    pub(super) async fn workflow_plan_text(&self, run_id: String, sequence: i64) -> Result<String> {
        self.execute(move |db| {
            let payload: Vec<u8> = db
                .query_row(
                    "SELECT payload FROM agent_events WHERE run_id = ?1 AND seq = ?2
                 AND source IN ('provider', 'application')",
                    params![run_id, sequence],
                    |row| row.get(0),
                )
                .optional()?
                .context("plan proposal event is unavailable on the issuing attempt")?;
            let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
            match event.data {
                ProviderEventData::PlanProposed { text, .. }
                    if event.location.parent_item_id.is_none() =>
                {
                    Ok(text)
                }
                _ => anyhow::bail!("the evidence is outside the owning attempt's plan proposals"),
            }
        })
        .await
    }

    async fn check_artifact_attempt(
        &self,
        project: &str,
        run: &str,
        node: &str,
        attempt: &str,
        kinds: &[BuiltinNode],
    ) -> Result<()> {
        let attempt = self
            .workflow_attempt(project.into(), attempt.into())
            .await?
            .context("artifact issuer is not a workflow attempt in this project")?;
        ensure!(
            self.workflow_attempt_input(attempt.run_id.clone())
                .await?
                .is_some(),
            "artifact issuer is not the primary workflow attempt"
        );
        ensure!(
            attempt.pin.workflow_run_id == run
                && attempt.pin.node_instance_id == node
                && attempt.status == RunStatus::Succeeded,
            "artifact issuer must be a successful attempt of the pinned workflow node"
        );
        let pinned = self
            .pinned_workflow_node(project.into(), run.into(), node.into())
            .await?
            .context("artifact producer node is unavailable")?;
        ensure!(
            kinds.contains(&pinned.definition.kind),
            "artifact producer has the wrong executor"
        );
        Ok(())
    }

    async fn check_workflow_checkout(&self, project: &str, checkout: &str) -> Result<()> {
        let scope = (project.to_owned(), checkout.to_owned());
        let belongs: bool = self
            .execute(move |db| {
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM checkouts WHERE project_id = ?1 AND id = ?2)",
                    params![scope.0, scope.1],
                    |row| row.get(0),
                )?)
            })
            .await?;
        ensure!(belongs, "artifact checkout is outside the workflow project");
        Ok(())
    }
}

pub(super) fn load_artifact(
    db: &Connection,
    project: &str,
    run: &str,
    id: &str,
    kind: ArtifactKind,
) -> Result<Option<WorkflowArtifact>> {
    let evidence: Option<Vec<u8>> = db.query_row(
        "SELECT a.payload FROM workflow_artifacts a JOIN workflow_run_pins w ON w.pipeline_run_id = a.pipeline_run_id
         WHERE a.id = ?1 AND a.kind = ?2 AND w.project_id = ?3 AND w.pipeline_run_id = ?4",
        params![id, kind.key(), project, run], |row| row.get(0),
    ).optional()?;
    let Some(evidence) = evidence else {
        return Ok(None);
    };
    let artifact: WorkflowArtifact = serde_json::from_slice(&protection::unprotect(&evidence)?)?;
    Ok((artifact.value.kind() == kind).then_some(artifact))
}

pub(super) fn insert_artifact(
    db: &Connection,
    project: &str,
    run: String,
    node: String,
    port: String,
    value: ArtifactValue,
) -> Result<WorkflowArtifact> {
    insert_scoped_artifact(db, project, run, node, port, None, value)
}

pub(super) fn insert_scoped_artifact(
    db: &Connection,
    project: &str,
    run: String,
    node: String,
    port: String,
    unit_id: Option<String>,
    value: ArtifactValue,
) -> Result<WorkflowArtifact> {
    let kind = value.kind();
    let pinned = load_pinned_node(db, project, &run, &node)?
        .context("artifact producer is outside this workflow")?;
    ensure!(
        pinned.definition.outputs.iter().any(|definition| {
            definition.name == port && definition.value == PortType::Data(kind)
        }),
        "artifact kind does not match the pinned output port"
    );
    ensure!(
        pinned.definition.kind.per_unit() == unit_id.is_some(),
        "artifact must use its pinned unit scope"
    );
    let bytes = serde_json::to_vec(&(&run, &node, &port, &unit_id, &value))?;
    let artifact = WorkflowArtifact {
        id: digest(&bytes),
        workflow_run_id: run,
        node_instance_id: node,
        port,
        unit_id,
        value,
    };
    db.execute(
        "INSERT INTO workflow_artifacts VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(id) DO NOTHING",
        params![
            artifact.id,
            artifact.workflow_run_id,
            artifact.node_instance_id,
            artifact.port,
            artifact.unit_id,
            kind.key(),
            protection::protect(&serde_json::to_vec(&artifact)?)?
        ],
    )?;
    Ok(artifact)
}
