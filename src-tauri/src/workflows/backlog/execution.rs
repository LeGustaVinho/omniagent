use std::{cmp::Reverse, collections::BTreeMap};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    diagnostics,
    repository::{
        IntegratedWorktreeBase,
        verified_merge::{VerifiedMerge, load_verified_delivery_merge},
    },
    store::Store,
    tasks::{
        TaskWorkflowState, current_work_state, delivery_merge_candidate,
        load_delivery_milestone_policy,
    },
};

use crate::workflows::{
    ArtifactKind, ArtifactValue, Endpoint, NodeConfiguration, NodeInputs, PinnedNode, PortType,
    WorkflowRevision, WorkflowRun,
    artifacts::{insert_artifact, load_artifact},
    control::{ControlStageState, recorded_control, refresh, require_stage},
    inspection::record_workflow_input_ids,
    integrated_checks::IntegratedRevision,
    launch::{WorkflowStartSchema, launch_backlog_child, load_workflow_launch},
    runtime::{load_definition, load_pinned_node, load_workflow_run},
};

const MAX_PARALLEL_TASKS: u16 = 32;
const STAGES_PER_POLL: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunBacklogConfiguration {
    pub child: WorkflowRevision,
    pub max_parallel: u16,
    pub failed: Endpoint,
}

impl RunBacklogConfiguration {
    pub(crate) fn valid(&self) -> bool {
        (1..=MAX_PARALLEL_TASKS).contains(&self.max_parallel)
            && [
                &self.child.id,
                &self.child.revision,
                &self.failed.node,
                &self.failed.port,
            ]
            .into_iter()
            .all(|value| !value.trim().is_empty())
    }
}

pub(crate) fn validate_child_binding(
    db: &Connection,
    project: &str,
    config: &RunBacklogConfiguration,
) -> Result<()> {
    let child = load_definition(db, project, &config.child)?
        .context("The pinned child workflow revision is unavailable")?;
    let start = WorkflowStartSchema::for_definition(&child)
        .context("The child workflow needs a typed Start with a backlog object input")?;
    start.validate_backlog_child()?;
    ensure!(
        child
            .nodes
            .iter()
            .any(|node| node.instance.kind == crate::workflows::BuiltinNode::ApproveImplementation),
        "The child delivery workflow needs an implementation approval"
    );
    ensure!(
        child.nodes.iter().any(|node| {
            node.instance.id == config.failed.node
                && node
                    .definition
                    .outputs
                    .iter()
                    .any(|port| port.name == config.failed.port && port.value == PortType::Control)
        }),
        "The pinned child workflow has no configured failure control output"
    );
    Ok(())
}

pub(crate) struct BacklogChildLaunch<'a> {
    pub project: &'a str,
    pub parent: &'a str,
    pub node: &'a str,
    pub work: &'a str,
    pub checkout: &'a str,
    pub workflow: &'a WorkflowRevision,
    pub inputs: Map<String, Value>,
    pub plan: super::BacklogDeliveryPlan,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklogExecution {
    node_id: String,
    backlog_artifact_id: String,
    state: String,
    reason: Option<String>,
    tasks: Vec<BacklogChild>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BacklogChild {
    key: String,
    task_id: String,
    workflow_run_id: String,
    #[serde(skip_serializing)]
    destination_branch: String,
}

pub(crate) enum ExecutionState {
    Waiting(Option<String>),
    Completed,
    Failed(String),
}

pub(crate) fn stage_state(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    backlog: &str,
) -> Result<ExecutionState> {
    if unit.is_some() {
        return Ok(ExecutionState::Failed(
            "Run backlog needs a project-scoped backlog".into(),
        ));
    }
    let enabled: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_automatic_runs WHERE pipeline_run_id = ?1)",
        [&run.id],
        |row| row.get(0),
    )?;
    if !enabled {
        return Ok(ExecutionState::Failed(
            "Enable automatic dispatch when launching a workflow that runs a backlog".into(),
        ));
    }
    let plan = run.plan_revision.as_deref().unwrap_or_default();
    db.execute(
        "INSERT INTO workflow_backlog_executions
         (pipeline_run_id, node_instance_id, backlog_artifact_id, plan_key, state)
         VALUES (?1, ?2, ?3, ?4, 'pending') ON CONFLICT DO NOTHING",
        params![run.id, node.instance.id, backlog, plan],
    )?;
    let (state, reason, pinned_plan): (String, Option<String>, String) = db.query_row(
        "SELECT state, reason, plan_key FROM workflow_backlog_executions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND backlog_artifact_id = ?3",
        params![run.id, node.instance.id, backlog],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if pinned_plan != plan {
        return Ok(ExecutionState::Failed(
            "The selected backlog belongs to an earlier delivery plan".into(),
        ));
    }
    Ok(match state.as_str() {
        "completed" => ExecutionState::Completed,
        "failed" => ExecutionState::Failed(reason.context("Backlog failure lost its diagnostic")?),
        _ => ExecutionState::Waiting(reason),
    })
}

