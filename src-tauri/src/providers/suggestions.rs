use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ProviderEvent, ProviderEventData, ProviderLocation};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionSource {
    pub provider_session_id: String,
    pub provider_turn_id: Option<String>,
    pub provider_result_id: Option<String>,
    pub message_id: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuggestionOutcome {
    Available { text: String },
    Empty,
    Failed,
    Unavailable,
    TimedOut,
    Cancelled,
}

impl SuggestionOutcome {
    pub(super) fn from_text(text: &Value) -> Self {
        match text {
            Value::String(text) if !text.trim().is_empty() => Self::Available {
                text: text.trim().into(),
            },
            Value::String(_) | Value::Null => Self::Empty,
            _ => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSuggestion {
    pub source: SuggestionSource,
    pub outcome: SuggestionOutcome,
}

impl SuggestionSource {
    pub(super) fn settlement(self, outcome: SuggestionOutcome) -> ProviderEvent {
        ProviderEvent {
            location: ProviderLocation {
                session_id: Some(self.provider_session_id.clone()),
                turn_id: self.provider_turn_id.clone(),
                ..Default::default()
            },
            data: ProviderEventData::PromptSuggestion {
                suggestion: PromptSuggestion {
                    source: self,
                    outcome,
                },
            },
        }
    }
}
