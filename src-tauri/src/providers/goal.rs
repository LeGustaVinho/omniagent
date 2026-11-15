use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeGoalAction {
    Start,
    Status,
    Pause,
    Stop,
    Resume,
    Cancel,
}

pub enum NativeGoalCommand {
    Start {
        objective: String,
        token_budget: Option<u64>,
    },
    Status,
    Pause,
    Stop,
    Resume,
    Cancel,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeGoalState {
    Active,
    Paused,
    Blocked,
    BudgetLimited,
    UsageLimited,
    Completed,
    Cancelled,
    Cleared,
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeGoalEvaluation {
    pub verdict: Option<String>,
    pub reason: Option<String>,
    pub evaluator: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeGoalObservation {
    pub provider_session_id: String,
    pub provider_goal_id: Option<String>,
    pub provider_turn_id: Option<String>,
    pub state: NativeGoalState,
    pub reported_state: Option<String>,
    pub tokens_used: Option<u64>,
    pub token_budget: Option<u64>,
    pub elapsed_seconds: Option<u64>,
    pub evaluation: Option<NativeGoalEvaluation>,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedGoalReason {
    Unavailable,
    Unqualified,
    StaleEvidence,
    IncompleteLifecycle,
    TokenBudgetUnverified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeGoalSupport {
    Qualified {
        controls: BTreeSet<NativeGoalAction>,
        evidence_id: String,
    },
    Managed {
        reason: ManagedGoalReason,
    },
}

impl NativeGoalSupport {
    pub(super) fn from_advertisement(advertised: Option<bool>) -> Self {
        Self::Managed {
            reason: match advertised {
                Some(true) => ManagedGoalReason::IncompleteLifecycle,
                Some(false) => ManagedGoalReason::Unavailable,
                None => ManagedGoalReason::Unqualified,
            },
        }
    }
}

pub struct NativeGoalEvidence {
    // A discovered command does not establish working lifecycle controls.
    pub scope_key: String,
    pub evidence_id: String,
    pub controls: BTreeSet<NativeGoalAction>,
    pub token_budget_verified: bool,
}

impl NativeGoalEvidence {
    pub fn resolve(&self, effective_scope: &str, requires_token_budget: bool) -> NativeGoalSupport {
        use NativeGoalAction::*;

        let reason = if self.scope_key != effective_scope {
            ManagedGoalReason::StaleEvidence
        } else if self.evidence_id.is_empty() {
            ManagedGoalReason::Unqualified
        } else if [Start, Status, Resume, Cancel]
            .iter()
            .any(|action| !self.controls.contains(action))
            || !(self.controls.contains(&Pause) || self.controls.contains(&Stop))
        {
            ManagedGoalReason::IncompleteLifecycle
        } else if requires_token_budget && !self.token_budget_verified {
            ManagedGoalReason::TokenBudgetUnverified
        } else {
            return NativeGoalSupport::Qualified {
                controls: self.controls.clone(),
                evidence_id: self.evidence_id.clone(),
            };
        };
        NativeGoalSupport::Managed { reason }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_lifecycle_budget_and_scope_before_choosing_the_native_route() {
        use NativeGoalAction::*;

        let mut evidence = NativeGoalEvidence {
            scope_key: "effective-profile".into(),
            evidence_id: "qualification-1".into(),
            controls: BTreeSet::from([Start]),
            token_budget_verified: false,
        };
        assert_eq!(
            evidence.resolve("effective-profile", false),
            NativeGoalSupport::Managed {
                reason: ManagedGoalReason::IncompleteLifecycle,
            }
        );
        evidence.controls.extend([Status, Stop, Resume, Cancel]);
        assert_eq!(
            evidence.resolve("changed-model", false),
            NativeGoalSupport::Managed {
                reason: ManagedGoalReason::StaleEvidence,
            }
        );
        assert_eq!(
            evidence.resolve("effective-profile", true),
            NativeGoalSupport::Managed {
                reason: ManagedGoalReason::TokenBudgetUnverified,
            }
        );
        assert!(matches!(
            evidence.resolve("effective-profile", false),
            NativeGoalSupport::Qualified { .. }
        ));
        evidence.token_budget_verified = true;
        assert!(matches!(
            evidence.resolve("effective-profile", true),
            NativeGoalSupport::Qualified { .. }
        ));
    }
}
