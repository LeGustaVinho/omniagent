use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{
    BuiltinNode, Endpoint, NodeConfiguration, NodeInstance, Wire, WorkflowGraph, WorkflowRun,
};

pub(super) const BUILTIN_DELIVERY_ID: &str = "builtin-task-delivery";
pub(super) const MAINTENANCE_ACTIVATION_ID: &str = "activate-maintenance";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryRoles {
    pub planner: String,
    pub plan_reviewer: String,
    pub implementer: String,
    pub candidate_reviewer: String,
}

impl Default for DeliveryRoles {
    fn default() -> Self {
        Self {
            planner: "planner".into(),
            plan_reviewer: "plan reviewer".into(),
            implementer: "implementer".into(),
            candidate_reviewer: "candidate reviewer".into(),
        }
    }
}

pub fn builtin_delivery_graph(roles: DeliveryRoles) -> WorkflowGraph {
    use BuiltinNode::*;
    use NodeConfiguration::{AgentRole, None};
    let nodes = [
        ("start", Start, None),
        ("context", LoadContext, None),
        (
            "plan",
            Plan,
            AgentRole {
                role: roles.planner,
                model: Option::None,
                requirements: Default::default(),
            },
        ),
        (
            "review-plan",
            ReviewPlan,
            AgentRole {
                role: roles.plan_reviewer,
                model: Option::None,
                requirements: Default::default(),
            },
        ),
        ("work", LinkWork, None),
        ("approve", ApproveImplementation, None),
        ("prepare", PrepareWorktree, None),
        (
            "implement",
            Implement,
            AgentRole {
                role: roles.implementer,
                model: Option::None,
                requirements: Default::default(),
            },
        ),
        (
            "review-candidate",
            ReviewCandidate,
            AgentRole {
                role: roles.candidate_reviewer,
                model: Option::None,
                requirements: Default::default(),
            },
        ),
        (
            "verify-candidate",
            VerifyCandidate,
            NodeConfiguration::RequiredChecks {
                names: Vec::new(),
                parallel_groups: Vec::new(),
            },
        ),
        ("accept", AcceptCandidate, None),
        ("commit", VerifyCommit, None),
        ("merge", VerifyMerge, None),
        ("result", Result, None),
    ]
    .into_iter()
    .map(|(id, kind, configuration)| NodeInstance {
        id: id.into(),
        kind,
        configuration,
    })
    .collect();
    let wires = [
        ("start", "start", "context", "ready"),
        ("start", "document", "context", "document"),
        ("context", "loaded", "plan", "ready"),
        ("context", "context", "plan", "context"),
        ("plan", "planned", "review-plan", "ready"),
        ("plan", "plan", "review-plan", "plan"),
        ("review-plan", "reviewed", "work", "ready"),
        ("review-plan", "review", "work", "review"),
        ("plan", "plan", "work", "plan"),
        ("work", "linked", "approve", "ready"),
        ("work", "task", "approve", "task"),
        ("plan", "plan", "approve", "plan"),
        ("approve", "approved", "prepare", "ready"),
        ("approve", "decision", "prepare", "decision"),
        ("work", "task", "prepare", "task"),
        ("prepare", "prepared", "implement", "ready"),
        ("prepare", "worktree", "implement", "worktree"),
        ("prepare", "release", "implement", "release"),
        ("plan", "plan", "implement", "plan"),
        ("implement", "implemented", "review-candidate", "ready"),
        ("implement", "candidate", "review-candidate", "candidate"),
        ("implement", "implemented", "verify-candidate", "ready"),
        ("implement", "candidate", "verify-candidate", "candidate"),
        ("verify-candidate", "verified", "accept", "ready"),
        ("verify-candidate", "checks", "accept", "checks"),
        ("review-candidate", "review", "accept", "review"),
        ("implement", "candidate", "accept", "candidate"),
        ("accept", "accepted", "commit", "ready"),
        ("accept", "acceptance", "commit", "acceptance"),
        ("implement", "candidate", "commit", "candidate"),
        ("commit", "verified", "merge", "ready"),
        ("commit", "git", "merge", "git"),
        ("commit", "verified", "result", "ready"),
        ("commit", "git", "result", "git"),
        ("work", "task", "result", "task"),
    ]
    .into_iter()
    .map(|(source, output, target, input)| Wire {
        from: Endpoint {
            node: source.into(),
            port: output.into(),
        },
        to: Endpoint {
            node: target.into(),
            port: input.into(),
        },
    })
    .collect();
    WorkflowGraph {
        name: "Auditable task delivery".into(),
        nodes,
        wires,
    }
}

impl Store {
    pub(super) async fn builtin_delivery_for_project(
        &self,
        project: String,
        roles: DeliveryRoles,
    ) -> Result<WorkflowGraph> {
        let maintenance = self
            .execute(move |db| super::maintenance::builtin_enabled(db, &project))
            .await?;
        let mut graph = builtin_delivery_graph(roles);
        if maintenance {
            graph.nodes.push(NodeInstance {
                id: MAINTENANCE_ACTIVATION_ID.into(),
                kind: BuiltinNode::ActivateMaintenance,
                configuration: NodeConfiguration::None,
            });
            for (from, to) in [("verified", "ready"), ("git", "git")] {
                graph.wires.push(Wire {
                    from: Endpoint {
                        node: "commit".into(),
                        port: from.into(),
                    },
                    to: Endpoint {
                        node: MAINTENANCE_ACTIVATION_ID.into(),
                        port: to.into(),
                    },
                });
            }
        }
        Ok(graph)
    }

    pub async fn start_builtin_delivery(
        &self,
        project: String,
        roles: DeliveryRoles,
    ) -> Result<WorkflowRun> {
        let graph = self
            .builtin_delivery_for_project(project.clone(), roles)
            .await?;
        let version = self
            .save_workflow_revision(project.clone(), BUILTIN_DELIVERY_ID.into(), graph)
            .await?;
        self.start_workflow_run(project, version).await
    }
}
