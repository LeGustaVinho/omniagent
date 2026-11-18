use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::WorkflowRun;
use crate::repository::verification::test_phase::{TestCheckResult, TestPhase, TestSpecification};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestPhaseEvidence {
    pub phase: TestPhase,
    pub base_commit: String,
    pub release_id: String,
    pub check: String,
    pub policy_revision: String,
    pub candidate_id: Option<String>,
    pub prior_artifact_id: Option<String>,
    pub specification: Option<TestSpecification>,
    pub result: TestCheckResult,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestPhaseWait {
    node_id: String,
    unit_id: String,
    reason: Option<String>,
}

pub(super) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<TestPhaseWait>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, unit_id, reason FROM workflow_test_phase_actions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND output_id IS NULL
         ORDER BY node_instance_id, unit_id",
    )?;
    let waits = query
        .query_map(params![run.id, run.plan_revision], |row| {
            Ok(TestPhaseWait {
                node_id: row.get(0)?,
                unit_id: row.get(1)?,
                reason: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(waits)
}
