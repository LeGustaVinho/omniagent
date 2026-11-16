use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    protection,
    repository::{self, ManagedWorktree, RepositoryMarkdownRevision},
    store::Store,
    tasks::{TaskPlacement, insert_project_work, pipeline_work_origin},
};

use super::{
    ApprovalDecision, ApprovalDecisionSource, ArtifactValue, Executor, PinnedNode, PortType,
    TaskReference, WorkflowArtifact, WorkflowPlanUnit,
    approval::{
        issue_prepared_release, record_decision, require_active_decision, require_current_plan,
    },
    artifacts::insert_artifact,
    digest, registry::supports_pinned_execution,
};

pub type NodeInputs = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowHostRequest {
    Start {
        document: RepositoryMarkdownRevision,
    },
    LoadContext,
    LinkExistingTask {
        task_id: String,
    },
    CreateTask {
        title: String,
    },
    ApproveImplementation,
    PrepareWorktree {
        default_root: PathBuf,
    },
}

pub struct WorkflowHostSubmission {
    pub project_id: String,
    pub run_id: String,
    pub node_id: String,
    pub inputs: NodeInputs,
    pub request: WorkflowHostRequest,
}

enum HostAction {
    Publish(&'static str, ArtifactValue),
    CreateTask(String),
    Approve(ApprovalDecision),
    Prepare {
        decision: ApprovalDecision,
        artifact_id: String,
        worktree: ManagedWorktree,
        units: Vec<WorkflowPlanUnit>,
    },
}

impl Store {
    pub async fn execute_workflow_host(
        &self,
        submission: WorkflowHostSubmission,
    ) -> Result<WorkflowArtifact> {
        let WorkflowHostSubmission {
            project_id,
            run_id,
            node_id,
            inputs,
            request,
        } = submission;
        let (node, inputs) = self
            .workflow_execution_inputs(&project_id, &run_id, &node_id, None, &inputs)
            .await?;
        let request_revision = host_request_revision(&inputs, &request)?;
        let execution_key = inputs
            .get("plan")
            .map(|plan| plan.id.clone())
            .or_else(
                || match inputs.get("decision").map(|artifact| &artifact.value) {
                    Some(ArtifactValue::Approval(decision)) => Some(decision.plan_revision.clone()),
                    _ => None,
                },
            )
            .unwrap_or_default();
        let saved = self
            .execute({
                let (run, node, key, revision) = (
                    run_id.clone(),
                    node_id.clone(),
                    execution_key.clone(),
                    request_revision.clone(),
                );
                move |db| load_host_completion(db, &run, &node, &key, &revision)
            })
            .await?;
        if let Some(saved) = saved {
            return Ok(saved);
        }
        let action = match (node.definition.executor, request) {
            (Executor::BindInput, WorkflowHostRequest::Start { document }) => {
                HostAction::Publish("document", ArtifactValue::Markdown(document))
            }
            (Executor::ReadContext, WorkflowHostRequest::LoadContext) => {
                HostAction::Publish("context", inputs["document"].value.clone())
            }
            (
                Executor::FindOrCreateWork,
                work @ (WorkflowHostRequest::LinkExistingTask { .. }
                | WorkflowHostRequest::CreateTask { .. }),
            ) => {
                let ArtifactValue::Review { run_id: review_run } = &inputs["review"].value else {
                    anyhow::bail!("work linking requires a plan review");
                };
                let reviewed = self
                    .workflow_attempt(project_id.clone(), review_run.clone())
                    .await?
                    .context("plan review attempt is unavailable")?;
                ensure!(
                    reviewed.pin.plan_revision.as_ref() == Some(&inputs["plan"].id),
                    "work linking requires a review of the selected plan revision"
                );
                match work {
                    WorkflowHostRequest::LinkExistingTask { task_id } => HostAction::Publish(
                        "task",
                        ArtifactValue::Task(
                            self.workflow_task_reference(project_id.clone(), task_id)
                                .await?,
                        ),
                    ),
                    WorkflowHostRequest::CreateTask { title } => HostAction::CreateTask(title),
                    _ => unreachable!(),
                }
            }
            (Executor::RecordImplementationRelease, WorkflowHostRequest::ApproveImplementation) => {
                let ArtifactValue::Task(task) = &inputs["task"].value else {
                    anyhow::bail!("approval requires linked work");
                };
                let plan = &inputs["plan"];
                let ArtifactValue::Plan {
                    run_id: attempt, ..
                } = &plan.value
                else {
                    anyhow::bail!("approval requires a plan");
                };
                self.workflow_plan_units(project_id.clone(), run_id.clone(), plan.id.clone())
                    .await?;
                HostAction::Approve(ApprovalDecision {
                    workflow_run_id: run_id.clone(),
                    node_instance_id: node_id.clone(),
                    plan_revision: plan.id.clone(),
                    task: task.clone(),
                    task_artifact_id: inputs["task"].id.clone(),
                    source_checkout_id: self.workflow_plan_checkout(&project_id, attempt).await?,
                    source: ApprovalDecisionSource::Operator,
                })
            }
            (
                Executor::ProvisionWorktree,
                WorkflowHostRequest::PrepareWorktree { default_root },
            ) => {
                let ArtifactValue::Task(task) = &inputs["task"].value else {
                    anyhow::bail!("preparation requires linked work");
                };
                let approval = &inputs["decision"];
                let ArtifactValue::Approval(decision) = &approval.value else {
                    anyhow::bail!("preparation requires an approval decision");
                };
                ensure!(
                    decision.task == *task && decision.task_artifact_id == inputs["task"].id,
                    "preparation must use the work from the approval decision"
                );
                let worktree = repository::provision_task_worktree(
                    self,
                    default_root,
                    project_id.clone(),
                    task.task_id.clone(),
                    decision.source_checkout_id.clone(),
                )
                .await?;
                let units = self
                    .workflow_plan_units(
                        project_id.clone(),
                        run_id.clone(),
                        decision.plan_revision.clone(),
                    )
                    .await?;
                HostAction::Prepare {
                    decision: decision.clone(),
                    artifact_id: approval.id.clone(),
                    worktree,
                    units,
                }
            }
            _ => anyhow::bail!("the host request does not match the pinned executor"),
        };
        if let HostAction::Publish(_, value) = &action {
            self.check_artifact_source(&project_id, &run_id, &node_id, value)
                .await?;
        }
        let completed = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if !execution_key.is_empty() {
                    require_current_plan(&tx, &project_id, &run_id, &execution_key)?;
                }
                if let Some(saved) =
                    load_host_completion(&tx, &run_id, &node_id, &execution_key, &request_revision)?
                {
                    return Ok(saved);
                }
                let artifact = match action {
                    HostAction::Publish(port, value) => {
                        if let ArtifactValue::Task(task) = &value {
                            super::launch::require_launch_work(&tx, &project_id, &run_id, Some(&task.task_id))?;
                        }
                        insert_artifact(&tx, &project_id, run_id, node_id, port.into(), value)?
                    }
                    HostAction::CreateTask(title) => {
                        super::launch::require_launch_work(&tx, &project_id, &run_id, None)?;
                        let origin = pipeline_work_origin(&tx, &project_id, &run_id)?;
                        let task = insert_project_work(&tx, project_id.clone(), title, TaskPlacement::Task { epic_id: None }, &origin)?;
                        insert_artifact(
                            &tx,
                            &project_id,
                            run_id,
                            node_id,
                            "task".into(),
                            ArtifactValue::Task(TaskReference {
                                project_id: task.project_id,
                                task_id: task.id,
                            }),
                        )?
                    }
                    HostAction::Approve(decision) => record_decision(&tx, decision)?,
                    HostAction::Prepare {
                        decision,
                        artifact_id,
                        worktree,
                        units,
                    } => issue_prepared_release(
                        &tx,
                        decision,
                        artifact_id,
                        node_id,
                        worktree,
                        units,
                    )?,
                };
                record_host_completion(&tx, &artifact, &execution_key, &request_revision)?;
                tx.commit()?;
                Ok(artifact)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(completed)
    }

