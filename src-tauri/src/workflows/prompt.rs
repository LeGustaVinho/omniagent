use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::ArtifactKind;

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptDecisionConfiguration {
    pub routes: Vec<String>,
    pub delegates: BTreeMap<String, bool>,
    pub output_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PromptDecision {
    pub route: String,
    pub reason: String,
    pub outputs: Map<String, Value>,
    pub selected: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptOutcomeConfiguration {
    pub outputs: BTreeMap<String, ArtifactKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptCallStatus {
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PromptCall {
    pub role: String,
    pub focus: Option<String>,
    pub status: PromptCallStatus,
    pub artifact_id: Option<String>,
    pub stage_id: String,
    pub attempt_id: Option<String>,
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PromptOutcome {
    pub route: String,
    pub reason: String,
    pub outputs: Map<String, Value>,
    pub selected: BTreeMap<String, String>,
    pub calls: Vec<PromptCall>,
    pub diagnostic: Option<String>,
    pub approved: bool,
}

pub(crate) fn stage_alias(node: &str) -> Option<&str> {
    node.rsplit("::")
        .next()?
        .strip_prefix("delegate-")
        .filter(|alias| valid_identifier(alias))
}
