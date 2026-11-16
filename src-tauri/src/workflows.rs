use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod approval;
mod artifacts;
pub mod authoring;
pub mod comparison;
mod builtin;
mod command;
mod control;
mod execution;
pub mod inspection;
#[cfg(test)]
mod git_integration;
mod job_inputs;
mod launch;
mod merge_execution;
mod operator;
mod plan_revision;
#[cfg(test)]
mod planning_integration;
mod registry;
pub mod role_templates;
pub mod triggers;
mod runtime;
#[cfg(test)]
mod stage_models;
#[cfg(test)]
mod tests;
mod unit_execution;
mod units;

pub use approval::{
    ApprovalDecision, ApprovalDecisionSource, ApprovalFacts, ApprovalRuleEvidence, ApprovalRuleOutcome,
    ApprovalRuleSettings, UnmetApproval, WorkflowApprovalPreview, WorkflowApprovalSubmission,
};
pub(crate) use approval::{
    active_delivery_workflow, detach_active_delivery_workflow, workflow_release_authorized,
};
pub use artifacts::{
    ApprovalReference, ArtifactValue, CandidateReference, ScopedEvidence, TaskReference,
    WorkflowAcceptanceReference, WorkflowArtifact, WorkflowMergeReference,
};
pub use builtin::{DeliveryRoles, builtin_delivery_graph};
pub use control::{ConditionPredicate, ControlStageReceipt};
pub(crate) use control::{refresh_from_attempt, validate_attempt_control};
pub use command::{
    CommandConfiguration, CommandData, CommandOutputFormat, CommandRegistration,
    CommandRegistrationRequest, CommandTarget, WorkflowCommandResult, WorkflowCommandSubmission,
};
pub(crate) use command::publish_command_result;
pub use execution::{NodeInputs, WorkflowHostRequest, WorkflowHostSubmission};
pub(crate) use job_inputs::{WorkflowJobInput, bind_job_input, stage_job_input};
pub use launch::{
    ManualWorkflowLaunch, WorkflowLaunchOrigin, WorkflowLaunchReceipt, WorkflowLaunchSource,
};
pub(crate) use launch::{launch_workflow_in_tx, validate_workflow_launch_source};
pub(crate) use operator::waiting_for_delivery_commit;
pub use operator::{WorkflowAgentTarget, WorkflowOperatorJob, WorkflowPlanProposal};
pub use plan_revision::{WorkflowPlanHistory, WorkflowPlanRevisionRequest};
pub use registry::{
    ArtifactKind, BuiltinNode, ConfigurationKind, Effect, Executor, NodeConfiguration,
    NodeDefinition, Port, PortType, builtin_registry,
};
pub use runtime::{NodeAttempt, NodePin, WorkflowAgentSubmission, WorkflowRun};
pub(crate) use runtime::{load_definition as load_workflow_definition, load_node_attempt};
pub(crate) use unit_execution::validate_unit_acceptance;
pub use unit_execution::{WorkflowUnitOutcome, WorkflowUnitRequest, WorkflowUnitSubmission};
pub use units::{WorkflowDeliveryUnit, WorkflowPlanUnit};
pub(crate) use units::{
    bind_delivery_commit, load_delivery_unit, load_delivery_units, validate_unit_capture,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeInstance {
    pub id: String,
    pub kind: BuiltinNode,
    pub configuration: NodeConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Endpoint {
    pub node: String,
    pub port: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Wire {
    pub from: Endpoint,
    pub to: Endpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowGraph {
    pub name: String,
    pub nodes: Vec<NodeInstance>,
    pub wires: Vec<Wire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRevision {
    pub id: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedNode {
    pub instance: NodeInstance,
    pub revision: String,
    pub definition: NodeDefinition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinition {
    pub version: WorkflowRevision,
    pub name: String,
    pub nodes: Vec<PinnedNode>,
    pub wires: Vec<Wire>,
}

fn digest(payload: &[u8]) -> String {
    format!("{:x}", Sha256::digest(payload))
}

fn compile(id: String, graph: WorkflowGraph) -> Result<WorkflowDefinition> {
    let registry: HashMap<_, _> = builtin_registry()
        .into_iter()
        .map(|node| (node.kind, node))
        .collect();
    compile_resolved(id, graph, |instance| registry.get(&instance.kind))
}

fn compile_resolved<'definition>(
    id: String,
    graph: WorkflowGraph,
    definition_for: impl Fn(&NodeInstance) -> Option<&'definition NodeDefinition>,
) -> Result<WorkflowDefinition> {
    ensure!(
        !id.trim().is_empty() && !graph.name.trim().is_empty(),
        "a workflow needs an ID and name"
    );
    let mut nodes = Vec::with_capacity(graph.nodes.len());
    for instance in graph.nodes {
        let definition = definition_for(&instance)
            .filter(|definition| definition.kind == instance.kind)
            .context("unregistered built-in node")?;
        ensure!(
            !instance.id.trim().is_empty(),
            "a workflow node needs an ID"
        );
        ensure!(
            instance.configuration.matches(definition.configuration),
            "invalid configuration for node {}",
            instance.id
        );
        let revision = digest(&serde_json::to_vec(&(&instance, definition))?);
        nodes.push(PinnedNode {
            instance,
            revision,
            definition: definition.clone(),
        });
    }

    let indexes: HashMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.instance.id.as_str(), index))
        .collect();
    ensure!(
        indexes.len() == nodes.len(),
        "workflow node IDs must be unique"
    );
    ensure!(
        nodes
            .iter()
            .filter(|node| node.instance.kind == BuiltinNode::Start)
            .count()
            == 1,
        "a workflow needs one Start node"
    );
    let mut incoming = HashSet::new();
    let mut connections = HashSet::new();
    let mut dependents = vec![Vec::new(); nodes.len()];
    let mut inputs_remaining = vec![0; nodes.len()];
    for wire in &graph.wires {
        let from = *indexes
            .get(wire.from.node.as_str())
            .context("wire source node is unavailable")?;
        let to = *indexes
            .get(wire.to.node.as_str())
            .context("wire target node is unavailable")?;
        let output = nodes[from]
            .definition
            .outputs
            .iter()
            .find(|port| port.name == wire.from.port)
            .context("wire source port is unavailable")?;
        let input = nodes[to]
            .definition
            .inputs
            .iter()
            .find(|port| port.name == wire.to.port)
            .context("wire target port is unavailable")?;
        ensure!(input.value.accepts(output.value), "incompatible workflow ports");
        ensure!(connections.insert((&wire.from, &wire.to)), "workflow wire already exists");
        let accepts_multiple = nodes[to].instance.kind == BuiltinNode::Join && input.name == "branches";
        ensure!(
            incoming.insert((wire.to.node.as_str(), wire.to.port.as_str())) || accepts_multiple,
            "an input port accepts one wire"
        );
        dependents[from].push(to);
        inputs_remaining[to] += 1;
    }
    for node in &nodes {
        for port in &node.definition.inputs {
            ensure!(
                incoming.contains(&(node.instance.id.as_str(), port.name.as_str())),
                "input {} on {} needs a connection",
                port.name,
                node.instance.id
            );
        }
    }
    let mut ready: VecDeque<_> = inputs_remaining
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect();
    let mut visited = 0;
    while let Some(index) = ready.pop_front() {
        visited += 1;
        for &dependent in &dependents[index] {
            inputs_remaining[dependent] -= 1;
            if inputs_remaining[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }
    ensure!(visited == nodes.len(), "workflow graph contains a cycle");
    control::validate_release_wires(&nodes, &graph.wires)?;
    let revision = digest(&serde_json::to_vec(&(&graph.name, &nodes, &graph.wires))?);
    Ok(WorkflowDefinition {
        version: WorkflowRevision { id, revision },
        name: graph.name,
        nodes,
        wires: graph.wires,
    })
}
