use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::repository::verification::SelectedCandidate;

use super::{ArtifactValue, WorkflowArtifact};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitInspection {
    pub candidate: SelectedCandidate,
    pub captured_head: String,
    pub status: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiffReport {
    pub git_inspection_artifact_id: String,
    pub candidate_id: String,
    pub review_run_id: String,
    pub checks_id: String,
    pub required_checks_passed: bool,
}

pub(in crate::workflows) fn inspection(artifact: &WorkflowArtifact) -> Result<&GitInspection> {
    let ArtifactValue::GitInspection(git) = &artifact.value else {
        anyhow::bail!("The stage requires the frozen Git inspection");
    };
    Ok(git)
}
