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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{Provider, SessionIntent, SessionMode, SessionRequest, TurnInput};

    #[test]
    fn an_unqualified_transport_retains_the_instruction_for_app_routing() -> anyhow::Result<()> {
        let checkout = tempfile::tempdir()?;
        let target = SteerTarget {
            session_id: "session".into(),
            turn_id: "active".into(),
        };
        for provider in [Provider::Claude, Provider::Antigravity] {
            let mut session = provider.adapter().create_session(SessionRequest {
                checkout: checkout.path().into(),
                intent: SessionIntent::Start,
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            })?;
            let candidate = TurnInput {
                message_id: "draft-revision".into(),
                text: "Follow-up instruction".into(),
                attachments: Vec::new(),
            };
            let update = session.steer(target.clone(), candidate)?;
            assert!(update.outbound.is_empty());
            assert!(matches!(update.events.as_slice(), [ProviderEvent {
                data: ProviderEventData::SteeringSettled {
                    message_id, target: intended, outcome: SteerOutcome::Unavailable,
                }, location
            }] if message_id == "draft-revision" && intended == &target && location.turn_id.is_none()));
        }
        Ok(())
    }
}
