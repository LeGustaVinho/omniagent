use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::diagnostics;

use super::{
    CandidateGate, CandidateGateAction, CandidateGateOutcome, PinnedGate, command, load_policy,
    materialize::ScratchGit, test_phase::TestVerdict,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratedCheckResult {
    pub name: String,
    pub executable_revision: Option<String>,
    pub outcome: CandidateGateOutcome,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct IntegratedGatePlan {
    pub policy_revision: String,
    gates: Vec<PinnedGate>,
}

pub(crate) struct IntegratedRun {
    pub tree: Option<String>,
    pub verdict: TestVerdict,
    pub reason: Option<String>,
    pub checks: Vec<IntegratedCheckResult>,
}

impl IntegratedGatePlan {
    pub(crate) fn load(db: &Connection, project: &str, names: &[String]) -> Result<Self> {
        let (policy_revision, registered) = load_policy(db, project)?;
        let gates = names
            .iter()
            .map(|name| {
                registered
                    .iter()
                    .find(|command| command.gate.name == *name)
                    .cloned()
                    .unwrap_or_else(|| PinnedGate {
                        gate: CandidateGate {
                            name: name.clone(),
                            required: true,
                            action: CandidateGateAction::Unavailable {
                                reason: format!(
                                    "No command is configured for required check {name}"
                                ),
                            },
                        },
                        executable_revision: None,
                        deferred: false,
                    })
            })
            .collect();
        Ok(Self {
            policy_revision,
            gates,
        })
    }

    pub(crate) fn matches_names(&self, names: &[String]) -> bool {
        self.gates.iter().map(|gate| &gate.gate.name).eq(names)
    }

    pub(crate) fn is_current(&self, db: &Connection, project: &str) -> Result<bool> {
        let (revision, _) = load_policy(db, project)?;
        Ok(revision == self.policy_revision)
    }

    pub(crate) fn blocked(&self, reason: &str) -> IntegratedRun {
        let reason = diagnostics::preview(reason);
        IntegratedRun {
            tree: None,
            verdict: TestVerdict::Blocked,
            checks: self
                .gates
                .iter()
                .map(|gate| {
                    result(
                        gate,
                        CandidateGateOutcome::CouldNotRun {
                            reason: reason.clone(),
                        },
                    )
                })
                .collect(),
            reason: Some(reason),
        }
    }

    pub(crate) fn run(&self, common_dir: &Path, commit: &str) -> IntegratedRun {
        let mut tree = None;
        let checks = self
            .gates
            .iter()
            .map(|gate| {
                let outcome = match &gate.gate.action {
                    CandidateGateAction::Unavailable { reason } => {
                        CandidateGateOutcome::CouldNotRun {
                            reason: diagnostics::preview(reason),
                        }
                    }
                    CandidateGateAction::Command { .. } => {
                        match ScratchGit::new(common_dir, commit)
                            .and_then(|git| git.materialize_commit(commit))
                        {
                            Ok((checkout, materialized_tree)) => {
                                tree.get_or_insert(materialized_tree);
                                command::run_in_checkout(
                                    checkout.path(),
                                    &gate.gate.action,
                                    gate.executable_revision.as_deref(),
                                )
                            }
                            Err(error) => CandidateGateOutcome::CouldNotRun {
                                reason: diagnostics::preview(&format!(
                                    "Could not materialize the exact integrated commit: {error:#}"
                                )),
                            },
                        }
                    }
                };
                result(gate, outcome)
            })
            .collect::<Vec<_>>();
        let failed = checks
            .iter()
            .find(|check| matches!(check.outcome, CandidateGateOutcome::Failed { .. }));
        let blocked = checks.iter().find(|check| {
            matches!(
                check.outcome,
                CandidateGateOutcome::CouldNotRun { .. } | CandidateGateOutcome::Interrupted { .. }
            )
        });
        let (verdict, reason) = if let Some(check) = failed {
            (
                TestVerdict::Failed,
                Some(format!("Required check {} failed", check.name)),
            )
        } else if let Some(check) = blocked {
            (
                TestVerdict::Blocked,
                Some(format!("Required check {} could not finish", check.name)),
            )
        } else {
            (TestVerdict::Verified, None)
        };
        IntegratedRun {
            tree,
            verdict,
            reason,
            checks,
        }
    }
}

fn result(gate: &PinnedGate, outcome: CandidateGateOutcome) -> IntegratedCheckResult {
    IntegratedCheckResult {
        name: gate.gate.name.clone(),
        executable_revision: gate.executable_revision.clone(),
        outcome,
    }
}
