use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgileEventData, AgileEventOrigin, WorkStateChanges, agile_events::publish_event};
use crate::{protection, store::Store};

pub(super) mod progress;
#[cfg(test)]
mod tests;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistExecutionMode {
    #[default]
    TrackProgress,
    RunByItem,
}

impl ChecklistExecutionMode {
    fn key(self) -> &'static str {
        match self {
            Self::TrackProgress => "track_progress",
            Self::RunByItem => "run_by_item",
        }
    }

    pub(super) fn from_sql(key: &str) -> Result<Self> {
        match key {
            "track_progress" => Ok(Self::TrackProgress),
            "run_by_item" => Ok(Self::RunByItem),
            _ => anyhow::bail!("unknown checklist execution mode"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistItem {
    pub id: String,
    pub position: i64,
    pub revision: i64,
    pub body: String,
    pub checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskChecklist {
    pub id: String,
    pub mode: ChecklistExecutionMode,
    pub items: Vec<ChecklistItem>,
}

impl Store {
    pub async fn project_checklist_mode(&self, project: String) -> Result<ChecklistExecutionMode> {
        self.execute(move |db| {
            let mode: String = db
                .query_row(
                    "SELECT coalesce(settings.mode, 'track_progress') FROM projects p
                 LEFT JOIN project_checklist_modes settings ON settings.project_id = p.id
                 WHERE p.id = ?1",
                    [&project],
                    |row| row.get(0),
                )
                .context("checklist project is unavailable")?;
            ChecklistExecutionMode::from_sql(&mode)
        })
        .await
    }

    pub async fn set_project_checklist_mode(
        &self,
        project: String,
        mode: ChecklistExecutionMode,
    ) -> Result<()> {
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_checklist_modes (project_id, mode) VALUES (?1, ?2)
                 ON CONFLICT(project_id) DO UPDATE SET mode = excluded.mode",
                params![project, mode.key()],
            )
            .context("checklist project is unavailable")?;
            Ok(())
        })
        .await
    }

    pub async fn task_checklist_mode(
        &self,
        project: String,
        task: String,
    ) -> Result<ChecklistExecutionMode> {
        self.execute(move |db| effective_mode(db, &project, &task))
            .await
    }

    pub async fn set_task_checklist_mode(
        &self,
        project: String,
        task: String,
        mode: Option<ChecklistExecutionMode>,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match mode {
                Some(mode) => {
                    tx.execute(
                        "INSERT INTO task_checklist_modes (project_id, task_id, mode) VALUES (?1, ?2, ?3)
                         ON CONFLICT(project_id, task_id) DO UPDATE SET mode = excluded.mode",
                        params![project, task, mode.key()],
                    ).context("checklist mode requires a task or sub-task in this project")?;
                }
                None => {
                    effective_mode(&tx, &project, &task)?;
                    tx.execute(
                        "DELETE FROM task_checklist_modes WHERE project_id = ?1 AND task_id = ?2",
                        params![project, task],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn project_checklist(
        &self,
        project: String,
        task: String,
    ) -> Result<Option<TaskChecklist>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let mode = effective_mode(&tx, &project, &task)?;
            let id = tx
                .query_row(
                    "SELECT id FROM project_checklists WHERE project_id = ?1 AND task_id = ?2",
                    params![project, task],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = id else { return Ok(None) };
            let items = load_checklist_items(&tx, &project, &task, None, None)?;
            Ok(Some(TaskChecklist { id, mode, items }))
        })
        .await
    }

    pub async fn append_project_checklist_item(
        &self,
        project: String,
        task: String,
        body: String,
    ) -> Result<ChecklistItem> {
        ensure!(!body.trim().is_empty(), "checklist item text is empty");
        let id = Uuid::new_v4().to_string();
        let encrypted = protection::protect(body.as_bytes())?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let position = tx.query_row(
                "INSERT INTO project_checklist_items (project_id, task_id, id, position)
                 SELECT ?1, ?2, ?3, coalesce(max(position) + 1, 0)
                 FROM project_checklist_items WHERE project_id = ?1 AND task_id = ?2
                 RETURNING position",
                params![project, task, id],
                |row| row.get(0),
            ).context("checklist item requires a checklist on this task")?;
            tx.execute(
                "INSERT INTO project_checklist_item_revisions
                 (project_id, task_id, item_id, revision, body, checked) VALUES (?1, ?2, ?3, 1, ?4, 0)",
                params![project, task, id, encrypted],
            )?;
            tx.commit()?;
            Ok(ChecklistItem { id, position, revision: 1, body, checked: false })
        }).await
    }

    pub async fn revise_project_checklist_item(
        &self,
        project: String,
        task: String,
        item: String,
        expected_revision: i64,
        body: String,
        checked: bool,
    ) -> Result<ChecklistItem> {
        ensure!(!body.trim().is_empty(), "checklist item text is empty");
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = progress::item_at_expected_revision(
                &tx,
                &project,
                &task,
                &item,
                expected_revision,
            )?;
            let revised = revise_checklist_item(
                &tx,
                &project,
                &task,
                &current,
                body,
                checked,
                &AgileEventOrigin::Operator,
            )?;
            progress::record_operator_check_change(&tx, &project, &task, &current, &revised)?;
            tx.commit()?;
            Ok(revised)
        })
        .await
    }

    pub async fn project_checklist_item_revision(
        &self,
        project: String,
        task: String,
        item: String,
        revision: i64,
    ) -> Result<Option<ChecklistItem>> {
        self.execute(move |db| {
            Ok(load_checklist_items(db, &project, &task, Some(&item), Some(revision))?.pop())
        })
        .await
    }
}

