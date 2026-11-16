use anyhow::{Context, Result, ensure};

use crate::{
    repository::{
        verified_git::DeliveryGitPolicy,
        verified_merge::{DeliveryMergeOutcome, DeliveryMergeRequest},
    },
    store::Store,
};

use super::{
    ArtifactKind, ArtifactValue, Executor, NodeInputs, TaskReference, WorkflowArtifact,
    WorkflowDefinition, WorkflowDeliveryUnit, WorkflowMergeReference, WorkflowUnitOutcome,
    WorkflowUnitRequest, builtin::BUILTIN_DELIVERY_ID,
};

impl Store {
    pub(super) async fn execute_workflow_merge_submission(
        &self,
        unit: &WorkflowDeliveryUnit,
        node: String,
        inputs: NodeInputs,
        request: WorkflowUnitRequest,
    ) -> Result<WorkflowUnitOutcome> {
        let WorkflowUnitRequest::VerifyMerge {
            expected_source_branch,
            expected_source_sha,
            destination_branch,
            expected_destination_sha,
            external_sha,
            drift_acceptance_id,
            prepare_drift,
            perform,
        } = request
        else {
            anyhow::bail!("the host request does not match the pinned merge executor");
        };
        let (project, run, unit_id) = (&unit.project_id, &unit.workflow_run_id, &unit.unit.id);
        let (node, artifact) = if let Some(pinned) = self
            .pinned_workflow_node(project.clone(), run.clone(), node.clone())
            .await?
        {
            ensure!(
                pinned.definition.executor == Executor::VerifyGitMerge,
                "the host request does not match the pinned merge executor"
            );
            let (_, mut resolved) = self
                .workflow_execution_inputs(project, run, &node, Some(unit_id), &inputs)
                .await?;
            let artifact = resolved
                .remove("git")
                .context("merge requires verified commit input")?;
            (Some(node), artifact)
        } else {
            ensure!(
                node == "merge" && legacy_builtin(&self.merge_workflow_definition(unit).await?),
                "merge node is outside this workflow"
            );
            ensure!(
                inputs.len() == 1,
                "legacy merge requires exactly one verified commit artifact"
            );
            let id = inputs
                .get("git")
                .context("merge requires verified commit input")?;
            let artifact = self
                .resolve_workflow_artifact(
                    project.clone(),
                    run.clone(),
                    id.clone(),
                    ArtifactKind::Git,
                )
                .await?
                .context("verified commit is outside the pinned legacy workflow")?;
            ensure!(
                artifact.node_instance_id == "commit"
                    && artifact.port == "git"
                    && artifact.unit_id.as_ref() == Some(unit_id),
                "merge requires this unit's pinned commit output"
            );
            (None, artifact)
        };
        let ArtifactValue::Git(commit) = artifact.value else {
            anyhow::bail!("merge verification requires the final verified commit");
        };
        ensure!(
            commit.task.project_id == unit.project_id
                && commit.task.task_id == unit.task_id
                && unit.commit_evidence_id.as_ref() == Some(&commit.id),
            "merge input does not match this unit's verified commit"
        );
        self.execute_workflow_merge(
            unit,
            node,
            DeliveryMergeRequest {
                project_id: project.clone(),
                task_id: unit.task_id.clone(),
                workflow_run_id: run.clone(),
                unit_id: unit_id.clone(),
                commit_evidence_id: commit.id,
                expected_source_branch,
                expected_source_sha,
                destination_branch,
                expected_destination_sha,
                external_sha,
                drift_acceptance_id,
                prepare_drift,
                perform,
            },
            false,
        )
        .await
    }

    async fn execute_workflow_merge(
        &self,
        unit: &WorkflowDeliveryUnit,
        node: Option<String>,
        request: DeliveryMergeRequest,
        automatic: bool,
    ) -> Result<WorkflowUnitOutcome> {
        let outcome = if automatic {
            self.verify_automatic_delivery_merge(request).await?
        } else {
            self.verify_delivery_merge(request).await?
        };
        Ok(match outcome {
            DeliveryMergeOutcome::WaitingForMerge => WorkflowUnitOutcome::WaitingForMerge,
            DeliveryMergeOutcome::DestinationMoved { actual_sha } => {
                WorkflowUnitOutcome::DestinationMoved { actual_sha }
            }
            DeliveryMergeOutcome::DestinationDrift { recovery } => {
                WorkflowUnitOutcome::DestinationDrift { recovery }
            }
            DeliveryMergeOutcome::DestinationCheckedOut { checkout } => {
                WorkflowUnitOutcome::DestinationCheckedOut { checkout }
            }
            DeliveryMergeOutcome::RecoveryRequired {
                checkout,
                recovery_directory,
                message,
            } => WorkflowUnitOutcome::RecoveryRequired {
                checkout,
                recovery_directory,
                message,
            },
            DeliveryMergeOutcome::Conflicted => WorkflowUnitOutcome::Conflicted,
            DeliveryMergeOutcome::Verified { evidence } => {
                let Some(node) = node else {
                    return Ok(WorkflowUnitOutcome::MergeVerified { evidence });
                };
                let artifact = self
                    .record_workflow_unit_artifact(
                        unit,
                        node,
                        "integration",
                        ArtifactValue::Merge(WorkflowMergeReference {
                            task: TaskReference {
                                project_id: unit.project_id.clone(),
                                task_id: unit.task_id.clone(),
                            },
                            id: evidence.id,
                            commit_evidence_id: evidence.commit_evidence_id,
                        }),
                    )
                    .await?;
                WorkflowUnitOutcome::Artifact { artifact }
            }
        })
    }