    pub(super) async fn workflow_execution_inputs(
        &self,
        project: &str,
        run: &str,
        node: &str,
        unit_id: Option<&str>,
        artifacts: &NodeInputs,
    ) -> Result<(PinnedNode, BTreeMap<String, WorkflowArtifact>)> {
        let workflow = self
            .workflow_run(project.into(), run.into())
            .await?
            .context("workflow run is outside this project")?;
        let definition = self
            .workflow_definition(project.into(), workflow.version)
            .await?
            .context("workflow definition is unavailable")?;
        let pinned = self
            .pinned_workflow_node(project.into(), run.into(), node.into())
            .await?
            .context("workflow node is outside this run")?;
        ensure!(
            supports_pinned_execution(&pinned.definition),
            "the pinned executor definition is unavailable"
        );
        ensure!(
            pinned.definition.kind.per_unit() == unit_id.is_some(),
            "workflow node has the wrong logical unit scope"
        );
        let mut resolved = BTreeMap::new();
        for port in &pinned.definition.inputs {
            let source = definition
                .wires
                .iter()
                .find(|wire| wire.to.node == node && wire.to.port == port.name)
                .context("pinned workflow input has no source")?;
            let producer = definition
                .nodes
                .iter()
                .find(|pinned| pinned.instance.id == source.from.node)
                .context("pinned workflow input producer is unavailable")?;
            let producer_unit = if producer.definition.kind.per_unit() {
                unit_id
            } else {
                None
            };
            match port.value {
                PortType::Control => {
                    let (run, node, unit) = (
                        run.to_owned(),
                        source.from.node.clone(),
                        producer_unit.map(str::to_owned),
                    );
                    let plan_revision = workflow.plan_revision.clone();
                    let complete: bool = self.execute(move |db| Ok(db.query_row(
                        "SELECT EXISTS (
                             SELECT 1 FROM workflow_artifacts a
                             WHERE a.pipeline_run_id = ?1 AND a.node_instance_id = ?2 AND a.unit_id IS ?3
                             UNION ALL
                             SELECT 1 FROM workflow_command_results c
                             JOIN job_attempts a ON a.run_id = c.run_id
                             JOIN job_pipeline_origins p ON p.job_id = a.job_id
                             WHERE c.pipeline_run_id = ?1 AND c.node_instance_id = ?2 AND ?3 IS NULL
                               AND p.plan_revision IS ?4
                         )", params![run, node, unit, plan_revision], |row| row.get(0),
                    )?)).await?;
                    ensure!(
                        complete,
                        "control input {} is waiting for its source",
                        port.name
                    );
                }
                PortType::Data(kind) => {
                    let id = artifacts.get(&port.name).with_context(|| {
                        format!("workflow input {} needs an artifact", port.name)
                    })?;
                    let artifact = self
                        .resolve_workflow_artifact(project.into(), run.into(), id.clone(), kind)
                        .await?
                        .with_context(|| {
                            format!("artifact for workflow input {} is unavailable", port.name)
                        })?;
                    ensure!(
                        artifact.node_instance_id == source.from.node
                            && artifact.port == source.from.port
                            && artifact.unit_id.as_deref() == producer_unit,
                        "artifact for workflow input {} is outside its pinned wire",
                        port.name
                    );
                    resolved.insert(port.name.clone(), artifact);
                }
            }
        }
        ensure!(
            resolved.len() == artifacts.len(),
            "workflow submission contains an unknown data input"
        );
        if let Some(plan) = resolved.get("plan") {
            ensure!(
                workflow.plan_revision.as_ref() == Some(&plan.id),
                "the pinned wire must receive the currently selected plan"
            );
        }
        if let Some(approval) = resolved.get("decision")
            && let ArtifactValue::Approval(decision) = &approval.value
        {
            let (decision, id) = (decision.clone(), approval.id.clone());
            self.execute(move |db| require_active_decision(db, &decision, &id))
                .await?;
        }
        Ok((pinned, resolved))
    }
}

