use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::workflows::{Endpoint, WorkflowRevision, WorkflowRun};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunBacklogConfiguration {
    pub child: WorkflowRevision,
    pub max_parallel: u16,
    pub failed: Endpoint,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklogExecution {
    node_id: String,
    backlog_artifact_id: String,
    state: String,
    reason: Option<String>,
    tasks: Vec<BacklogChild>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BacklogChild {
    key: String,
    task_id: String,
    workflow_run_id: String,
    #[serde(skip_serializing)]
    destination_branch: String,
}

pub(crate) fn inspect(db: &Connection, run: &WorkflowRun) -> Result<Vec<BacklogExecution>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, backlog_artifact_id, state, reason FROM workflow_backlog_executions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 ORDER BY node_instance_id",
    )?;
    let mut executions = query
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or_default()],
            |row| {
                Ok(BacklogExecution {
                    node_id: row.get(0)?,
                    backlog_artifact_id: row.get(1)?,
                    state: row.get(2)?,
                    reason: row.get(3)?,
                    tasks: Vec::new(),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut tasks = db.prepare(
        "SELECT task_key, task_id, child_run_id, destination_branch FROM workflow_backlog_task_runs
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND backlog_artifact_id = ?3 ORDER BY task_key",
    )?;
    for execution in &mut executions {
        execution.tasks = tasks
            .query_map(
                params![run.id, execution.node_id, execution.backlog_artifact_id],
                |row| {
                    Ok(BacklogChild {
                        key: row.get(0)?,
                        task_id: row.get(1)?,
                        workflow_run_id: row.get(2)?,
                        destination_branch: row.get(3)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(executions)
}
