use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    protection,
    repository::{RepositoryMarkdownRevision, workflow_markdown_revision},
    store::Store,
    tasks::{AgileEventData, ChecklistProgressReceipt, bind_workflow_trigger_origin},
};

use super::{
    ArtifactValue, BuiltinNode, DeliveryRoles, NodeConfiguration, TaskReference, WorkflowArtifact,
    WorkflowHostRequest, WorkflowRevision, WorkflowRun,
    artifacts::insert_artifact,
    digest,
    execution::{host_request_revision, record_host_completion},
    registry::supports_pinned_execution,
    runtime::{insert_workflow_run, load_definition, load_workflow_run},
};

mod input;
pub use input::{TypedStartConfiguration, WorkflowStartSchema};

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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedWorkflowLaunch {
    project_id: String,
    workflow: WorkflowRevision,
    checkout_id: String,
    document_revision_id: Option<String>,
    request_id: String,
    work_id: Option<String>,
    #[serde(default)]
    inputs: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum WorkflowLaunchRequest {
    Markdown(ManualWorkflowLaunch),
    Typed(TypedWorkflowLaunch),
}

struct LaunchEntry {
    checkout_id: String,
    document_revision_id: Option<String>,
    inputs: Map<String, Value>,
}

impl From<WorkflowLaunchSource> for LaunchEntry {
    fn from(source: WorkflowLaunchSource) -> Self {
        Self {
            checkout_id: source.checkout_id,
            document_revision_id: Some(source.revision_id),
            inputs: Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLaunchEvent {
    pub id: String,
    pub work: TaskReference,
    pub data: AgileEventData,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLaunchReceipt {
    pub workflow_run_id: String,
    pub version: WorkflowRevision,
    #[serde(default)]
    pub checkout_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<RepositoryMarkdownRevision>,
    pub origin: WorkflowLaunchOrigin,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub inputs: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<WorkflowLaunchEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checklist_item: Option<ChecklistProgressReceipt>,
}

impl Store {
    pub async fn launch_workflow_manual(
        &self,
        request: ManualWorkflowLaunch,
    ) -> Result<WorkflowRun> {
        self.launch_workflow_request(WorkflowLaunchRequest::Markdown(request))
            .await
    }

    pub async fn launch_workflow_request(
        &self,
        request: WorkflowLaunchRequest,
    ) -> Result<WorkflowRun> {
        let (project, workflow, request_id, work_id, entry) = match request {
            WorkflowLaunchRequest::Markdown(request) => (
                request.project_id,
                request.workflow,
                request.request_id,
                request.work_id,
                request.source.into(),
            ),
            WorkflowLaunchRequest::Typed(request) => (
                request.project_id,
                request.workflow,
                request.request_id,
                request.work_id,
                LaunchEntry {
                    checkout_id: request.checkout_id,
                    document_revision_id: request.document_revision_id,
                    inputs: request.inputs,
                },
            ),
        };
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
                let run = launch_entry_in_tx(&tx, &project, &workflow, entry, &origin)?;
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
        let graph = self
            .builtin_delivery_for_project(project.clone(), DeliveryRoles::default())
            .await?;
        let revision = self
            .save_workflow_revision(
                project.clone(),
                super::builtin::BUILTIN_DELIVERY_ID.into(),
                graph,
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
    checkout_id: &str,
    revision_id: &str,
) -> Result<RepositoryMarkdownRevision> {
    workflow_markdown_revision(db, project, checkout_id, revision_id)?
        .context("The selected Markdown revision is unavailable in this project checkout")
}

pub(super) fn validate_workflow_launch_checkout(
    db: &Connection,
    project: &str,
    checkout: &str,
) -> Result<()> {
    let checkout_exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM checkouts WHERE project_id = ?1 AND id = ?2)",
        params![project, checkout],
        |row| row.get(0),
    )?;
    ensure!(
        checkout_exists,
        "The selected checkout is outside this project"
    );
    Ok(())
}

/// The caller owns the transaction that records the event and rule activation.
pub(crate) fn launch_workflow_in_tx(
    db: &Connection,
    project: &str,
    revision: &WorkflowRevision,
    checkout_id: &str,
    document_revision_id: Option<&str>,
    origin: &WorkflowLaunchOrigin,
) -> Result<WorkflowRun> {
    launch_entry_in_tx(
        db,
        project,
        revision,
        LaunchEntry {
            checkout_id: checkout_id.into(),
            document_revision_id: document_revision_id.map(str::to_owned),
            inputs: Map::new(),
        },
        origin,
    )
}

fn launch_entry_in_tx(
    db: &Connection,
    project: &str,
    revision: &WorkflowRevision,
    entry: LaunchEntry,
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
                && receipt.checkout_id == entry.checkout_id
                && receipt.source.as_ref().map(|source| &source.id)
                    == entry.document_revision_id.as_ref()
                && receipt.inputs == entry.inputs
                && receipt.origin == *origin,
            "The launch identity was already used with different inputs"
        );
        return load_workflow_run(db, project, &receipt.workflow_run_id)?
            .context("The saved workflow launch lost its run");
    }
    validate_workflow_launch_checkout(db, project, &entry.checkout_id)?;
    let work = if let Some(work) = origin.work_id() {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_tasks WHERE project_id = ?1 AND id = ?2)",
            params![project, work],
            |row| row.get(0),
        )?;
        ensure!(exists, "The launch work item is outside this project");
        Some(TaskReference {
            project_id: project.into(),
            task_id: work.into(),
        })
    } else {
        None
    };
    let event = origin_launch_event(db, project, origin)?;
    let checklist_item = event
        .as_ref()
        .map(|event| origin_checklist_item(db, event))
        .transpose()?
        .flatten();
    let document = entry
        .document_revision_id
        .as_ref()
        .map(|revision_id| {
            validate_workflow_launch_source(db, project, &entry.checkout_id, revision_id)
        })
        .transpose()?;
    let workflow = load_definition(db, project, revision)?
        .context("The published workflow revision is unavailable in this project")?;
    ensure!(
        workflow
            .nodes
            .iter()
            .all(|node| supports_pinned_execution(&node.definition)),
        "A pinned node executor is unavailable on this installation"
    );
    let start = workflow
        .nodes
        .iter()
        .find(|node| {
            matches!(
                node.definition.kind,
                BuiltinNode::Start | BuiltinNode::TypedStart
            )
        })
        .context("The published workflow has no Start node")?;
    let schema = WorkflowStartSchema::for_definition(&workflow);
    if let NodeConfiguration::TypedStart(configuration) = &start.instance.configuration {
        let schema = schema
            .as_ref()
            .context("The typed Start fields are unavailable")?;
        ensure!(
            event.is_none() || schema.accepts_automation(),
            "This workflow requires inputs supplied at manual launch"
        );
        configuration.validate_values(&entry.inputs)?;
        ensure!(
            !schema.requires_document || document.is_some(),
            "Select a saved Markdown revision for the Start document port"
        );
        ensure!(
            !schema.requires_work || work.is_some(),
            "Select the Task or Sub-Task for the Start task port"
        );
        ensure!(
            !schema.requires_event || event.is_some(),
            "This workflow needs an Agile event from an automation rule"
        );
    } else {
        ensure!(
            document.is_some(),
            "This workflow needs a saved Markdown revision"
        );
        ensure!(
            entry.inputs.is_empty(),
            "This workflow does not declare typed Start inputs"
        );
    }

    let run = insert_workflow_run(db, project, &workflow)?;
    if let Some(event) = &event {
        bind_workflow_trigger_origin(db, project, &run.id, &event.id)?;
    }
    let receipt = WorkflowLaunchReceipt {
        workflow_run_id: run.id.clone(),
        version: revision.clone(),
        checkout_id: entry.checkout_id,
        source: document,
        origin: origin.clone(),
        inputs: entry.inputs,
        event,
        checklist_item,
    };
    let document_output = receipt
        .source
        .clone()
        .map(|source| {
            record_entry_output(
                db,
                project,
                &run.id,
                &start.instance.id,
                "document",
                ArtifactValue::Markdown(source),
            )
        })
        .transpose()?;
    if schema.is_some() {
        let input_output = record_entry_output(
            db,
            project,
            &run.id,
            &start.instance.id,
            "inputs",
            ArtifactValue::JsonObject(receipt.inputs.clone()),
        )?;
        record_host_completion(
            db,
            &input_output,
            "",
            &digest(&serde_json::to_vec(&receipt)?),
        )?;
        if let Some(work) = work {
            record_entry_output(
                db,
                project,
                &run.id,
                &start.instance.id,
                "task",
                ArtifactValue::Task(work),
            )?;
        }
        if let Some(event) = receipt.event.clone() {
            record_entry_output(
                db,
                project,
                &run.id,
                &start.instance.id,
                "event",
                ArtifactValue::AgileEvent(event),
            )?;
        }
    } else {
        let artifact = document_output
            .as_ref()
            .context("The saved Markdown output is missing")?;
        let document = receipt
            .source
            .clone()
            .context("The saved Markdown revision is missing")?;
        let request = WorkflowHostRequest::Start { document };
        record_host_completion(
            db,
            artifact,
            "",
            &host_request_revision(&BTreeMap::new(), &request)?,
        )?;
    }

    // A legacy Load context directly connected to Start can bind its Markdown
    // revision before returning the run to the scheduler.
    if let Some(document) = &document_output {
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
                    document.value.clone(),
                )?;
                let inputs = BTreeMap::from([("document".into(), document.clone())]);
                record_host_completion(
                    db,
                    &artifact,
                    "",
                    &host_request_revision(&inputs, &WorkflowHostRequest::LoadContext)?,
                )?;
                super::inspection::record_workflow_completion_inputs(db, &artifact, "", &inputs)?;
            }
        }
    }
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