struct PendingStage {
    project: String,
    run: String,
    node: String,
    artifact: String,
    plan: String,
    destination: Option<String>,
}

impl Store {
    pub(crate) async fn settle_workflow_backlogs(&self) -> Result<()> {
        self.execute(|db| {
            for _ in 0..STAGES_PER_POLL {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let Some(stage) = next_stage(&tx)? else {
                    break;
                };
                settle(&tx, &stage)?;
                tx.commit()?;
            }
            Ok(())
        })
        .await
    }
}

fn next_stage(db: &Connection) -> Result<Option<PendingStage>> {
    Ok(db.query_row(
        "SELECT pin.project_id, b.pipeline_run_id, b.node_instance_id, b.backlog_artifact_id, b.plan_key, b.destination_branch
         FROM workflow_backlog_executions b JOIN workflow_run_pins pin ON pin.pipeline_run_id = b.pipeline_run_id
         JOIN job_pipeline_runs run ON run.id = b.pipeline_run_id
         WHERE b.state = 'pending' AND b.next_check <= unixepoch() AND b.plan_key = coalesce(run.plan_revision, '')
           AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
         ORDER BY b.next_check, b.pipeline_run_id, b.node_instance_id LIMIT 1",
        [], |row| Ok(PendingStage {
            project: row.get(0)?, run: row.get(1)?, node: row.get(2)?,
            artifact: row.get(3)?, plan: row.get(4)?, destination: row.get(5)?,
        }),
    ).optional()?)
}

fn settle(db: &Connection, stage: &PendingStage) -> Result<()> {
    db.execute_batch("SAVEPOINT backlog_dispatch")?;
    let (state, reason) = match advance(db, stage) {
        Ok(outcome) => {
            db.execute_batch("RELEASE backlog_dispatch")?;
            match outcome {
                ExecutionState::Waiting(reason) => ("pending", reason),
                ExecutionState::Completed => ("completed", None),
                ExecutionState::Failed(reason) => ("failed", Some(reason)),
            }
        }
        Err(error) => {
            db.execute_batch("ROLLBACK TO backlog_dispatch; RELEASE backlog_dispatch")?;
            ("failed", Some(diagnostics::preview(&error.to_string())))
        }
    };
    db.execute(
        "UPDATE workflow_backlog_executions SET state = ?4, reason = ?5, next_check = unixepoch() + 1
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND backlog_artifact_id = ?3",
        params![stage.run, stage.node, stage.artifact, state, reason],
    )?;
    if state != "pending" {
        refresh(db, &stage.project, &stage.run)?;
    }
    Ok(())
}

