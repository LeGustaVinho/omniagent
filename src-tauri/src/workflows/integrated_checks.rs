use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::WorkflowRun;
use crate::repository::verification::{
    integrated_checks::IntegratedCheckResult, test_phase::TestVerdict,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegratedRevision {
    pub destination_branch: String,
    pub sha: String,
    pub integration_shas: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegratedChecksConfiguration {
    pub checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratedChecksEvidence {
    pub integrated_artifact_id: String,
    pub revision: IntegratedRevision,
    pub policy_revision: String,
    pub tree: Option<String>,
    pub verdict: TestVerdict,
    pub reason: Option<String>,
    pub checks: Vec<IntegratedCheckResult>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratedChecksWait {
    node_id: String,
    reason: Option<String>,
}

pub(super) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<IntegratedChecksWait>> {
    let mut statement = db.prepare(
        "SELECT node_instance_id, reason FROM workflow_integrated_check_actions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND output_id IS NULL
         ORDER BY node_instance_id, request_key",
    )?;
    Ok(statement
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or_default()],
            |row| {
                Ok(IntegratedChecksWait {
                    node_id: row.get(0)?,
                    reason: row.get(1)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?)
}
