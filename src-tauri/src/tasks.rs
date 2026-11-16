use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use crate::store::Store;

mod activity;
mod agile_transition;
mod checklists;
mod delivery;
mod descriptions;
pub(crate) use activity::bind_task_run_scope;
pub use activity::{
    PlannedAssignment, TaskDetails, TaskRun, TaskRunRole, TaskRunScope, TaskWorkflowState,
};
pub(crate) use checklists::bind_checklist_job_mode;
pub(crate) use checklists::progress::ChecklistProgressRequest;
pub use checklists::progress::{
    ChecklistProgressAccess, ChecklistProgressActor, ChecklistProgressAvailability,
    ChecklistProgressError, ChecklistProgressReceipt, ChecklistProgressRecord,
};
pub use checklists::{ChecklistExecutionMode, ChecklistItem, TaskChecklist};
pub use delivery::milestone::{
    DeliveryCompletionMilestone, DeliveryMilestonePolicy, PendingTaskDeliveryMerge,
};
pub(crate) use delivery::milestone::{
    delivery_merge_candidate, load_delivery_milestone_policy, record_verified_task_delivery,
    record_verified_task_merge,
};
pub(crate) use delivery::validate_repository_delivery_open;
pub use delivery::{
    TaskDelivery, TaskDeliveryEvidence, TaskDeliveryState, TaskPrerequisiteBlocker,
};
pub use descriptions::{
    DescriptionDraft, DescriptionEditorConflict, DescriptionEditorSave, DescriptionEditorState,
    DescriptionOwner, DescriptionRevision, TaskDirectWork, TaskDispatchContext, WorkDescriptions,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTask {
    pub id: String,
    pub project_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEpic {
    pub id: String,
    pub project_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum TaskPlacement {
    Task { epic_id: Option<String> },
    Subtask { parent_task_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWork {
    pub task: ProjectTask,
    pub placement: TaskPlacement,
    pub checklist_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectWorkHierarchy {
    pub epics: Vec<ProjectEpic>,
    pub work: Vec<ProjectWork>,
}

pub(crate) fn load_project_task(
    db: &Connection,
    project: &str,
    id: &str,
) -> Result<Option<ProjectTask>> {
    Ok(db
        .query_row(
            "SELECT title FROM project_tasks WHERE project_id = ?1 AND id = ?2",
            params![project, id],
            |row| {
                Ok(ProjectTask {
                    id: id.into(),
                    project_id: project.into(),
                    title: row.get(0)?,
                })
            },
        )
        .optional()?)
}

impl Store {
    pub async fn create_project_task(
        &self,
        project_id: String,
        title: String,
    ) -> Result<ProjectTask> {
        self.execute(move |db| insert_project_task(db, project_id, title))
            .await
    }

    pub async fn create_project_epic(
        &self,
        project_id: String,
        title: String,
    ) -> Result<ProjectEpic> {
        ensure!(!title.trim().is_empty(), "epic title is empty");
        let epic = ProjectEpic {
            id: Uuid::new_v4().to_string(),
            project_id,
            title,
        };
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_epics(project_id, id, title) VALUES (?1, ?2, ?3)",
                params![epic.project_id, epic.id, epic.title],
            )?;
            Ok(epic)
        })
        .await
    }

    pub async fn create_project_task_in_epic(
        &self,
        project: String,
        epic: String,
        title: String,
    ) -> Result<ProjectTask> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let task = insert_project_task(&tx, project, title)?;
            tx.execute(
                "INSERT INTO project_task_memberships(project_id, task_id, epic_id) VALUES (?1, ?2, ?3)",
                params![task.project_id, task.id, epic],
            ).context("epic must belong to the same project")?;
            tx.commit()?;
            Ok(task)
        }).await
    }

    pub async fn create_project_subtask(
        &self,
        project_id: String,
        parent_task_id: String,
        title: String,
    ) -> Result<ProjectTask> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let task = insert_project_task(&tx, project_id, title)?;
            tx.execute(
                "INSERT INTO project_task_memberships(project_id, task_id, parent_task_id)
                 VALUES (?1, ?2, ?3)",
                params![task.project_id, task.id, parent_task_id],
            )
            .context("sub-task requires a task parent in the same project")?;
            tx.commit()?;
            Ok(task)
        })
        .await
    }

    pub async fn set_project_task_epic(
        &self,
        project_id: String,
        task_id: String,
        epic_id: Option<String>,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let is_root: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM project_tasks t
                    LEFT JOIN project_task_memberships m ON m.project_id = t.project_id AND m.task_id = t.id
                    WHERE t.project_id = ?1 AND t.id = ?2 AND m.parent_task_id IS NULL
                )",
                params![project_id, task_id],
                |row| row.get(0),
            )?;
            ensure!(is_root, "only a task in the same project can belong to an epic");
            if let Some(epic_id) = epic_id {
                tx.execute(
                    "INSERT INTO project_task_memberships(project_id, task_id, epic_id) VALUES (?1, ?2, ?3)
                     ON CONFLICT(project_id, task_id) DO UPDATE SET epic_id = excluded.epic_id",
                    params![project_id, task_id, epic_id],
                ).context("epic must belong to the same project")?;
            } else {
                tx.execute(
                    "DELETE FROM project_task_memberships WHERE project_id = ?1 AND task_id = ?2",
                    params![project_id, task_id],
                )?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn ensure_project_checklist(
        &self,
        project_id: String,
        task_id: String,
    ) -> Result<String> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO project_checklists(project_id, id, task_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id, task_id) DO NOTHING",
                params![project_id, Uuid::new_v4().to_string(), task_id],
            )
            .context("checklist requires a task or sub-task in the same project")?;
            let id = tx.query_row(
                "SELECT id FROM project_checklists WHERE project_id = ?1 AND task_id = ?2",
                params![project_id, task_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    pub async fn project_work_hierarchy(&self, project_id: String) -> Result<ProjectWorkHierarchy> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let epics = tx.prepare(
                "SELECT id, title FROM project_epics WHERE project_id = ?1 ORDER BY title, id",
            )?.query_map([&project_id], |row| Ok(ProjectEpic {
                id: row.get(0)?, project_id: project_id.clone(), title: row.get(1)?,
            }))?.collect::<rusqlite::Result<_>>()?;
            let work = tx.prepare(
                "SELECT t.id, t.title, m.epic_id, m.parent_task_id, c.id FROM project_tasks t
                 LEFT JOIN project_task_memberships m ON m.project_id = t.project_id AND m.task_id = t.id
                 LEFT JOIN project_checklists c ON c.project_id = t.project_id AND c.task_id = t.id
                 WHERE t.project_id = ?1 ORDER BY t.title, t.id",
            )?.query_map([&project_id], |row| {
                let placement = match row.get(3)? {
                    Some(parent_task_id) => TaskPlacement::Subtask { parent_task_id },
                    None => TaskPlacement::Task { epic_id: row.get(2)? },
                };
                Ok(ProjectWork {
                    task: ProjectTask { id: row.get(0)?, project_id: project_id.clone(), title: row.get(1)? },
                    placement,
                    checklist_id: row.get(4)?,
                })
            })?.collect::<rusqlite::Result<_>>()?;
            Ok(ProjectWorkHierarchy { epics, work })
        }).await
    }

    pub async fn project_task(
        &self,
        project_id: String,
        id: String,
    ) -> Result<Option<ProjectTask>> {
        self.execute(move |db| load_project_task(db, &project_id, &id))
            .await
    }
}

pub(crate) fn insert_project_task(
    db: &Connection,
    project_id: String,
    title: String,
) -> Result<ProjectTask> {
    ensure!(!title.trim().is_empty(), "task title is empty");
    let task = ProjectTask {
        id: Uuid::new_v4().to_string(),
        project_id,
        title,
    };
    db.execute(
        "INSERT INTO project_tasks(project_id, id, title) VALUES (?1, ?2, ?3)",
        params![task.project_id, task.id, task.title],
    )?;
    Ok(task)
}
