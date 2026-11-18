use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;

use super::WorkflowRun;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticPreparationWait {
    node_id: String,
    reason: Option<String>,
}

pub(crate) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<AutomaticPreparationWait>> {
    let mut statement = db.prepare(
        "SELECT p.node_instance_id, p.reason FROM workflow_automatic_preparations p
         JOIN workflow_delivery_heads h ON h.pipeline_run_id = p.pipeline_run_id AND h.decision_artifact_id = p.decision_id
         WHERE p.pipeline_run_id = ?1 AND p.plan_key = ?2 AND p.worktree_artifact_id IS NULL
         ORDER BY p.node_instance_id",
    )?;
    let waits = statement
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or_default()],
            |row| {
                Ok(AutomaticPreparationWait {
                    node_id: row.get(0)?,
                    reason: row.get(1)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(waits)
}
