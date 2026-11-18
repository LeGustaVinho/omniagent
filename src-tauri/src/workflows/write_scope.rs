use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    jobs::{PipelineDispatch, PipelineRelease, PipelineReleaseScope, issue_pipeline_release},
    repository::{provision_task_worktree, writers::writer_owner},
    store::Store,
    tasks::{TaskPlacement, insert_project_epic, insert_project_work, pipeline_work_origin},
};

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, Executor, NodeConfiguration, PinnedNode, PortType,
    Wire, WorkflowArtifact,
    artifacts::load_artifact,
    decision_nodes::{HumanInteraction, RequestInputKind},
    launch::load_workflow_launch,
    runtime::{load_definition, load_pinned_node, load_workflow_run},
};

pub(super) fn writer(node: &PinnedNode) -> bool {
    node.definition.kind == BuiltinNode::Agent
        && node.definition.executor == Executor::WriteAgent
        && node.definition.revision == 4
}

pub(super) fn authorizes_writes(node: &PinnedNode) -> bool {
    matches!(&node.instance.configuration, NodeConfiguration::Human(config)
        if config.interaction == HumanInteraction::Approval
            && config.fields.iter().any(|field| field.id == "authorizeWrites"
                && field.required && matches!(field.input, RequestInputKind::Boolean)))
}

pub(super) fn approval_node<'a>(
    nodes: &'a [PinnedNode],
    wires: &[Wire],
    writer: &str,
) -> Result<&'a str> {
    let by_id: HashMap<_, _> = nodes
        .iter()
        .map(|node| (node.instance.id.as_str(), node))
        .collect();
    let mut incoming = HashMap::<&str, Vec<(&PinnedNode, &str)>>::new();
    for wire in wires {
        let source = by_id
            .get(wire.from.node.as_str())
            .context("The writer's control source is missing")?;
        if source
            .definition
            .outputs
            .iter()
            .any(|port| port.name == wire.from.port && port.value == PortType::Control)
        {
            incoming
                .entry(&wire.to.node)
                .or_default()
                .push((source, &wire.from.port));
        }
    }
    let mut pending = vec![writer];
    let mut visited = HashSet::new();
    let mut approvals = HashSet::new();
    while let Some(target) = pending.pop() {
        if !visited.insert(target) {
            continue;
        }
        for &(source, port) in incoming.get(target).into_iter().flatten() {
            if authorizes_writes(source) && port == "approved" {
                approvals.insert(source.instance.id.as_str());
            } else {
                pending.push(&source.instance.id);
            }
        }
    }
    ensure!(
        approvals.len() == 1,
        "Connect one Human approval for repository writes before {writer}"
    );
    Ok(approvals.into_iter().next().expect("one approval"))
}

#[derive(Deserialize)]
struct ApprovedTask {
    key: String,
    title: String,
}

fn approved_tasks(
    db: &Connection,
    project: &str,
    run: &str,
    node: &str,
) -> Result<(String, Vec<ApprovedTask>)> {
    let action =
        load_pinned_node(db, project, run, node)?.context("The write approval is unavailable")?;
    ensure!(
        authorizes_writes(&action),
        "Select a Human approval for repository writes"
    );
    let revision: String = db
        .query_row(
            "SELECT d.artifact_id FROM workflow_stage_decisions d
         JOIN workflow_host_completions h ON h.pipeline_run_id = d.pipeline_run_id
           AND h.node_instance_id = d.node_instance_id AND h.execution_key = d.plan_key
           AND h.artifact_id = d.artifact_id AND h.request_revision = d.request_id
         WHERE d.pipeline_run_id = ?1 AND d.node_instance_id = ?2 AND d.plan_key = ''
           AND d.unit_key = '' AND d.resolution IS NOT NULL",
            params![run, node],
            |row| row.get(0),
        )
        .optional()?
        .context("Approve repository writes before running the AI Node")?;
    let artifact = load_artifact(db, project, run, &revision, ArtifactKind::JsonObject)?
        .context("The write approval receipt is unavailable")?;
    let ArtifactValue::JsonObject(evidence) = artifact.value else {
        anyhow::bail!("Invalid write approval receipt")
    };
    ensure!(
        evidence.get("approved") == Some(&Value::Bool(true))
            && evidence
                .get("values")
                .and_then(|values| values.get("authorizeWrites"))
                == Some(&Value::Bool(true)),
        "Repository writes were not authorized"
    );
    let tasks: Vec<ApprovedTask> = serde_json::from_value(
        evidence
            .get("original")
            .and_then(|value| value.get("tasks"))
            .cloned()
            .context("The approved input must include its backlog tasks")?,
    )?;
    let mut keys = HashSet::new();
    ensure!(
        !tasks.is_empty()
            && tasks.iter().all(|task| {
                !task.key.is_empty()
                    && task.key.len() <= 128
                    && !task.title.trim().is_empty()
                    && keys.insert(&task.key)
            }),
        "The approved backlog needs distinct task keys and titles"
    );
    Ok((revision, tasks))
}

