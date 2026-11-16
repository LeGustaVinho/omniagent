use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{protection, store::Store};

use super::ChecklistItem;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionOwner {
    Epic(String),
    Work(String),
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
    pub checklist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checklist_item_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub checklist: Vec<ChecklistItem>,
}

impl TaskDispatchContext {
    pub(crate) fn append_to_prompt(&self, prompt: &mut String) -> Result<()> {
        if self.descriptions != WorkDescriptions::default() || !self.checklist.is_empty() {
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
            if let Some(previous) = previous.as_ref().filter(|saved| saved.markdown == markdown) {
                return Ok(previous.clone());
            }
            let (epic, task) = match &owner {
                DescriptionOwner::Epic(id) => (Some(id), None),
                DescriptionOwner::Work(id) => (None, Some(id)),
            };
            let id = Uuid::new_v4().to_string();
            let revision = previous.map_or(1, |saved| saved.revision + 1);
            let body = markdown.as_ref().map(|value| protection::protect(value.as_bytes())).transpose()?;
            tx.execute(
                "INSERT INTO project_description_revisions (project_id, id, epic_id, task_id, revision, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![project, id, epic, task, revision, body],
            ).context("description owner must belong to the project")?;
            tx.commit()?;
            Ok(DescriptionRevision { id, owner, revision, markdown })
        }).await
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
        self.execute(move |db| {
            read_description(
                db,
                "SELECT id, epic_id, task_id, revision, body FROM project_description_revisions
                 WHERE project_id = ?1 AND id = ?2",
                &project,
                &revision_id,
            )
        })
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
                        c.id, r.checklist_item_id
                 FROM task_runs r
                 LEFT JOIN project_task_memberships m ON m.project_id = r.project_id AND m.task_id = r.task_id
                 LEFT JOIN project_task_memberships parent ON parent.project_id = r.project_id AND parent.task_id = m.parent_task_id
                 LEFT JOIN project_checklists c ON c.project_id = r.project_id AND c.task_id = r.task_id
                 WHERE r.run_id = ?1",
                [&run],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?, row.get::<_, Option<String>>(5)?)),
            ).optional()?;
            let Some((project, task, parent, epic, checklist_id, checklist_item_id)) = scope else {
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
            let checklist = dispatch_checklist(&tx, &project, &task, checklist_item_id.as_deref())?;
            Ok(Some(TaskDispatchContext {
                task_id: task, descriptions, checklist_id, checklist_item_id, checklist,
            }))
        }).await
    }
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

fn dispatch_checklist(
    db: &Connection,
    project: &str,
    task: &str,
    item: Option<&str>,
) -> Result<Vec<ChecklistItem>> {
    let mut query = db.prepare(
        "SELECT i.id, i.position, r.revision, r.body, r.checked FROM project_checklist_items i
         JOIN project_checklist_item_revisions r ON r.project_id = i.project_id AND r.task_id = i.task_id
         AND r.item_id = i.id AND r.revision = (
             SELECT max(revision) FROM project_checklist_item_revisions
             WHERE project_id = i.project_id AND task_id = i.task_id AND item_id = i.id
         )
         WHERE i.project_id = ?1 AND i.task_id = ?2 AND (?3 IS NULL OR i.id = ?3)
         ORDER BY i.position",
    )?;
    let mut rows = query.query(params![project, task, item])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let encrypted: Vec<u8> = row.get(3)?;
        items.push(ChecklistItem {
            id: row.get(0)?,
            position: row.get(1)?,
            revision: row.get(2)?,
            body: String::from_utf8(protection::unprotect(&encrypted)?)?,
            checked: row.get(4)?,
        });
    }
    ensure!(
        item.is_none() || items.len() == 1,
        "the selected checklist item has no saved revision"
    );
    Ok(items)
}
