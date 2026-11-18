use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ArtifactKind;

pub const MAX_FOR_EACH_ITEMS: u16 = 32;
const MAX_LIST_BYTES: usize = 192 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildListConfiguration {
    Values { values: Vec<Value> },
    Evidence { pointer: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachConfiguration {
    pub max_items: u16,
    pub ready_input: String,
    pub item_input: String,
    pub completion_output: String,
    pub result_output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachBeginConfiguration {
    pub max_items: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachGateConfiguration {
    pub ordinal: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachCollectConfiguration {
    pub max_items: u16,
    pub result_kind: ArtifactKind,
}

fn valid_limit(limit: u16) -> bool {
    (1..=MAX_FOR_EACH_ITEMS).contains(&limit)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowJsonList {
    pub values: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_artifact_id: Option<String>,
}

impl WorkflowJsonList {
    pub fn new(values: Vec<Value>, source_artifact_id: Option<String>) -> Result<Self> {
        let list = Self {
            values,
            source_artifact_id,
        };
        list.validate()?;
        Ok(list)
    }

    pub(super) fn validate(&self) -> Result<()> {
        validate_values(&self.values)
    }
}

fn validate_values(values: &[Value]) -> Result<()> {
    ensure!(
        values.len() <= usize::from(MAX_FOR_EACH_ITEMS),
        "List exceeds the maximum supported item count"
    );
    ensure!(
        serde_json::to_vec(values)?.len() <= MAX_LIST_BYTES,
        "List exceeds the workflow data size limit"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowJsonItem {
    pub list_artifact_id: String,
    pub ordinal: u16,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowIterationResults {
    pub list_artifact_id: String,
    pub result_kind: ArtifactKind,
    pub results: Vec<String>,
}
