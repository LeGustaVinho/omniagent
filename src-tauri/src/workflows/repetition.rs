use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ArtifactKind, ConditionPredicate};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepeatFeedback {
    pub input: String,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepeatUntilConfiguration {
    pub max_iterations: u16,
    pub ready_input: String,
    pub completion_output: String,
    pub evidence_output: String,
    pub until: ConditionPredicate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<RepeatFeedback>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_attempt_budget: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepeatOutputConfiguration {
    pub(super) rounds: u16,
    pub(super) exports: BTreeMap<String, ArtifactKind>,
}
