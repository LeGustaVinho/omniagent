use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{protection, store::Store};

use super::{
    ChecklistExecutionMode, ChecklistItem, activity::load_task_details,
    checklists::load_checklist_items,
};

mod editor;
pub use editor::{
    DescriptionDraft, DescriptionEditorConflict, DescriptionEditorSave, DescriptionEditorState,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionOwner {
    Epic(String),
    Work(String),
}

impl DescriptionOwner {
    fn database_scope(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Epic(id) => (Some(id), None),
            Self::Work(id) => (None, Some(id)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionRevision {
    pub id: String,
    pub owner: DescriptionOwner,
    pub revision: i64,
    pub markdown: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkDescriptions {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub epic: Option<DescriptionRevision>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub task: Option<DescriptionRevision>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subtask: Option<DescriptionRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDispatchContext {
    pub task_id: String,
    pub descriptions: WorkDescriptions,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub direct_work: Option<TaskDirectWork>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checklist_mode: Option<ChecklistExecutionMode>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checklist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checklist_item_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub checklist: Vec<ChecklistItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDirectWork {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_title: Option<String>,
    pub acceptance_criteria: Vec<String>,
    pub prerequisites: Vec<String>,
    pub blockers: Vec<String>,
}

impl TaskDispatchContext {
    pub(crate) fn append_to_prompt(&self, prompt: &mut String) -> Result<()> {
        match self.checklist_mode {
            Some(ChecklistExecutionMode::TrackProgress) => prompt.push_str(
                "\n\nCarry out the selected task's direct work and use its checklist to track progress.",
            ),
            Some(ChecklistExecutionMode::RunByItem) => prompt.push_str(
                "\n\nCarry out the selected checklist item. Use the owning task and its ancestors as context.",
            ),
            None => {}
        }
        if self.direct_work.is_some()
            || self.descriptions != WorkDescriptions::default()
            || !self.checklist.is_empty()
        {
            let serialized = serde_json::to_string(self)?;
            prompt.push_str("\n\nSaved task context\n");
            prompt.push_str(&serialized);
        }
        Ok(())
    }
}

impl Store {
    pub async fn save_project_description(
        &self,
        project: String,
        owner: DescriptionOwner,
        expected_revision: Option<String>,
        markdown: Option<String>,
    ) -> Result<DescriptionRevision> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let previous = current_description(&tx, &project, &owner)?;
            ensure!(
                previous.as_ref().map(|saved| &saved.id) == expected_revision.as_ref(),
                "the saved description revision changed"
            );
            let revision = write_description(&tx, &project, owner, previous, markdown)?;
            tx.commit()?;
            Ok(revision)
        })
        .await
    }

    pub async fn project_description(
        &self,
        project: String,
        owner: DescriptionOwner,
    ) -> Result<Option<DescriptionRevision>> {
        self.execute(move |db| current_description(db, &project, &owner))
            .await
    }

    pub async fn project_description_revision(
        &self,
        project: String,
        revision_id: String,
    ) -> Result<Option<DescriptionRevision>> {
        self.execute(move |db| description_revision(db, &project, &revision_id))
            .await
    }

    pub(crate) async fn task_dispatch_context(
        &self,
        run: String,
    ) -> Result<Option<TaskDispatchContext>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let scope = tx.query_row(
                "SELECT r.project_id, r.task_id, m.parent_task_id, coalesce(m.epic_id, parent.epic_id),
                        c.id, r.checklist_item_id, execution.mode, task.title, parent_task.title, r.job_id
                 FROM task_runs r
                 JOIN project_tasks task ON task.project_id = r.project_id AND task.id = r.task_id
                 LEFT JOIN project_task_memberships m ON m.project_id = r.project_id AND m.task_id = r.task_id
                 LEFT JOIN project_task_memberships parent ON parent.project_id = r.project_id AND parent.task_id = m.parent_task_id
                 LEFT JOIN project_tasks parent_task ON parent_task.project_id = r.project_id AND parent_task.id = m.parent_task_id
                 LEFT JOIN project_checklists c ON c.project_id = r.project_id AND c.task_id = r.task_id
                 LEFT JOIN job_checklist_modes execution ON execution.job_id = r.job_id
                 WHERE r.run_id = ?1",
                [&run],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?, row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?, row.get::<_, String>(7)?, row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?)),
            ).optional()?;
            let Some((project, task, parent, epic, checklist_id, checklist_item_id, mode, title, parent_title, job)) = scope else {
                return Ok(None);
            };
            let mut descriptions = WorkDescriptions {
                epic: epic.map(|id| current_description(&tx, &project, &DescriptionOwner::Epic(id)))
                    .transpose()?.flatten().filter(|revision| revision.markdown.is_some()),
                task: active_work_description(&tx, &project, parent.as_ref().unwrap_or(&task))?,
                subtask: None,
            };
            if parent.is_some() {
                descriptions.subtask = active_work_description(&tx, &project, &task)?;
            }
            let checklist_mode = mode.as_deref().map(ChecklistExecutionMode::from_sql).transpose()?;
            let direct_work = if checklist_mode.is_some() {
                let details = load_task_details(&tx, &project, &task)?;
                Some(TaskDirectWork {
                    title, parent_title, acceptance_criteria: details.acceptance_criteria,
                    prerequisites: details.prerequisites, blockers: details.blockers,
                })
            } else { None };
            let item = checklist_item_id.as_deref();
            let pinned_item = match (job.as_deref(), item) {
                (Some(job), Some(item)) => crate::workflows::pinned_workflow_checklist_for_job(&tx, &project, job, &task, Some(item))?,
                _ => None,
            };
            let checklist = load_checklist_items(&tx, &project, &task, item, pinned_item.map(|pin| pin.revision))?;
            ensure!(checklist_item_id.is_none() || checklist.len() == 1, "the selected checklist item has no saved revision");
            Ok(Some(TaskDispatchContext {
                task_id: task, descriptions, direct_work, checklist_mode, checklist_id, checklist_item_id, checklist,
            }))
        }).await
    }
}