fn revise_checklist_item(
    db: &Connection,
    project: &str,
    task: &str,
    current: &ChecklistItem,
    body: String,
    checked: bool,
    origin: &AgileEventOrigin,
) -> Result<ChecklistItem> {
    if current.body == body && current.checked == checked {
        return Ok(current.clone());
    }
    let revision = current
        .revision
        .checked_add(1)
        .context("checklist item revisions exhausted")?;
    let states = (current.checked != checked)
        .then(|| WorkStateChanges::capture(db, project, task))
        .transpose()?;
    let encrypted = protection::protect(body.as_bytes())?;
    db.execute(
        "INSERT INTO project_checklist_item_revisions
         (project_id, task_id, item_id, revision, body, checked) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![project, task, current.id, revision, encrypted, checked],
    )?;
    if let Some(states) = states {
        let item_id = current.id.clone();
        let event = if checked {
            AgileEventData::ChecklistChecked { item_id, revision }
        } else {
            AgileEventData::ChecklistReopened { item_id, revision }
        };
        publish_event(db, project, task, event, origin)?;
        states.publish(db, project, origin)?;
    }
    Ok(ChecklistItem {
        revision,
        body,
        checked,
        ..current.clone()
    })
}

fn effective_mode(db: &Connection, project: &str, task: &str) -> Result<ChecklistExecutionMode> {
    let mode: String = db.query_row(
        "SELECT coalesce(own.mode, parent.mode, defaults.mode, 'track_progress')
         FROM project_tasks task
         LEFT JOIN project_task_memberships membership ON membership.project_id = task.project_id
            AND membership.task_id = task.id
         LEFT JOIN task_checklist_modes own ON own.project_id = task.project_id AND own.task_id = task.id
         LEFT JOIN task_checklist_modes parent ON parent.project_id = task.project_id
            AND parent.task_id = membership.parent_task_id
         LEFT JOIN project_checklist_modes defaults ON defaults.project_id = task.project_id
         WHERE task.project_id = ?1 AND task.id = ?2",
        params![project, task], |row| row.get(0),
    ).context("checklist mode requires a task or sub-task in this project")?;
    ChecklistExecutionMode::from_sql(&mode)
}

pub(crate) fn bind_checklist_job_mode(
    db: &Connection,
    project: &str,
    task: &str,
    job: &str,
    item: Option<&str>,
) -> Result<()> {
    let mode = effective_mode(db, project, task)?;
    match mode {
        ChecklistExecutionMode::TrackProgress => {
            ensure!(
                item.is_none(),
                "Track progress runs the whole task. Clear the checklist item selection"
            );
        }
        ChecklistExecutionMode::RunByItem => {
            ensure!(
                item.is_some(),
                "Run by item requires a checklist item selection"
            );
        }
    }
    db.execute(
        "INSERT INTO job_checklist_modes (job_id, mode) VALUES (?1, ?2)",
        params![job, mode.key()],
    )?;
    Ok(())
}

pub(super) fn load_checklist_items(
    db: &Connection,
    project: &str,
    task: &str,
    item: Option<&str>,
    revision: Option<i64>,
) -> Result<Vec<ChecklistItem>> {
    let mut query = db.prepare(
        "SELECT i.id, i.position, r.revision, r.body, r.checked FROM project_checklist_items i
         JOIN project_checklist_item_revisions r ON r.project_id = i.project_id AND r.task_id = i.task_id
         AND r.item_id = i.id AND r.revision = coalesce(?4, (
             SELECT max(revision) FROM project_checklist_item_revisions
             WHERE project_id = i.project_id AND task_id = i.task_id AND item_id = i.id
         ))
         WHERE i.project_id = ?1 AND i.task_id = ?2 AND (?3 IS NULL OR i.id = ?3)
         ORDER BY i.position",
    )?;
    let mut rows = query.query(params![project, task, item, revision])?;
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
    Ok(items)
}