fn task_key(input: Option<&WorkflowArtifact>) -> Result<&str> {
    match input.map(|artifact| &artifact.value) {
        Some(ArtifactValue::JsonObject(fields)) => fields
            .get("task_key")
            .or_else(|| fields.get("input").and_then(|input| input.get("task_key")))
            .and_then(Value::as_str)
            .context("A writing AI Node needs the approved task key on its input"),
        _ => anyhow::bail!("A writing AI Node needs its typed input"),
    }
}

fn approval_for(db: &Connection, project: &str, run: &str, node: &str) -> Result<String> {
    let run = load_workflow_run(db, project, run)?.context("The workflow run is unavailable")?;
    let definition = load_definition(db, project, &run.version)?
        .context("The workflow revision is unavailable")?;
    Ok(approval_node(&definition.nodes, &definition.wires, node)?.into())
}

pub(crate) fn release_is_human(
    db: &Connection,
    project: &str,
    run: &str,
    node: &str,
) -> Result<bool> {
    Ok(load_pinned_node(db, project, run, node)?
        .as_ref()
        .is_some_and(authorizes_writes))
}

pub(super) fn current_release(
    db: &Connection,
    project: &str,
    run: &str,
    approval: &str,
    revision: &str,
    task: &str,
) -> Result<bool> {
    let Ok((current, tasks)) = approved_tasks(db, project, run, approval) else {
        return Ok(false);
    };
    let key: Option<String> = db.query_row(
        "SELECT task_key FROM workflow_backlog_tasks WHERE pipeline_run_id = ?1 AND task_id = ?2",
        params![run, task], |row| row.get(0),
    ).optional()?;
    Ok(current == revision && tasks.iter().any(|task| Some(&task.key) == key.as_ref()))
}

pub(super) fn require_binding(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
    input: Option<&WorkflowArtifact>,
) -> Result<PipelineRelease> {
    let key = task_key(input)?;
    let run = &binding.pipeline_run_id;
    let approval = approval_for(db, project, run, &binding.node_instance_id)?;
    let release_id = binding
        .release_id
        .as_deref()
        .context("The AI Node has no approved worktree release")?;
    let release = db
        .query_row(
            "SELECT r.plan_revision, r.checkout_id, r.task_id, r.revoked
         FROM job_pipeline_releases r JOIN workflow_backlog_tasks t ON
            t.pipeline_run_id = r.pipeline_run_id AND t.task_id = r.task_id
         WHERE r.id = ?1 AND r.project_id = ?2 AND r.pipeline_run_id = ?3
           AND r.approval_node_id = ?4 AND t.task_key = ?5",
            params![release_id, project, run, approval, key],
            |row| {
                Ok(PipelineRelease {
                    id: release_id.into(),
                    approval_node_id: approval.clone(),
                    plan_revision: row.get(0)?,
                    checkout_id: row.get(1)?,
                    task_id: row.get(2)?,
                    revoked: row.get(3)?,
                })
            },
        )
        .optional()?
        .context("The release belongs to a different task or workflow")?;
    ensure!(
        !release.revoked
            && binding.plan_revision.is_none()
            && current_release(
                db,
                project,
                run,
                &approval,
                &release.plan_revision,
                &release.task_id
            )?
            && writer_owner(db, project, &release.task_id, &release.checkout_id)?.is_some(),
        "The task worktree release is stale or revoked"
    );
    Ok(release)
}

