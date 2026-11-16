use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    protection,
    repository::{RepositoryMarkdownRevision, workflow_markdown_revision},
    store::Store,
    tasks::{AgileEventData, ChecklistProgressReceipt},
};

use super::{
    ArtifactValue, BuiltinNode, DeliveryRoles, WorkflowHostRequest, WorkflowRevision, WorkflowRun,
    artifacts::insert_artifact,
    builtin_delivery_graph, digest,
    execution::{host_request_revision, record_host_completion},
    registry::supports_pinned_execution,
    runtime::{insert_workflow_run, load_definition, load_workflow_run},
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowLaunchSource {
    pub checkout_id: String,
    pub revision_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowLaunchOrigin {
    Editor {
        request_id: String,
    },
    Agile {
        request_id: String,
        work_id: String,
    },
    Rule {
        rule_id: String,
        event_id: String,
        work_id: String,
    },
}

impl WorkflowLaunchOrigin {
    fn work_id(&self) -> Option<&str> {
        match self {
            Self::Editor { .. } => None,
            Self::Agile { work_id, .. } | Self::Rule { work_id, .. } => Some(work_id),
        }
    }

    fn key(&self) -> Result<String> {
        let key = match self {
            Self::Editor { request_id } | Self::Agile { request_id, .. } => {
                ("manual", request_id.as_str(), "")
            }
            Self::Rule {
                rule_id, event_id, ..
            } => ("rule", rule_id.as_str(), event_id.as_str()),
        };
        ensure!(
            !key.1.trim().is_empty() && key.1.len() <= 256,
            "The workflow launch identity is invalid"
        );
        ensure!(
            key.0 != "rule" || !key.2.trim().is_empty(),
            "The Agile launch did not identify an event"
        );
        Ok(digest(&serde_json::to_vec(&key)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualWorkflowLaunch {
    pub project_id: String,
    pub workflow: WorkflowRevision,
    pub source: WorkflowLaunchSource,
    pub request_id: String,
    pub work_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLaunchReceipt {
    pub workflow_run_id: String,
    pub version: WorkflowRevision,
    pub source: RepositoryMarkdownRevision,
    pub origin: WorkflowLaunchOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checklist_item: Option<ChecklistProgressReceipt>,
}

impl Store {
    pub async fn launch_workflow_manual(
        &self,
        request: ManualWorkflowLaunch,
    ) -> Result<WorkflowRun> {
        let (request_id, work_id) = (request.request_id, request.work_id);
        let origin = match work_id {
            Some(work_id) => WorkflowLaunchOrigin::Agile {
                request_id,
                work_id,
            },
            None => WorkflowLaunchOrigin::Editor { request_id },
        };
        let run = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let run = launch_workflow_in_tx(
                    &tx,
                    &request.project_id,
                    &request.workflow,
                    &request.source,
                    &origin,
                )?;
                tx.commit()?;
                Ok(run)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(run)
    }

    pub async fn launch_builtin_repository_delivery(
        &self,
        project: String,
        source: WorkflowLaunchSource,
    ) -> Result<WorkflowRun> {
        let revision = self
            .save_workflow_revision(
                project.clone(),
                super::builtin::BUILTIN_DELIVERY_ID.into(),
                builtin_delivery_graph(DeliveryRoles::default()),
            )
            .await?;
        self.launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project,
            workflow: revision,
            source,
            request_id: Uuid::new_v4().to_string(),
            work_id: None,
        })
        .await
    }

    pub async fn workflow_launch(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<WorkflowLaunchReceipt>> {
        self.execute(move |db| load_workflow_launch(db, &project, &run))
            .await
    }
}

pub(crate) fn validate_workflow_launch_source(
    db: &Connection,
    project: &str,
    source: &WorkflowLaunchSource,
) -> Result<RepositoryMarkdownRevision> {
    workflow_markdown_revision(db, project, &source.checkout_id, &source.revision_id)?
        .context("The selected Markdown revision is unavailable in this project checkout")
}

/// The caller owns the transaction and can bind causal Agile event lineage
/// alongside the run, graph pins, and its initial host outputs.
pub(crate) fn launch_workflow_in_tx(
    db: &Connection,
    project: &str,
    revision: &WorkflowRevision,
    source: &WorkflowLaunchSource,
    origin: &WorkflowLaunchOrigin,
) -> Result<WorkflowRun> {
    let key = origin.key()?;
    let saved: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM workflow_launches WHERE project_id = ?1 AND launch_key = ?2",
            params![project, key],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(bytes) = saved {
        let receipt = open_receipt(&bytes)?;
        ensure!(
            receipt.version == *revision
                && receipt.source.checkout_id == source.checkout_id
                && receipt.source.id == source.revision_id
                && receipt.origin == *origin,
            "The launch identity was already used with different inputs"
        );
        return load_workflow_run(db, project, &receipt.workflow_run_id)?
            .context("The saved workflow launch lost its run");
    }
    let document = validate_workflow_launch_source(db, project, source)?;
    if let Some(work) = origin.work_id() {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_tasks WHERE project_id = ?1 AND id = ?2)",
            params![project, work],
            |row| row.get(0),
        )?;
        ensure!(exists, "The launch work item is outside this project");
    }
    let checklist_item = origin_checklist_item(db, project, origin)?;
    let workflow = load_definition(db, project, revision)?
        .context("The published workflow revision is unavailable in this project")?;
    ensure!(
        workflow
            .nodes
            .iter()
            .all(|node| supports_pinned_execution(&node.definition)),
        "A pinned node executor is unavailable on this installation"
    );
    let run = insert_workflow_run(db, project, &workflow)?;
    let start = workflow
        .nodes
        .iter()
        .find(|node| node.definition.kind == BuiltinNode::Start)
        .context("The published workflow has no Start node")?;
    let start_request = WorkflowHostRequest::Start {
        document: document.clone(),
    };
    let start_artifact = insert_artifact(
        db,
        project,
        run.id.clone(),
        start.instance.id.clone(),
        "document".into(),
        ArtifactValue::Markdown(document.clone()),
    )?;
    record_host_completion(
        db,
        &start_artifact,
        "",
        &host_request_revision(&BTreeMap::new(), &start_request)?,
    )?;
    super::inspection::record_workflow_input_ids(db, &start_artifact, "", &Default::default())?;

    // Initial context nodes have no executable side effects. Binding their
    // exact input here leaves the first scheduler/manual stage ready at commit.
    for node in workflow
        .nodes
        .iter()
        .filter(|node| node.definition.kind == BuiltinNode::LoadContext)
    {
        let direct_source = workflow
            .wires
            .iter()
            .filter(|wire| wire.to.node == node.instance.id)
            .all(|wire| wire.from.node == start.instance.id);
        if direct_source {
            let artifact = insert_artifact(
                db,
                project,
                run.id.clone(),
                node.instance.id.clone(),
                "context".into(),
                start_artifact.value.clone(),
            )?;
            let inputs = BTreeMap::from([("document".into(), start_artifact.clone())]);
            record_host_completion(
                db,
                &artifact,
                "",
                &host_request_revision(&inputs, &WorkflowHostRequest::LoadContext)?,
            )?;
            super::inspection::record_workflow_completion_inputs(db, &artifact, "", &inputs)?;
        }
    }
    let receipt = WorkflowLaunchReceipt {
        workflow_run_id: run.id.clone(),
        version: revision.clone(),
        source: document,
        origin: origin.clone(),
        checklist_item,
    };
    db.execute(
        "INSERT INTO workflow_launches VALUES (?1, ?2, ?3, ?4)",
        params![
            run.id,
            project,
            key,
            protection::protect(&serde_json::to_vec(&receipt)?)?
        ],
    )?;
    super::control::refresh(db, project, &run.id)?;
    Ok(run)
}

fn origin_checklist_item(
    db: &Connection,
    project: &str,
    origin: &WorkflowLaunchOrigin,
) -> Result<Option<ChecklistProgressReceipt>> {
    let WorkflowLaunchOrigin::Rule {
        event_id, work_id, ..
    } = origin
    else {
        return Ok(None);
    };
    let payload: String = db.query_row(
        "SELECT payload FROM project_work_events WHERE id = ?1 AND project_id = ?2 AND task_id = ?3",
        params![event_id, project, work_id], |row| row.get(0),
    ).optional()?.context("The Agile event does not identify this work item")?;
    let event = serde_json::from_str::<AgileEventData>(&payload)?;
    let (item_id, revision, checked) = match event {
        AgileEventData::ChecklistChecked { item_id, revision } => (item_id, revision, true),
        AgileEventData::ChecklistReopened { item_id, revision } => (item_id, revision, false),
        _ => return Ok(None),
    };
    let saved: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM project_checklist_item_revisions
         WHERE project_id = ?1 AND task_id = ?2 AND item_id = ?3 AND revision = ?4 AND checked = ?5)",
        params![project, work_id, item_id, revision, checked], |row| row.get(0),
    )?;
    ensure!(
        saved,
        "The Agile event did not pin an existing checklist revision"
    );
    Ok(Some(ChecklistProgressReceipt {
        item_id,
        revision,
        checked,
    }))
}

pub(super) fn load_workflow_launch(
    db: &Connection,
    project: &str,
    run: &str,
) -> Result<Option<WorkflowLaunchReceipt>> {
    let saved: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM workflow_launches WHERE project_id = ?1 AND pipeline_run_id = ?2",
            params![project, run],
            |row| row.get(0),
        )
        .optional()?;
    let Some(bytes) = saved else { return Ok(None) };
    let mut receipt = open_receipt(&bytes)?;
    if receipt.checklist_item.is_none() {
        receipt.checklist_item = origin_checklist_item(db, project, &receipt.origin)?;
    }
    Ok(Some(receipt))
}

fn open_receipt(bytes: &[u8]) -> Result<WorkflowLaunchReceipt> {
    Ok(serde_json::from_slice(&protection::unprotect(bytes)?)?)
}

pub(super) fn require_launch_work(
    db: &Connection,
    project: &str,
    run: &str,
    selected: Option<&str>,
) -> Result<()> {
    if let Some(receipt) = load_workflow_launch(db, project, run)?
        && let Some(bound) = receipt.origin.work_id()
    {
        ensure!(
            selected == Some(bound),
            "The workflow must link the Task or Sub-Task selected at launch"
        );
    }
    Ok(())
}

pub(crate) fn require_launch_checklist_item(
    db: &Connection,
    project: &str,
    run: &str,
    task: &str,
    item: Option<&str>,
) -> Result<Option<ChecklistProgressReceipt>> {
    let Some(receipt) = load_workflow_launch(db, project, run)? else {
        return Ok(None);
    };
    if let Some(work) = receipt.origin.work_id() {
        ensure!(
            work == task,
            "The workflow work scope belongs to a different Task or Sub-Task"
        );
    }
    if let Some(scope) = &receipt.checklist_item {
        ensure!(
            item == Some(scope.item_id.as_str()),
            "The workflow is scoped to the checklist item selected by its Agile event"
        );
    }
    Ok(receipt.checklist_item)
}

pub(crate) fn pinned_workflow_checklist_for_job(
    db: &Connection,
    project: &str,
    job: &str,
    task: &str,
    item: Option<&str>,
) -> Result<Option<ChecklistProgressReceipt>> {
    let pipeline: Option<String> = db.query_row(
        "SELECT pipeline_run_id FROM job_pipeline_origins WHERE project_id = ?1 AND job_id = ?2",
        params![project, job], |row| row.get(0),
    ).optional()?;
    match pipeline {
        Some(pipeline) => require_launch_checklist_item(db, project, &pipeline, task, item),
        None => Ok(None),
    }
}
