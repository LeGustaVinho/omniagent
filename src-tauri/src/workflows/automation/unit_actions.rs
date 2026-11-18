use anyhow::Result;
use rusqlite::{Connection, params};

use super::{AutomaticAgentWait, WorkflowRun};

pub(in crate::workflows) fn waits(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Vec<AutomaticAgentWait>> {
    let mut statement = db.prepare(
        "SELECT node_instance_id, unit_id, coalesce(diagnostic, 'Waiting for the delivery action')
         FROM workflow_automatic_unit_actions WHERE pipeline_run_id = ?1 AND plan_key = ?2
           AND state IN ('pending', 'operator') ORDER BY node_instance_id, unit_id",
    )?;
    Ok(statement
        .query_map(params![run.id, run.plan_revision], |row| {
            Ok(AutomaticAgentWait {
                node_id: row.get(0)?,
                unit_id: Some(row.get(1)?),
                reason: Some(row.get(2)?),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}