fn advance(db: &Connection, stage: &PendingStage) -> Result<ExecutionState> {
    let run = load_workflow_run(db, &stage.project, &stage.run)?
        .context("The parent workflow is unavailable")?;
    ensure!(
        run.plan_revision.as_deref().unwrap_or_default() == stage.plan,
        "The backlog belongs to a superseded plan"
    );
    let inputs = NodeInputs::from([("backlog".into(), stage.artifact.clone())]);
    require_stage(db, &stage.project, &stage.run, &stage.node, None, &inputs)?;
    let node = load_pinned_node(db, &stage.project, &stage.run, &stage.node)?
        .context("The backlog node is unavailable")?;
    let NodeConfiguration::RunBacklog(config) = node.instance.configuration else {
        anyhow::bail!("The backlog node lost its pinned configuration");
    };
    validate_child_binding(db, &stage.project, &config)?;
    let data = load_artifact(
        db,
        &stage.project,
        &stage.run,
        &stage.artifact,
        ArtifactKind::Backlog,
    )?
    .context("The selected materialized backlog is unavailable")?;
    let ArtifactValue::Backlog(backlog) = data.value else {
        anyhow::bail!("Run backlog needs a materialization receipt");
    };
    let children = children(db, &stage.run)?;
    let mut verified = BTreeMap::new();
    for child in children.values() {
        if let Some(merge) = child_verified_merge(db, &stage.project, child)? {
            verified.insert(child.key.as_str(), merge);
        }
    }
    let mut active = 0;
    let mut failure = None;
    let mut ready = Vec::new();
    for item in &backlog.tasks {
        let draft = &item.specification;
        if let Some(child) = children.get(&draft.key) {
            if !verified.contains_key(draft.key.as_str()) {
                if child_failed(db, &stage.project, child, &config.failed)? {
                    failure.get_or_insert_with(|| {
                        format!("Backlog task {} reached its failure branch", draft.key)
                    });
                } else {
                    active += 1;
                }
            }
        } else if draft
            .depends_on
            .iter()
            .all(|key| verified.contains_key(key.as_str()))
        {
            ready.push(item);
        }
    }
    if let Some(reason) = failure {
        return Ok(if active == 0 {
            ExecutionState::Failed(reason)
        } else {
            ExecutionState::Waiting(Some(format!(
                "{reason}. Waiting for the other dispatched tasks to settle."
            )))
        });
    }
    if backlog
        .tasks
        .iter()
        .all(|item| verified.contains_key(item.specification.key.as_str()))
    {
        let rows: Vec<_> = backlog.tasks.iter().map(|item| {
            let key = &item.specification.key;
            let merge = &verified[key.as_str()];
            json!({
                "key": key, "task": item.task, "workflowRunId": merge.workflow_run_id,
                "mergeId": merge.id, "commitEvidenceId": merge.commit_evidence_id,
                "destinationBranch": merge.destination_branch, "integrationSha": merge.integration_sha
            })
        }).collect();
        let output = insert_artifact(
            db,
            &stage.project,
            stage.run.clone(),
            stage.node.clone(),
            "deliveries".into(),
            ArtifactValue::JsonObject(Map::from_iter([("tasks".into(), Value::Array(rows))])),
        )?;
        record_workflow_input_ids(db, &output, &stage.plan, &inputs)?;
        if node
            .definition
            .outputs
            .iter()
            .any(|port| port.name == "integrated")
        {
            let destination = stage
                .destination
                .as_deref()
                .context("The completed backlog has no pinned integration branch")?;
            ensure!(
                verified
                    .values()
                    .all(|merge| merge.destination_branch == destination),
                "The backlog deliveries were merged to different destinations"
            );
            let launch = load_workflow_launch(db, &stage.project, &stage.run)?
                .context("The backlog has no registered integration checkout")?;
            let verified_tip =
                crate::workflows::github::verified_destination(db, &stage.project, destination)?;
            let base = IntegratedWorktreeBase {
                branch: destination.into(),
                prerequisite_commits: backlog
                    .tasks
                    .iter()
                    .map(|item| {
                        verified[item.specification.key.as_str()]
                            .integration_sha
                            .clone()
                    })
                    .collect(),
                verified_tip,
            };
            let sha = base.registered_tip(db, &stage.project, &launch.checkout_id)?;
            let integration = insert_artifact(
                db,
                &stage.project,
                stage.run.clone(),
                stage.node.clone(),
                "integrated".into(),
                ArtifactValue::IntegratedRevision(IntegratedRevision {
                    destination_branch: base.branch,
                    sha,
                    integration_shas: base.prerequisite_commits,
                }),
            )?;
            record_workflow_input_ids(db, &integration, &stage.plan, &inputs)?;
        }
        return Ok(ExecutionState::Completed);
    }
    let queue_capacity: u32 = db.query_row(
        "SELECT capacity FROM job_queue_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let capacity = u32::from(config.max_parallel).min(queue_capacity) as usize;
    let capacity = capacity.saturating_sub(active);
    if capacity == 0 {
        return Ok(ExecutionState::Waiting(Some(
            "The parallel task capacity is in use".into(),
        )));
    }
    if ready.is_empty() {
        return Ok(ExecutionState::Waiting(Some(
            "Waiting for the outstanding tasks to reach verified merge".into(),
        )));
    }
    let parent = load_workflow_launch(db, &stage.project, &stage.run)?
        .context("The backlog has no registered launch checkout")?;
    let destination = match &stage.destination {
        Some(destination) => destination.clone(),
        None => {
            let Some(destination) =
                load_delivery_milestone_policy(db, &stage.project)?.destination_branch
            else {
                return Ok(ExecutionState::Waiting(Some("Configure the project's delivery destination branch before running the backlog".into())));
            };
            db.execute(
                "UPDATE workflow_backlog_executions SET destination_branch = ?4
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND backlog_artifact_id = ?3",
                params![stage.run, stage.node, stage.artifact, destination],
            )?;
            destination
        }
    };
    ensure!(
        verified
            .values()
            .all(|merge| merge.destination_branch == destination),
        "The backlog prerequisites were merged to a different destination than this execution"
    );
    ready.sort_by_key(|item| {
        (
            Reverse(item.specification.priority),
            &item.specification.key,
        )
    });
    let specification =
        super::SpecificationEvidence::load(db, &stage.project, &stage.run, &backlog.sources)?;
    for item in ready.into_iter().take(capacity) {
        let plan = super::BacklogDeliveryPlan {
            task: item.clone(),
            parent_run_id: stage.run.clone(),
            backlog_artifact_id: stage.artifact.clone(),
            source_artifacts: backlog.sources.clone(),
            checkout_id: parent.checkout_id.clone(),
            destination_branch: destination.clone(),
            prerequisite_integrations: item
                .specification
                .depends_on
                .iter()
                .map(|key| (key.clone(), verified[key.as_str()].integration_sha.clone()))
                .collect(),
            repair: None,
        };
        let parameters = Map::from_iter([(
            "backlog".into(),
            json!({
                "parentRunId": stage.run, "backlogArtifactId": stage.artifact,
                "sourceArtifacts": backlog.sources, "task": item.task, "specification": item.specification,
                "specificationEvidence": specification.select(&item.specification)?,
                "destinationBranch": destination,
                "prerequisiteIntegrations": plan.prerequisite_integrations,
            }),
        )]);
        let child = launch_backlog_child(
            db,
            BacklogChildLaunch {
                project: &stage.project,
                parent: &stage.run,
                node: &stage.node,
                work: &item.task.task_id,
                checkout: &parent.checkout_id,
                workflow: &config.child,
                inputs: parameters,
                plan,
            },
        )?;
        db.execute(
            "INSERT INTO workflow_backlog_task_runs VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                stage.run,
                stage.node,
                stage.artifact,
                item.specification.key,
                item.task.task_id,
                child.id,
                destination
            ],
        )?;
    }
    Ok(ExecutionState::Waiting(None))
}

