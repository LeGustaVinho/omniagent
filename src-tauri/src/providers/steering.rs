use serde::{Deserialize, Serialize};

use super::{AdapterUpdate, ProviderEvent, ProviderEventData, ProviderLocation};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerTarget {
    pub session_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SteerOutcome {
    Accepted,
    StaleTurn,
    UnsupportedInput,
    Unavailable,
    Busy,
    ProviderRejected,
    Unconfirmed,
}

impl SteerTarget {
    pub(super) fn location(&self) -> ProviderLocation {
        ProviderLocation {
            session_id: Some(self.session_id.clone()),
            turn_id: Some(self.turn_id.clone()),
            ..Default::default()
        }
    }

    pub(super) fn settlement(&self, message_id: String, outcome: SteerOutcome) -> ProviderEvent {
        // Rejecting a mismatched target must never associate the active session with it.
        let location = if outcome == SteerOutcome::Accepted {
            self.location()
        } else {
            ProviderLocation::default()
        };
        ProviderEvent {
            location,
            data: ProviderEventData::SteeringSettled {
                message_id,
                target: self.clone(),
                outcome,
            },
        }
    }

    pub(super) fn rejected(&self, message_id: String, outcome: SteerOutcome) -> AdapterUpdate {
        AdapterUpdate {
            outbound: Vec::new(),
            events: vec![self.settlement(message_id, outcome)],
        }
    }
}
