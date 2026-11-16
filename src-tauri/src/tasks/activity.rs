use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    conversations::SessionScope,
    model_catalog::ModelSelection,
    protection,
    providers::{Provider, SessionMode},
    runs::RunStatus,
    store::Store,
};

use super::{ChecklistExecutionMode, delivery::load_delivery_state, load_project_task};

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
pub struct TaskRunScope {
    pub role: String,
    pub checklist_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TaskRunRole {
    Workflow(String),
    Session(SessionMode),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRun {
    pub job_id: String,
    pub ordinal: u32,
    pub run_id: String,
    pub conversation_id: String,
    pub turn_id: String,
    pub context_snapshot_id: Option<String>,
    pub role: TaskRunRole,
    pub checklist_item_id: Option<String>,
    pub checklist_mode: Option<ChecklistExecutionMode>,
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
    pub async fn set_project_task_assignment(
        &self,
        project: String,
        task: String,
        role: String,
        selection: Option<ModelSelection>,
    ) -> Result<Vec<PlannedAssignment>> {
        ensure!(!role.trim().is_empty(), "an assignment requires a role");
        ensure!(
            selection.as_ref().is_none_or(|selection| {
                !selection.profile_id.trim().is_empty() && !selection.model_id.trim().is_empty()
            }),
            "an assignment requires a profile and model"
        );
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            load_project_task(&tx, &project, &task)?.context("task is outside the selected project")?;
            let mut details = load_task_details(&tx, &project, &task)?;
            details.planned_assignments.retain(|saved| saved.role != role);
            if let Some(selection) = selection {
                details.planned_assignments.push(PlannedAssignment {
                    role,
                    provider: selection.provider,
                    profile_id: selection.profile_id,
                    model: Some(selection.model_id),
                });
            }
            let payload = ProtectedTaskDetails {
                acceptance_criteria: details.acceptance_criteria,
                blockers: details.blockers,
                planned_assignments: details.planned_assignments.clone(),
            };
            tx.execute(
                "INSERT INTO project_task_details(project_id, task_id, priority, workflow_state, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(project_id, task_id) DO UPDATE SET payload = excluded.payload",
                params![
                    project,
                    task,
                    details.priority,
                    details.workflow_state.key(),
                    protection::protect(&serde_json::to_vec(&payload)?)?
                ],
            )?;
            tx.commit()?;
            Ok(details.planned_assignments)
        }).await
    }

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
            let mut details = load_task_details(&tx, &project, &task)?;
            details.workflow_state = load_delivery_state(&tx, &project, &task)?.state;
            Ok(Some(details))
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
            let effective = load_delivery_state(&tx, &project, &task)?.state;
            let terminal = |state| matches!(state, TaskWorkflowState::Completed | TaskWorkflowState::Cancelled);
            let workflow_state = if terminal(effective) || terminal(details.workflow_state) {
                ensure!(
                    details.workflow_state == effective,
                    "use the task delivery actions to complete, cancel, or reopen work"
                );
                load_task_details(&tx, &project, &task)?.workflow_state
            } else {
                details.workflow_state
            };
            tx.execute(
                "INSERT INTO project_task_details(project_id, task_id, priority, workflow_state, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(project_id, task_id) DO UPDATE SET priority = excluded.priority,
                     workflow_state = excluded.workflow_state, payload = excluded.payload",
                params![project, task, details.priority, workflow_state.key(), encrypted],
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

    pub async fn project_task_runs(&self, project: String, task: String) -> Result<Vec<TaskRun>> {
        self.task_runs_for(project, task, None).await
    }

    pub async fn project_checklist_item_runs(
        &self,
        project: String,
        task: String,
        item: String,
    ) -> Result<Vec<TaskRun>> {
        self.task_runs_for(project, task, Some(item)).await
    }

    async fn task_runs_for(
        &self,
        project: String,
        task: String,
        item: Option<String>,
    ) -> Result<Vec<TaskRun>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT t.job_id, t.ordinal, t.run_id, t.turn_id, t.snapshot_id, t.role, t.checklist_item_id,
                        t.scope, t.reported_model, t.status, t.created_at, t.closed_at,
                        (SELECT min(recorded_at) FROM run_events e WHERE e.turn_id = t.turn_id
                         AND json_extract(e.payload, '$.type') = 'status'
                         AND json_extract(e.payload, '$.status') = 'running'), execution.mode,
                        attempt.conversation_id
                 FROM task_runs t
                 JOIN agent_attempts attempt ON attempt.run_id = t.run_id
                 LEFT JOIN job_checklist_modes execution ON execution.job_id = t.job_id
                 WHERE t.project_id = ?1 AND t.task_id = ?2
                 AND (?3 IS NULL OR t.checklist_item_id = ?3)
                 ORDER BY t.created_at, t.job_id, t.ordinal",
            )?;
            let mut rows = query.query(params![project, task, item])?;
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
                    conversation_id: row.get(14)?,
                    turn_id: row.get(3)?,
                    context_snapshot_id: row.get(4)?,
                    role,
                    checklist_item_id: row.get(6)?,
                    checklist_mode: row
                        .get::<_, Option<String>>(13)?
                        .as_deref()
                        .map(ChecklistExecutionMode::from_sql)
                        .transpose()?,
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

pub(super) fn load_task_details(db: &Connection, project: &str, task: &str) -> Result<TaskDetails> {
    let saved: Option<(i32, String, Vec<u8>)> = db
        .query_row(
            "SELECT priority, workflow_state, payload FROM project_task_details
             WHERE project_id = ?1 AND task_id = ?2",
            params![project, task],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((priority, state, encrypted)) = saved else {
        return Ok(TaskDetails::default());
    };
    let protected: ProtectedTaskDetails =
        serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
    let prerequisites = db
        .prepare(
            "SELECT prerequisite_id FROM project_task_prerequisites
             WHERE project_id = ?1 AND task_id = ?2 ORDER BY prerequisite_id",
        )?
        .query_map(params![project, task], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(TaskDetails {
        priority,
        workflow_state: serde_json::from_value(serde_json::Value::String(state))?,
        prerequisites,
        acceptance_criteria: protected.acceptance_criteria,
        blockers: protected.blockers,
        planned_assignments: protected.planned_assignments,
    })
}

pub(super) fn store_requested_workflow_state(
    db: &Connection,
    project: &str,
    task: &str,
    state: TaskWorkflowState,
) -> Result<()> {
    let changed = db.execute(
        "UPDATE project_task_details SET workflow_state = ?3 WHERE project_id = ?1 AND task_id = ?2",
        params![project, task, state.key()],
    )?;
    if changed == 0 {
        let payload = ProtectedTaskDetails {
            acceptance_criteria: vec![],
            blockers: vec![],
            planned_assignments: vec![],
        };
        db.execute(
            "INSERT INTO project_task_details(project_id, task_id, priority, workflow_state, payload)
             VALUES (?1, ?2, 0, ?3, ?4)",
            params![project, task, state.key(), protection::protect(&serde_json::to_vec(&payload)?)?],
        )?;
    }
    Ok(())
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