fn children(db: &Connection, run: &str) -> Result<BTreeMap<String, BacklogChild>> {
    let mut statement = db.prepare(
        "SELECT task_key, task_id, child_run_id, destination_branch
         FROM workflow_backlog_task_runs WHERE pipeline_run_id = ?1",
    )?;
    statement
        .query_map([run], |row| {
            let key: String = row.get(0)?;
            Ok((
                key.clone(),
                BacklogChild {
                    key,
                    task_id: row.get(1)?,
                    workflow_run_id: row.get(2)?,
                    destination_branch: row.get(3)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()
        .map_err(Into::into)
}

fn child_verified_merge(
    db: &Connection,
    project: &str,
    child: &BacklogChild,
) -> Result<Option<VerifiedMerge>> {
    let Some(candidate) =
        delivery_merge_candidate(db, project, &child.task_id, &child.workflow_run_id)?
    else {
        return Ok(None);
    };
    let Some(merge) =
        load_verified_delivery_merge(db, project, &child.task_id, &candidate.commit_evidence_id)?
    else {
        return Ok(None);
    };
    ensure!(
        merge.workflow_run_id == child.workflow_run_id
            && merge.destination_branch == child.destination_branch,
        "The merge evidence is outside the pinned child delivery and destination"
    );
    Ok(
        (current_work_state(db, project, &child.task_id)? == TaskWorkflowState::Completed)
            .then_some(merge),
    )
}

fn child_failed(
    db: &Connection,
    project: &str,
    child: &BacklogChild,
    failure: &Endpoint,
) -> Result<bool> {
    let run = load_workflow_run(db, project, &child.workflow_run_id)?
        .context("The child workflow is unavailable")?;
    Ok(recorded_control(db, &run)?.iter().any(|receipt| {
        receipt.node_id == failure.node
            && receipt.plan_revision == run.plan_revision
            && receipt.state != ControlStageState::Skipped
            && receipt.selected_ports.contains(&failure.port)
    }))
}

pub(crate) fn inspect(db: &Connection, run: &WorkflowRun) -> Result<Vec<BacklogExecution>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, backlog_artifact_id, state, reason FROM workflow_backlog_executions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 ORDER BY node_instance_id",
    )?;
    let mut executions = query
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or_default()],
            |row| {
                Ok(BacklogExecution {
                    node_id: row.get(0)?,
                    backlog_artifact_id: row.get(1)?,
                    state: row.get(2)?,
                    reason: row.get(3)?,
                    tasks: Vec::new(),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut tasks = db.prepare(
        "SELECT task_key, task_id, child_run_id, destination_branch FROM workflow_backlog_task_runs
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND backlog_artifact_id = ?3 ORDER BY task_key",
    )?;
    for execution in &mut executions {
        execution.tasks = tasks
            .query_map(
                params![run.id, execution.node_id, execution.backlog_artifact_id],
                |row| {
                    Ok(BacklogChild {
                        key: row.get(0)?,
                        task_id: row.get(1)?,
                        workflow_run_id: row.get(2)?,
                        destination_branch: row.get(3)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(executions)
}