pub(super) fn host_request_revision(
    inputs: &BTreeMap<String, WorkflowArtifact>,
    request: &WorkflowHostRequest,
) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(inputs, request))?))
}

pub(super) fn record_host_completion(
    db: &Connection,
    artifact: &WorkflowArtifact,
    execution_key: &str,
    request_revision: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO workflow_host_completions VALUES (?1, ?2, ?3, ?4, ?5)",
        params![artifact.workflow_run_id, artifact.node_instance_id, execution_key, request_revision, artifact.id],
    )?;
    Ok(())
}

fn load_host_completion(
    db: &Connection,
    run: &str,
    node: &str,
    execution_key: &str,
    revision: &str,
) -> Result<Option<WorkflowArtifact>> {
    let saved: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT h.request_revision, a.payload FROM workflow_host_completions h
         JOIN workflow_artifacts a ON a.id = h.artifact_id
         WHERE h.pipeline_run_id = ?1 AND h.node_instance_id = ?2 AND h.execution_key = ?3",
            params![run, node, execution_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((saved, payload)) = saved else {
        return Ok(None);
    };
    ensure!(
        saved == revision,
        "this workflow node has already completed with a different request"
    );
    Ok(Some(serde_json::from_slice(&protection::unprotect(
        &payload,
    )?)?))
}