fn write_description(
    db: &Connection,
    project: &str,
    owner: DescriptionOwner,
    previous: Option<DescriptionRevision>,
    markdown: Option<String>,
) -> Result<DescriptionRevision> {
    if let Some(previous) = previous.as_ref().filter(|saved| saved.markdown == markdown) {
        return Ok(previous.clone());
    }
    let (epic, task) = owner.database_scope();
    let id = Uuid::new_v4().to_string();
    let revision = previous.map_or(1, |saved| saved.revision + 1);
    let body = markdown
        .as_ref()
        .map(|value| protection::protect(value.as_bytes()))
        .transpose()?;
    db.execute(
        "INSERT INTO project_description_revisions (project_id, id, epic_id, task_id, revision, body)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![project, id, epic, task, revision, body],
    ).context("description owner must belong to the project")?;
    Ok(DescriptionRevision {
        id,
        owner,
        revision,
        markdown,
    })
}

fn description_revision(
    db: &Connection,
    project: &str,
    revision_id: &str,
) -> Result<Option<DescriptionRevision>> {
    read_description(
        db,
        "SELECT id, epic_id, task_id, revision, body FROM project_description_revisions
         WHERE project_id = ?1 AND id = ?2",
        project,
        revision_id,
    )
}

fn active_work_description(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<Option<DescriptionRevision>> {
    Ok(
        current_description(db, project, &DescriptionOwner::Work(task.into()))?
            .filter(|revision| revision.markdown.is_some()),
    )
}

fn current_description(
    db: &Connection,
    project: &str,
    owner: &DescriptionOwner,
) -> Result<Option<DescriptionRevision>> {
    let (sql, id) = match owner {
        DescriptionOwner::Epic(id) => (
            "SELECT id, epic_id, task_id, revision, body FROM project_description_revisions
             WHERE project_id = ?1 AND epic_id = ?2 ORDER BY revision DESC LIMIT 1",
            id,
        ),
        DescriptionOwner::Work(id) => (
            "SELECT id, epic_id, task_id, revision, body FROM project_description_revisions
             WHERE project_id = ?1 AND task_id = ?2 ORDER BY revision DESC LIMIT 1",
            id,
        ),
    };
    read_description(db, sql, project, id)
}

fn read_description(
    db: &Connection,
    sql: &str,
    project: &str,
    key: &str,
) -> Result<Option<DescriptionRevision>> {
    let saved = db
        .query_row(sql, params![project, key], |row| {
            let owner = match row.get(1)? {
                Some(id) => DescriptionOwner::Epic(id),
                None => DescriptionOwner::Work(row.get(2)?),
            };
            Ok((
                row.get(0)?,
                owner,
                row.get(3)?,
                row.get::<_, Option<Vec<u8>>>(4)?,
            ))
        })
        .optional()?;
    saved
        .map(|(id, owner, revision, encrypted)| {
            let markdown = encrypted
                .as_deref()
                .map(protection::unprotect)
                .transpose()?
                .map(String::from_utf8)
                .transpose()?;
            Ok(DescriptionRevision {
                id,
                owner,
                revision,
                markdown,
            })
        })
        .transpose()
}
