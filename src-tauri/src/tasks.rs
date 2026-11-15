use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTask {
    pub id: String,
    pub project_id: String,
    pub title: String,
}

impl Store {
    pub async fn create_project_task(
        &self,
        project_id: String,
        title: String,
    ) -> Result<ProjectTask> {
        ensure!(!title.trim().is_empty(), "task title is empty");
        let task = ProjectTask {
            id: Uuid::new_v4().to_string(),
            project_id,
            title,
        };
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_tasks(project_id, id, title) VALUES (?1, ?2, ?3)",
                params![task.project_id, task.id, task.title],
            )?;
            Ok(task)
        })
        .await
    }

    pub async fn project_task(
        &self,
        project_id: String,
        id: String,
    ) -> Result<Option<ProjectTask>> {
        self.execute(move |db| {
            Ok(db
                .query_row(
                    "SELECT title FROM project_tasks WHERE project_id = ?1 AND id = ?2",
                    params![project_id, id],
                    |row| row.get(0),
                )
                .optional()?
                .map(|title| ProjectTask {
                    id,
                    project_id,
                    title,
                }))
        })
        .await
    }
}
