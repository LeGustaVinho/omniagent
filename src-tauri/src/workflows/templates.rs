use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{
    ArtifactKind, BuiltinNode, CommandOutputFormat, DeliveryRoles, Endpoint, NodeConfiguration,
    NodeInstance, Port, PortType, Wire, WorkflowGraph,
    authoring::package::{
        PackageArtifactBody, PackageCommandBinding, PackageDestination, PackageMember,
        PackageReference, WorkflowPackage, WorkflowPackageDraft,
    },
    builtin_delivery_graph,
};

pub mod diff;
mod repair;
#[cfg(test)]
mod tests;

const FACTORY_REVISION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowFactoryTemplate {
    TaskDelivery,
    ReviewDiff,
    FixFailedChecks,
    DeliveryLibrary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowFactoryEntry {
    pub id: WorkflowFactoryTemplate,
    pub name: &'static str,
    pub revision: u16,
    pub members: Vec<&'static str>,
}

pub fn workflow_factory_templates() -> Vec<WorkflowFactoryEntry> {
    vec![
        WorkflowFactoryEntry {
            id: WorkflowFactoryTemplate::TaskDelivery,
            name: "Task delivery",
            revision: FACTORY_REVISION,
            members: vec!["Task delivery"],
        },
        WorkflowFactoryEntry {
            id: WorkflowFactoryTemplate::ReviewDiff,
            name: "Review diff",
            revision: FACTORY_REVISION,
            members: vec!["Review diff"],
        },
        WorkflowFactoryEntry {
            id: WorkflowFactoryTemplate::FixFailedChecks,
            name: "Fix failed checks",
            revision: FACTORY_REVISION,
            members: vec!["Fix failed checks", "Repair and verify"],
        },
        WorkflowFactoryEntry {
            id: WorkflowFactoryTemplate::DeliveryLibrary,
            name: "Delivery library",
            revision: FACTORY_REVISION,
            members: vec![
                "Plan",
                "Implement",
                "Review",
                "Build",
                "Lint",
                "Test",
                "Review and test",
            ],
        },
    ]
}

impl Store {
    pub async fn create_workflow_factory_draft(
        &self,
        project: String,
        template: WorkflowFactoryTemplate,
        revision: u16,
    ) -> Result<WorkflowPackageDraft> {
        ensure!(
            revision == FACTORY_REVISION,
            "Choose the current factory template revision"
        );
        let package = match template {
            WorkflowFactoryTemplate::TaskDelivery => task_delivery(),
            WorkflowFactoryTemplate::ReviewDiff => diff::review_diff(),
            WorkflowFactoryTemplate::FixFailedChecks => repair::fix_failed_checks(),
            WorkflowFactoryTemplate::DeliveryLibrary => delivery_library(),
        };
        self.create_workflow_package(project, None, package).await
    }
}

fn task_delivery() -> WorkflowPackage {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.name = "Task delivery".into();
    WorkflowPackage {
        name: graph.name.clone(),
        members: vec![member(
            "task-delivery",
            "Task delivery",
            PackageDestination::Project,
            PackageArtifactBody::Pipeline { graph },
        )],
    }
}

fn delivery_library() -> WorkflowPackage {
    let members = vec![
        agent("plan-agent", "Plan", BuiltinNode::Plan, "planner"),
        agent(
            "implement-agent",
            "Implement",
            BuiltinNode::Implement,
            "implementer",
        ),
        agent(
            "review-agent",
            "Review",
            BuiltinNode::ReviewCandidate,
            "candidate reviewer",
        ),
        command("build-command", "Build"),
        command("lint-command", "Lint"),
        command("test-command", "Test"),
        review_and_test(),
    ];
    WorkflowPackage {
        name: "Delivery library".into(),
        members,
    }
}

fn agent(key: &str, name: &str, kind: BuiltinNode, role: &str) -> PackageMember {
    member(
        key,
        name,
        PackageDestination::Library,
        PackageArtifactBody::Preset {
            node_kind: kind,
            configuration: agent_role(role),
            editable_fields: vec!["role".into()],
        },
    )
}

fn agent_role(role: &str) -> NodeConfiguration {
    NodeConfiguration::AgentRole {
        role: role.into(),
        model: None,
        requirements: Default::default(),
    }
}

fn command(key: &str, name: &str) -> PackageMember {
    member(
        key,
        name,
        PackageDestination::Library,
        PackageArtifactBody::Command {
            binding: PackageCommandBinding::Required { name: name.into() },
            arguments: vec![],
            input: Default::default(),
            output: CommandOutputFormat::Text,
        },
    )
}

fn review_and_test() -> PackageMember {
    use BuiltinNode::*;
    let mut flow = member(
        "review-and-test",
        "Review and test",
        PackageDestination::Library,
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: "Review and test".into(),
                nodes: vec![
                    node(
                        "entry",
                        Start,
                        NodeConfiguration::SubflowEntry {
                            outputs: vec![
                                Port {
                                    name: "ready".into(),
                                    value: PortType::Control,
                                },
                                Port {
                                    name: "candidate".into(),
                                    value: PortType::Data(ArtifactKind::Candidate),
                                },
                            ],
                        },
                    ),
                    node("fan-out", Parallel, NodeConfiguration::None),
                    node("review", ReviewCandidate, agent_role("candidate reviewer")),
                    node(
                        "check",
                        VerifyCandidate,
                        NodeConfiguration::RequiredChecks { names: vec![] },
                    ),
                    node("join", Join, NodeConfiguration::None),
                ],
                wires: vec![
                    wire("entry", "ready", "fan-out", "ready"),
                    wire("fan-out", "branches", "review", "ready"),
                    wire("fan-out", "branches", "check", "ready"),
                    wire("entry", "candidate", "review", "candidate"),
                    wire("entry", "candidate", "check", "candidate"),
                    wire("review", "reviewed", "join", "branches"),
                    wire("check", "verified", "join", "branches"),
                ],
            },
            inputs: BTreeMap::from([
                ("ready".into(), endpoint("entry", "ready")),
                ("candidate".into(), endpoint("entry", "candidate")),
            ]),
            outputs: BTreeMap::from([
                ("complete".into(), endpoint("join", "joined")),
                ("review".into(), endpoint("review", "review")),
                ("checks".into(), endpoint("check", "checks")),
            ]),
        },
    );
    flow.references.insert(
        "review".into(),
        PackageReference::Draft {
            key: "review-agent".into(),
        },
    );
    flow
}

fn member(
    key: &str,
    name: &str,
    destination: PackageDestination,
    body: PackageArtifactBody,
) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: name.into(),
        destination,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn wire(from: &str, output: &str, to: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(from, output),
        to: endpoint(to, input),
    }
}