    pub(super) async fn pin_and_follow_up_workflow_merge(
        &self,
        unit: &WorkflowDeliveryUnit,
        artifact: &WorkflowArtifact,
    ) -> Result<Option<WorkflowUnitOutcome>> {
        let (project, task, run, unit_id) = (
            &unit.project_id,
            &unit.task_id,
            &unit.workflow_run_id,
            &unit.unit.id,
        );
        let ArtifactValue::Git(commit) = &artifact.value else {
            anyhow::bail!("automatic integration requires verified commit evidence");
        };
        let units = self
            .workflow_delivery_units(project.clone(), task.clone(), run.clone())
            .await?;
        if !units.last().is_some_and(|last| {
            last.unit.id == *unit_id && last.commit_evidence_id.as_ref() == Some(&commit.id)
        }) {
            return Ok(None);
        }
        ensure!(
            units.iter().all(|unit| unit.commit_evidence_id.is_some()),
            "integration requires a complete chain of verified commits"
        );

        // The destination must already be pinned when an operator receives the
        // final commit, even when they will merge it outside OmniAgent.
        let preview = self
            .delivery_merge_preview(
                project.clone(),
                task.clone(),
                run.clone(),
                unit_id.clone(),
                commit.id.clone(),
            )
            .await?;
        if self.delivery_git_policy(project.clone()).await?
            != DeliveryGitPolicy::AutomaticLocalCommitAndMerge
        {
            return Ok(None);
        }

        let definition = self.merge_workflow_definition(unit).await?;
        let mut successors = definition.wires.iter().filter(|wire| {
            wire.from.node == artifact.node_instance_id
                && wire.from.port == artifact.port
                && wire.to.port == "git"
                && definition.nodes.iter().any(|node| {
                    node.instance.id == wire.to.node
                        && node.definition.executor == Executor::VerifyGitMerge
                })
        });
        let node = if let Some(merge) = successors.next() {
            ensure!(
                successors.next().is_none(),
                "the final commit has multiple automatic merge successors"
            );
            let node = &merge.to.node;
            let inputs = NodeInputs::from([("git".into(), artifact.id.clone())]);
            self.workflow_execution_inputs(project, run, node, Some(unit_id), &inputs)
                .await?;
            Some(node.clone())
        } else if legacy_builtin(&definition)
            && artifact.node_instance_id == "commit"
            && artifact.port == "git"
        {
            None
        } else {
            return Ok(None);
        };

        let outcome = self
            .execute_workflow_merge(
                unit,
                node,
                DeliveryMergeRequest {
                    project_id: project.clone(),
                    task_id: task.clone(),
                    workflow_run_id: run.clone(),
                    unit_id: unit_id.clone(),
                    commit_evidence_id: commit.id.clone(),
                    expected_source_branch: preview.source_branch,
                    expected_source_sha: preview.source_sha,
                    destination_branch: preview.destination_branch,
                    expected_destination_sha: preview.expected_destination_sha,
                    external_sha: None,
                    drift_acceptance_id: None,
                    prepare_drift: false,
                    perform: true,
                },
                true,
            )
            .await?;
        Ok(Some(outcome))
    }

    async fn merge_workflow_definition(
        &self,
        unit: &WorkflowDeliveryUnit,
    ) -> Result<WorkflowDefinition> {
        let workflow = self
            .workflow_run(unit.project_id.clone(), unit.workflow_run_id.clone())
            .await?
            .context("delivery workflow is unavailable")?;
        self.workflow_definition(unit.project_id.clone(), workflow.version)
            .await?
            .context("delivery workflow definition is unavailable")
    }
}

fn legacy_builtin(definition: &WorkflowDefinition) -> bool {
    definition.version.id == BUILTIN_DELIVERY_ID
        && !definition
            .nodes
            .iter()
            .any(|node| node.definition.executor == Executor::VerifyGitMerge)
}
