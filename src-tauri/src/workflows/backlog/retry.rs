use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BacklogCiRepair {
    pub attempt: u16,
    pub previous_plan_revision: String,
    pub failed_ci_artifact_id: String,
    pub previous_source_sha: String,
    pub destination_sha: String,
}
