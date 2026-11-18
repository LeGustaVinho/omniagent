use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::tasks::{
    TaskDetails, insert_planned_epic_task, insert_project_epic, pipeline_work_origin,
};

use super::{
    ArtifactKind, ArtifactValue, NodeInputs, TaskReference, WorkflowDeliveryUnit,
    artifacts::load_artifact, launch::require_launch_work,
};

const MAX_TASKS_PER_BATCH: usize = 256;

pub(super) mod attempts;
pub(super) mod execution;
pub(crate) mod retry;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskDraft {
    key: String,
    title: String,
    description: String,
    priority: i32,
    depends_on: Vec<String>,
    acceptance_criteria: Vec<String>,
    sdd_requirements: Vec<String>,
    bdd_scenarios: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BacklogDraft {
    epic_title: String,
    tasks: Vec<TaskDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklogReceipt {
    pub epic_id: String,
    pub sources: NodeInputs,
    pub tasks: Vec<BacklogTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklogTask {
    pub task: TaskReference,
    specification: TaskDraft,
}

struct SpecificationEvidence {
    sdd: Map<String, Value>,
    bdd: Map<String, Value>,
    context: Map<String, Value>,
}

impl SpecificationEvidence {
    fn load(db: &Connection, project: &str, run: &str, sources: &NodeInputs) -> Result<Self> {
        let document = |port: &str| -> Result<Map<String, Value>> {
            let id = sources
                .get(port)
                .with_context(|| format!("The backlog has no approved {port}"))?;
            let artifact = load_artifact(db, project, run, id, ArtifactKind::JsonObject)?
                .with_context(|| format!("The approved {port} is unavailable"))?;
            let ArtifactValue::JsonObject(document) = artifact.value else {
                anyhow::bail!("The approved {port} is not a structured specification");
            };
            Ok(document)
        };
        let context = ["design", "strategy"]
            .into_iter()
            .filter(|port| sources.contains_key(*port))
            .map(|port| Ok((port.to_owned(), Value::Object(document(port)?))))
            .collect::<Result<_>>()?;
        Ok(Self {
            sdd: document("sdd")?,
            bdd: document("bdd")?,
            context,
        })
    }

    fn select(&self, task: &TaskDraft) -> Result<Value> {
        let sdd = referenced_document(&self.sdd, "requirements", &task.sdd_requirements)?;
        let bdd = referenced_document(&self.bdd, "scenarios", &task.bdd_scenarios)?;
        let mut evidence = self.context.clone();
        evidence.insert("sdd".into(), Value::Object(sdd));
        evidence.insert("bdd".into(), Value::Object(bdd));
        Ok(Value::Object(evidence))
    }
}

fn referenced_document(
    source: &Map<String, Value>,
    field: &str,
    references: &[String],
) -> Result<Map<String, Value>> {
    let rows = source
        .get(field)
        .and_then(Value::as_array)
        .with_context(|| format!("The approved specification needs a {field} array"))?;
    let mut ids = BTreeMap::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .with_context(|| format!("Each approved {field} entry needs a stable ID"))?;
        ensure!(
            ids.insert(id, row).is_none(),
            "The approved {field} repeats ID {id}"
        );
    }
    let selected = references
        .iter()
        .map(|id| {
            ids.get(id.as_str())
                .map(|row| (*row).clone())
                .with_context(|| format!("Backlog reference {id} is absent from approved {field}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut document = source.clone();
    document.insert(field.into(), Value::Array(selected));
    Ok(document)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklogDeliveryPlan {
    pub task: BacklogTask,
    pub parent_run_id: String,
    pub backlog_artifact_id: String,
    pub source_artifacts: NodeInputs,
    pub checkout_id: String,
    pub destination_branch: String,
    pub prerequisite_integrations: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<retry::BacklogCiRepair>,
}

impl BacklogDeliveryPlan {
    pub(super) fn unit_scope(&self) -> (&str, &[String]) {
        (
            &self.task.specification.title,
            &self.task.specification.acceptance_criteria,
        )
    }
}

pub(crate) fn backlog_commit_message(
    db: &Connection,
    unit: &WorkflowDeliveryUnit,
    purpose: &str,
) -> Result<Option<String>> {
    let plan = load_artifact(
        db,
        &unit.project_id,
        &unit.workflow_run_id,
        &unit.plan_revision,
        ArtifactKind::Plan,
    )?;
    let Some(ArtifactValue::BacklogPlan(plan)) = plan.map(|artifact| artifact.value) else {
        return Ok(None);
    };
    ensure!(
        plan.task.task.project_id == unit.project_id && plan.task.task.task_id == unit.task_id,
        "The commit must reference the approved backlog task"
    );
    let sdd = plan
        .source_artifacts
        .get("sdd")
        .context("The approved task plan lost its SDD revision")?;
    let requirements = serde_json::to_string(&plan.task.specification.sdd_requirements)?;
    Ok(Some(format!(
        "{purpose}\n\nOmniAgent-Task: {}\nOmniAgent-Backlog-Key: {}\nOmniAgent-Plan: {}\nOmniAgent-SDD: {sdd}\nOmniAgent-Requirements: {requirements}",
        unit.task_id, plan.task.specification.key, unit.plan_revision
    )))
}

impl BacklogDraft {
    fn sorted_tasks(&self, existing: &BTreeMap<String, String>) -> Result<Vec<&TaskDraft>> {
        ensure!(
            !self.epic_title.trim().is_empty() && self.epic_title.len() <= 512,
            "The backlog needs an Epic title"
        );
        ensure!(
            (1..=MAX_TASKS_PER_BATCH).contains(&self.tasks.len()),
            "The backlog needs 1 to {MAX_TASKS_PER_BATCH} tasks"
        );
        let mut drafts = BTreeMap::new();
        let mut prerequisites = BTreeMap::new();
        let mut dependents = BTreeMap::<&str, Vec<&str>>::new();
        let mut ready = BTreeSet::new();
        for task in &self.tasks {
            task.validate()?;
            let key = task.key.as_str();
            ensure!(
                !existing.contains_key(key) && drafts.insert(key, task).is_none(),
                "Backlog task key {key} is already in use"
            );
            let mut unresolved = 0;
            for dependency in &task.depends_on {
                if !existing.contains_key(dependency) {
                    dependents.entry(dependency).or_default().push(key);
                    unresolved += 1;
                }
            }
            prerequisites.insert(key, unresolved);
            if unresolved == 0 {
                ready.insert((Reverse(task.priority), key));
            }
        }
        for key in dependents.keys() {
            ensure!(
                drafts.contains_key(key),
                "Unknown backlog prerequisite {key}"
            );
        }
        let mut ordered = Vec::with_capacity(drafts.len());
        while let Some((_, key)) = ready.pop_first() {
            ordered.push(drafts[key]);
            for dependent in dependents.get(key).into_iter().flatten() {
                let count = prerequisites
                    .get_mut(dependent)
                    .context("Missing backlog task")?;
                *count -= 1;
                if *count == 0 {
                    ready.insert((Reverse(drafts[dependent].priority), *dependent));
                }
            }
        }
        ensure!(
            ordered.len() == drafts.len(),
            "The backlog dependencies contain a cycle"
        );
        Ok(ordered)
    }
}

impl TaskDraft {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.key.is_empty()
                && self.key.len() <= 128
                && self
                    .key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
            "A backlog key must use letters, digits, dashes, underscores or periods"
        );
        ensure!(
            !self.title.trim().is_empty()
                && self.title.len() <= 512
                && !self.description.trim().is_empty(),
            "Task {} needs a title and description",
            self.key
        );
        ensure!(
            !self.acceptance_criteria.is_empty() && !self.sdd_requirements.is_empty(),
            "Task {} needs acceptance criteria and SDD requirement references",
            self.key
        );
        for values in [
            &self.depends_on,
            &self.acceptance_criteria,
            &self.sdd_requirements,
            &self.bdd_scenarios,
        ] {
            let mut unique = BTreeSet::new();
            ensure!(
                values
                    .iter()
                    .all(|value| !value.trim().is_empty() && unique.insert(value)),
                "Task {} contains an empty or duplicated entry",
                self.key
            );
        }
        Ok(())
    }
}

// Called inside the Agile action transaction, which also publishes the receipt.
pub(super) fn materialize(
    db: &Connection,
    project: &str,
    run: &str,
    plan: Map<String, Value>,
    sources: &NodeInputs,
) -> Result<BacklogReceipt> {
    require_launch_work(db, project, run, None)?;
    let draft: BacklogDraft = serde_json::from_value(Value::Object(plan))
        .context("The approved backlog does not match the task contract")?;
    let mut query = db.prepare(
        "SELECT task_key, task_id FROM workflow_backlog_tasks WHERE pipeline_run_id = ?1",
    )?;
    let mut identities: BTreeMap<String, String> = query
        .query_map([run], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let ordered = draft.sorted_tasks(&identities)?;
    let specification_evidence = SpecificationEvidence::load(db, project, run, sources)?;
    for task in &ordered {
        specification_evidence.select(task)?;
    }
    let origin = pipeline_work_origin(db, project, run)?;
    let epic: Option<String> = db
        .query_row(
            "SELECT epic_id FROM workflow_backlog_epics WHERE pipeline_run_id = ?1",
            [run],
            |row| row.get(0),
        )
        .optional()?;
    let epic_id = match epic {
        Some(epic_id) => epic_id,
        None => {
            let epic = insert_project_epic(db, project.into(), draft.epic_title.trim().into())?;
            db.execute(
                "INSERT INTO workflow_backlog_epics VALUES (?1, ?2)",
                params![run, epic.id],
            )?;
            epic.id
        }
    };
    let mut tasks = Vec::with_capacity(ordered.len());
    for specification in ordered {
        let prerequisites = specification
            .depends_on
            .iter()
            .map(|key| {
                identities
                    .get(key)
                    .cloned()
                    .context("Backlog prerequisite lost its task")
            })
            .collect::<Result<_>>()?;
        let details = TaskDetails {
            priority: specification.priority,
            prerequisites,
            acceptance_criteria: specification.acceptance_criteria.clone(),
            ..TaskDetails::default()
        };
        let created = insert_planned_epic_task(
            db,
            project,
            specification.title.clone(),
            epic_id.clone(),
            specification.description.clone(),
            details,
            &origin,
        )?;
        db.execute(
            "INSERT INTO workflow_backlog_tasks VALUES (?1, ?2, ?3)",
            params![run, specification.key, created.id],
        )?;
        identities.insert(specification.key.clone(), created.id.clone());
        tasks.push(BacklogTask {
            task: TaskReference {
                project_id: project.into(),
                task_id: created.id,
            },
            specification: specification.clone(),
        });
    }
    Ok(BacklogReceipt {
        epic_id,
        sources: sources.clone(),
        tasks,
    })
}
