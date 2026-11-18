use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::super::{NodeInputs, WorkflowRun};
use super::ComparisonSelection;
use crate::{model_catalog::ModelSelection, protection};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowComparisonConfiguration {
    pub selections: Vec<ModelSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowComparisonResults {
    pub group_id: String,
    pub commit: String,
    pub tree: String,
    pub state: WorkflowComparisonOutcome,
    pub candidates: Vec<WorkflowComparisonCandidate>,
}

impl WorkflowComparisonResults {
    pub fn successful_selection(&self) -> ComparisonSelection {
        ComparisonSelection {
            group_id: self.group_id.clone(),
            run_ids: self
                .candidates
                .iter()
                .filter_map(|candidate| {
                    candidate.response_revision_id.as_ref()?;
                    candidate.run_id.clone()
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowComparisonOutcome {
    Succeeded,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowComparisonCandidate {
    pub selection: ModelSelection,
    pub job_id: String,
    pub run_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub response_revision_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowComparisonStage {
    pub node_id: String,
    pub node_revision: String,
    pub unit_id: Option<String>,
    pub plan_revision: Option<String>,
    pub inputs: NodeInputs,
    pub group_id: Option<String>,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StagePin {
    node_revision: String,
    inputs: NodeInputs,
    unit_id: Option<String>,
    plan_revision: Option<String>,
}

pub(crate) fn load_stages(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Vec<WorkflowComparisonStage>> {
    db.prepare(
        "SELECT node_instance_id, payload, group_id, state, error FROM workflow_comparison_stages
         WHERE pipeline_run_id = ?1 ORDER BY node_instance_id, execution_key",
    )?
    .query_map([&run.id], |row| {
        Ok((
            row.get(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?
    .map(|row| {
        let (node_id, payload, group_id, state, error) = row?;
        let pin: StagePin = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        Ok(WorkflowComparisonStage {
            node_id,
            node_revision: pin.node_revision,
            unit_id: pin.unit_id,
            plan_revision: pin.plan_revision,
            inputs: pin.inputs,
            group_id,
            state,
            error,
        })
    })
    .collect()
}