fn record_entry_output(
    db: &Connection,
    project: &str,
    run: &str,
    node: &str,
    port: &str,
    value: ArtifactValue,
) -> Result<WorkflowArtifact> {
    let artifact = insert_artifact(db, project, run.into(), node.into(), port.into(), value)?;
    super::inspection::record_workflow_input_ids(db, &artifact, "", &Default::default())?;
    Ok(artifact)
}

fn origin_launch_event(
    db: &Connection,
    project: &str,
    origin: &WorkflowLaunchOrigin,
) -> Result<Option<WorkflowLaunchEvent>> {
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
    Ok(Some(WorkflowLaunchEvent {
        id: event_id.clone(),
        work: TaskReference {
            project_id: project.into(),
            task_id: work_id.clone(),
        },
        data: serde_json::from_str(&payload)?,
    }))
}

fn origin_checklist_item(
    db: &Connection,
    event: &WorkflowLaunchEvent,
) -> Result<Option<ChecklistProgressReceipt>> {
    let (item_id, revision, checked) = match &event.data {
        AgileEventData::ChecklistChecked { item_id, revision } => (item_id, revision, true),
        AgileEventData::ChecklistReopened { item_id, revision } => (item_id, revision, false),
        _ => return Ok(None),
    };
    let saved: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM project_checklist_item_revisions
         WHERE project_id = ?1 AND task_id = ?2 AND item_id = ?3 AND revision = ?4 AND checked = ?5)",
        params![event.work.project_id, event.work.task_id, item_id, revision, checked], |row| row.get(0),
    )?;
    ensure!(
        saved,
        "The Agile event did not pin an existing checklist revision"
    );
    Ok(Some(ChecklistProgressReceipt {
        item_id: item_id.clone(),
        revision: *revision,
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
    if receipt.event.is_none() {
        receipt.event = origin_launch_event(db, project, &receipt.origin)?;
    }
    if receipt.checklist_item.is_none() {
        receipt.checklist_item = receipt
            .event
            .as_ref()
            .map(|event| origin_checklist_item(db, event))
            .transpose()?
            .flatten();
    }
    Ok(Some(receipt))
}

fn open_receipt(bytes: &[u8]) -> Result<WorkflowLaunchReceipt> {
    let mut receipt: WorkflowLaunchReceipt =
        serde_json::from_slice(&protection::unprotect(bytes)?)?;
    if receipt.checkout_id.is_empty() {
        receipt.checkout_id = receipt
            .source
            .as_ref()
            .context("The saved launch has no checkout")?
            .checkout_id
            .clone();
    }
    ensure!(
        receipt
            .source
            .as_ref()
            .is_none_or(|source| source.checkout_id == receipt.checkout_id),
        "The saved launch document belongs to another checkout"
    );
    Ok(receipt)
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