impl Store {
    pub(super) async fn prepare_human_write_scope(
        &self,
        project: &str,
        run: &str,
        node: &str,
        input: Option<&WorkflowArtifact>,
    ) -> Result<PipelineRelease> {
        let key = task_key(input)?.to_owned();
        let (project, run, node) = (project.to_owned(), run.to_owned(), node.to_owned());
        let (approval, revision, task, source, root) = self.execute({
            let (project, run) = (project.clone(), run.clone());
            move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let approval = approval_for(&tx, &project, &run, &node)?;
                let (revision, tasks) = approved_tasks(&tx, &project, &run, &approval)?;
                let task = tasks.into_iter().find(|task| task.key == key)
                    .context("The task is outside the approved backlog")?;
                let source = load_workflow_launch(&tx, &project, &run)?
                    .context("The workflow has no registered launch checkout")?.checkout_id;
                let database: String = tx.query_row("SELECT file FROM pragma_database_list WHERE name = 'main'", [], |row| row.get(0))?;
                let path = PathBuf::from(database);
                let root = path.parent().context("The workflow store has no workspace directory")?.join("worktrees");
                let task_id: Option<String> = tx.query_row(
                    "SELECT task_id FROM workflow_backlog_tasks WHERE pipeline_run_id = ?1 AND task_key = ?2",
                    params![run, key], |row| row.get(0),
                ).optional()?;
                let task_id = if let Some(id) = task_id {
                    id
                } else {
                    let epic: Option<String> = tx.query_row(
                        "SELECT epic_id FROM workflow_backlog_epics WHERE pipeline_run_id = ?1",
                        [&run], |row| row.get(0),
                    ).optional()?;
                    let epic = if let Some(epic) = epic { epic } else {
                        let epic = insert_project_epic(&tx, project.clone(), "Development Pipeline".into())?;
                        tx.execute("INSERT INTO workflow_backlog_epics VALUES (?1, ?2)", params![run, epic.id])?;
                        epic.id
                    };
                    let origin = pipeline_work_origin(&tx, &project, &run)?;
                    let created = insert_project_work(&tx, project, task.title,
                        TaskPlacement::Task { epic_id: Some(epic) }, &origin)?;
                    tx.execute("INSERT INTO workflow_backlog_tasks VALUES (?1, ?2, ?3)", params![run, key, created.id])?;
                    created.id
                };
                tx.commit()?;
                Ok((approval, revision, task_id, source, root))
            }
        }).await?;
        let worktree =
            provision_task_worktree(self, root, project.clone(), task.clone(), source).await?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(current_release(&tx, &project, &run, &approval, &revision, &task)?,
                "The write approval changed while preparing the task worktree");
            let existing = tx.query_row(
                "SELECT id, revoked FROM job_pipeline_releases WHERE project_id = ?1 AND pipeline_run_id = ?2
                 AND approval_node_id = ?3 AND plan_revision = ?4 AND checkout_id = ?5 AND task_id = ?6
                 ORDER BY revoked DESC, id LIMIT 1",
                params![project, run, approval, revision, worktree.checkout_id, task],
                |row| Ok(PipelineRelease {
                    id: row.get(0)?, revoked: row.get(1)?, approval_node_id: approval.clone(),
                    plan_revision: revision.clone(), checkout_id: worktree.checkout_id.clone(), task_id: task.clone(),
                }),
            ).optional()?;
            if let Some(existing) = existing {
                ensure!(!existing.revoked, "The task worktree release was revoked");
                return Ok(existing);
            }
            let result = issue_pipeline_release(&tx, PipelineReleaseScope {
                project: &project, pipeline_run: &run, approval_node: &approval,
                plan_revision: &revision, checkout: &worktree.checkout_id, task: &task,
            })?;
            tx.commit()?;
            Ok(result)
        }).await
    }
}
