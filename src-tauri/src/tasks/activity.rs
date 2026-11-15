use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    conversations::SessionScope,
    protection,
    providers::{Provider, SessionMode},
    runs::RunStatus,
    store::Store,
};

use super::load_project_task;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskWorkflowState {
    #[default]
    Backlog,
    Ready,
    InProgress,
    InReview,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl TaskWorkflowState {
    fn key(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Ready => "ready",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAssignment {
    pub role: String,
    pub provider: Provider,
    pub profile_id: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDetails {
    pub priority: i32,
    pub workflow_state: TaskWorkflowState,
    pub prerequisites: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub blockers: Vec<String>,
    pub planned_assignments: Vec<PlannedAssignment>,
}

#[derive(Serialize, Deserialize)]
struct ProtectedTaskDetails {
    acceptance_criteria: Vec<String>,
    blockers: Vec<String>,
    planned_assignments: Vec<PlannedAssignment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistItem {
    pub id: String,
    pub position: i64,
    pub revision: i64,
    pub body: String,
    pub checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRunScope {
    pub role: String,
    pub checklist_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRunRole {
    Workflow(String),
    Session(SessionMode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRun {
    pub job_id: String,
    pub ordinal: u32,
    pub run_id: String,
    pub turn_id: String,
    pub context_snapshot_id: Option<String>,
    pub role: TaskRunRole,
    pub checklist_item_id: Option<String>,
    pub provider: Provider,
    pub profile_id: String,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    pub status: RunStatus,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

impl Store {
    pub async fn project_task_details(
        &self,
        project: String,
        task: String,
    ) -> Result<Option<TaskDetails>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            if load_project_task(&tx, &project, &task)?.is_none() {
                return Ok(None);
            }
            let saved: Option<(i32, String, Vec<u8>)> = tx
                .query_row(
                    "SELECT priority, workflow_state, payload FROM project_task_details
                     WHERE project_id = ?1 AND task_id = ?2",
                    params![project, task],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((priority, state, encrypted)) = saved else {
                return Ok(Some(TaskDetails::default()));
            };
            let protected: ProtectedTaskDetails =
                serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
            let prerequisites = tx
                .prepare(
                    "SELECT prerequisite_id FROM project_task_prerequisites
                     WHERE project_id = ?1 AND task_id = ?2 ORDER BY prerequisite_id",
                )?
                .query_map(params![project, task], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(Some(TaskDetails {
                priority,
                workflow_state: serde_json::from_value(serde_json::Value::String(state))?,
                prerequisites,
                acceptance_criteria: protected.acceptance_criteria,
                blockers: protected.blockers,
                planned_assignments: protected.planned_assignments,
            }))
        })
        .await
    }

    pub async fn set_project_task_details(
        &self,
        project: String,
        task: String,
        mut details: TaskDetails,
    ) -> Result<()> {
        details.prerequisites.sort_unstable();
        details.prerequisites.dedup();
        ensure!(
            details.planned_assignments.iter().all(|assignment| {
                !assignment.role.trim().is_empty() && !assignment.profile_id.trim().is_empty()
            }),
            "a planned assignment requires a role and profile"
        );
        let details_payload = ProtectedTaskDetails {
            acceptance_criteria: details.acceptance_criteria,
            blockers: details.blockers,
            planned_assignments: details.planned_assignments,
        };
        let encrypted = protection::protect(&serde_json::to_vec(&details_payload)?)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO project_task_details(project_id, task_id, priority, workflow_state, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(project_id, task_id) DO UPDATE SET priority = excluded.priority,
                     workflow_state = excluded.workflow_state, payload = excluded.payload",
                params![project, task, details.priority, details.workflow_state.key(), encrypted],
            ).context("task details require an actionable record in this project")?;
            tx.execute(
                "DELETE FROM project_task_prerequisites WHERE project_id = ?1 AND task_id = ?2",
                params![project, task],
            )?;
            for prerequisite in details.prerequisites {
                tx.execute(
                    "INSERT INTO project_task_prerequisites VALUES (?1, ?2, ?3)",
                    params![project, task, prerequisite],
                ).context("prerequisites must be other tasks in the same project")?;
            }
            let cycle: bool = tx.query_row(
                "WITH RECURSIVE dependencies(id) AS (
                     SELECT prerequisite_id FROM project_task_prerequisites
                     WHERE project_id = ?1 AND task_id = ?2
                     UNION
                     SELECT p.prerequisite_id FROM project_task_prerequisites p
                     JOIN dependencies d ON d.id = p.task_id WHERE p.project_id = ?1
                 ) SELECT EXISTS (SELECT 1 FROM dependencies WHERE id = ?2)",
                params![project, task],
                |row| row.get(0),
            )?;
            ensure!(!cycle, "the prerequisites contain a cycle");
            tx.commit()?;
            Ok(())
        }).await
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

    pub async fn project_task_runs(&self, project: String, task: String) -> Result<Vec<TaskRun>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT job_id, ordinal, run_id, turn_id, snapshot_id, role, checklist_item_id,
                        scope, reported_model, status, created_at, closed_at,
                        (SELECT min(recorded_at) FROM run_events e WHERE e.turn_id = t.turn_id
                         AND json_extract(e.payload, '$.type') = 'status'
                         AND json_extract(e.payload, '$.status') = 'running')
                 FROM task_runs t WHERE project_id = ?1 AND task_id = ?2
                 ORDER BY created_at, job_id, ordinal",
            )?;
            let mut rows = query.query(params![project, task])?;
            let mut runs = Vec::new();
            while let Some(row) = rows.next()? {
                let scope: SessionScope = serde_json::from_str(row.get_ref(7)?.as_str()?)?;
                let role = row
                    .get::<_, Option<String>>(5)?
                    .map_or(TaskRunRole::Session(scope.mode), TaskRunRole::Workflow);
                runs.push(TaskRun {
                    job_id: row.get(0)?,
                    ordinal: row.get(1)?,
                    run_id: row.get(2)?,
                    turn_id: row.get(3)?,
                    context_snapshot_id: row.get(4)?,
                    role,
                    checklist_item_id: row.get(6)?,
                    provider: scope.provider,
                    profile_id: scope.profile_id,
                    requested_model: scope.model,
                    reported_model: row.get(8)?,
                    status: RunStatus::from_sql(row.get_ref(9)?.as_str()?)?,
                    created_at: row.get(10)?,
                    finished_at: row.get(11)?,
                    started_at: row.get(12)?,
                });
            }
            Ok(runs)
        })
        .await
    }
}

pub(crate) fn bind_task_run_scope(
    db: &Connection,
    project: &str,
    job: &str,
    task: Option<&str>,
    target: &TaskRunScope,
) -> Result<()> {
    ensure!(!target.role.trim().is_empty(), "a task run requires a role");
    let task = task.context("a task run requires an actionable work scope")?;
    db.execute(
        "INSERT INTO job_task_run_scopes (job_id, project_id, task_id, role, checklist_item_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![job, project, task, target.role, target.checklist_item_id],
    )
    .context("the checklist item must belong to the job's task or sub-task")?;
    Ok(())
}
