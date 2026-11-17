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
    ApprovalDecision, ArtifactValue, Executor, NodeConfiguration, PinnedNode, PortType,
    TaskReference, WorkflowApprovalSubmission, WorkflowArtifact, WorkflowPlanUnit,
    approval::{issue_prepared_release, require_active_decision, require_current_plan},
    artifacts::insert_artifact,
    digest,
    registry::supports_pinned_execution,
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
    ReadGit(super::templates::diff::GitSelection),
    VerifyReviewDiff,
    PublishDiffReport,
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
        if matches!(request, WorkflowHostRequest::ApproveImplementation) {
            return self
                .approve_workflow_implementation(WorkflowApprovalSubmission {
                    project_id,
                    run_id,
                    node_id,
                    inputs,
                })
                .await;
        }
        let (node, inputs) = self
            .workflow_execution_inputs(&project_id, &run_id, &node_id, None, &inputs)
            .await?;
        let preparation_decision = if matches!(request, WorkflowHostRequest::PrepareWorktree { .. })
        {
            inputs.get("decision").map(|approval| approval.id.clone())
        } else {
            None
        };
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
                let decision = preparation_decision.clone();
                move |db| {
                    load_host_completion(db, &run, &node, &key, &revision, decision.as_deref())
                }
            })
            .await?;
        if let Some(saved) = saved {
            if matches!(saved.value, ArtifactValue::Task(_)) {
                self.refresh_control(project_id, run_id).await?;
            }
            return Ok(saved);
        }
        let action = match (node.definition.executor, request) {
            (Executor::BindInput, WorkflowHostRequest::Start { document }) => {
                HostAction::Publish("document", ArtifactValue::Markdown(document))
            }
            (Executor::ReadContext, WorkflowHostRequest::LoadContext) => {
                HostAction::Publish("context", inputs["document"].value.clone())
            }
            (Executor::ReadSelectedGit, WorkflowHostRequest::ReadGit(selection)) => {
                let ArtifactValue::Markdown(document) = &inputs["document"].value else {
                    anyhow::bail!("Read Git needs the selected document's registered checkout");
                };
                let inspection = super::templates::diff::capture(
                    self,
                    &project_id,
                    &document.checkout_id,
                    selection,
                )
                .await?;
                HostAction::Publish("git", ArtifactValue::GitInspection(inspection))
            }
            (Executor::CheckSelectedGit, WorkflowHostRequest::VerifyReviewDiff) => {
                let NodeConfiguration::RequiredChecks { names } = node.instance.configuration
                else {
                    anyhow::bail!("The diff checks configuration is unavailable");
                };
                let checks =
                    super::templates::diff::verify(self, &project_id, &inputs["git"], names)
                        .await?;
                HostAction::Publish("checks", ArtifactValue::Checks(checks))
            }
            (Executor::ReportSelectedGit, WorkflowHostRequest::PublishDiffReport) => {
                let report = super::templates::diff::report(
                    self,
                    &project_id,
                    &inputs["git"],
                    &inputs["review"],
                    &inputs["checks"],
                )
                .await?;
                HostAction::Publish("report", ArtifactValue::DiffReport(report))
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
                let units = self
                    .workflow_plan_units(
                        project_id.clone(),
                        run_id.clone(),
                        decision.plan_revision.clone(),
                    )
                    .await?;
                let worktree = repository::provision_task_worktree(
                    self,
                    default_root,
                    project_id.clone(),
                    task.task_id.clone(),
                    decision.source_checkout_id.clone(),
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
                if node.definition.executor == Executor::FindOrCreateWork {
                    super::units::validate_selected_plan_revision(&tx, &project_id, &run_id, &execution_key)?;
                }
                if let Some(saved) =
                    load_host_completion(&tx, &run_id, &node_id, &execution_key, &request_revision, preparation_decision.as_deref())?
                {
                    return Ok(saved);
                }
                let declared = inputs.iter().map(|(port, artifact)| (port.clone(), artifact.id.clone())).collect();
                super::control::require_stage(&tx, &project_id, &run_id, &node_id, None, &declared)?;
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
                    HostAction::Prepare {
                        decision,
                        artifact_id,
                        worktree,
                        units,
                    } => {
                        let artifact = issue_prepared_release(&tx, decision, artifact_id, node_id, worktree, units)?;
                        tx.execute(
                            "DELETE FROM workflow_host_completions WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND execution_key = ?3",
                            params![artifact.workflow_run_id, artifact.node_instance_id, execution_key],
                        )?;
                        artifact
                    }
                };
                record_host_completion(&tx, &artifact, &execution_key, &request_revision)?;
                super::inspection::record_workflow_completion_inputs(&tx, &artifact, &execution_key, &inputs)?;
                super::control::refresh(&tx, &project_id, &artifact.workflow_run_id)?;
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
            .workflow_definition(project.into(), workflow.version.clone())
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
        let control_checked = if super::control::uses_control(&definition) {
            self.refresh_control(project.into(), run.into()).await?;
            let (node, unit, supplied) = (
                node.to_owned(),
                unit_id.map(str::to_owned),
                artifacts.clone(),
            );
            let (definition, workflow) = (definition.clone(), workflow.clone());
            self.execute(move |db| {
                super::control::require_dependencies(
                    db,
                    &definition,
                    &workflow,
                    &node,
                    unit.as_deref(),
                    &supplied,
                )
            })
            .await?
        } else {
            false
        };
        let mut resolved = BTreeMap::new();
        for port in &pinned.definition.inputs {
            let source = definition
                .wires
                .iter()
                .find(|wire| wire.to.node == node && wire.to.port == port.name);
            let mcp_arguments = port.name == "arguments"
                && matches!(
                    &pinned.instance.configuration,
                    NodeConfiguration::McpTool(_)
                );
            if source.is_none() && mcp_arguments {
                continue;
            }
            let source = source.context("pinned workflow input has no source")?;
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
            let value = if mcp_arguments {
                producer
                    .definition
                    .outputs
                    .iter()
                    .find(|port| port.name == source.from.port)
                    .map(|port| port.value)
                    .filter(|value| matches!(value, PortType::Data(_)))
                    .context("MCP arguments must come from a typed workflow artifact")?
            } else {
                port.value
            };
            match value {
                PortType::Control => {
                    if control_checked {
                        continue;
                    }
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
                PortType::Evidence => {
                    anyhow::bail!("Conditions execute through the pinned control scheduler")
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
        params![
            artifact.workflow_run_id,
            artifact.node_instance_id,
            execution_key,
            request_revision,
            artifact.id
        ],
    )?;
    Ok(())
}

fn load_host_completion(
    db: &Connection,
    run: &str,
    node: &str,
    execution_key: &str,
    revision: &str,
    preparation_decision: Option<&str>,
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
    if saved != revision
        && let Some(decision) = preparation_decision
    {
        let was_prepared: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_release_bindings WHERE decision_artifact_id = ?1)",
            [decision], |row| row.get(0),
        )?;
        if !was_prepared {
            return Ok(None);
        }
    }
    ensure!(
        saved == revision,
        "this workflow node has already completed with a different request"
    );
    Ok(Some(serde_json::from_slice(&protection::unprotect(
        &payload,
    )?)?))
}
