use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod agile_nodes;
mod approval;
mod artifacts;
pub mod authoring;
pub(crate) mod automation;
mod backlog;
pub(crate) use backlog::backlog_commit_message;
mod builtin;
mod candidate_acceptance;
mod command;
pub mod comparison;
mod control;
pub mod conversation_turns;
mod data_nodes;
pub mod decision_nodes;
pub mod diff;
mod execution;
mod generic_agent;
pub mod github;
pub mod inspection;
pub(crate) mod integrated_checks;
mod iteration;
mod job_inputs;
mod launch;
pub mod maintenance;
pub mod mcp_tools;
mod operator;
mod orchestration;
mod plan_revision;
pub(crate) mod prompt;
mod registry;
mod repetition;
pub mod role_templates;
mod runtime;
pub(crate) mod schema;
pub(crate) mod structured_agent;
pub(crate) use runtime::load_definition;
pub mod settings_proposals;
pub mod stage_goal;
pub mod stage_plan;
pub mod stage_requirements;
mod subflow;
pub(crate) mod test_phase;
#[cfg(test)]
pub(crate) mod tests;
mod timed;
pub mod triggers;
mod unit_execution;
mod units;
pub(crate) mod write_scope;

pub use approval::{
    ApprovalDecision, ApprovalDecisionSource, ApprovalFacts, ApprovalRuleEvidence,
    ApprovalRuleOutcome, ApprovalRuleSettings, BacklogApprovalScope, UnmetApproval,
};
pub(crate) use approval::{
    active_delivery_workflow, detach_active_delivery_workflow, workflow_release_authorized,
};
pub use artifacts::{
    ApprovalReference, ArtifactValue, CandidateReference, ScopedEvidence, TaskReference,
    WorkflowAcceptanceReference, WorkflowArtifact, WorkflowMergeReference,
};
pub(crate) use command::publish_command_result;
pub use command::{
    CommandConfiguration, CommandData, CommandOutputFormat, CommandRegistration,
    CommandRegistrationRequest, CommandTarget, WorkflowCommandResult,
};
pub use control::{ConditionPredicate, ControlStageReceipt};
pub(crate) use control::{refresh_from_attempt, validate_attempt_control};
pub use execution::NodeInputs;
pub use iteration::{
    BuildListConfiguration, ForEachBeginConfiguration, ForEachCollectConfiguration,
    ForEachConfiguration, ForEachGateConfiguration, WorkflowIterationResults, WorkflowJsonItem,
    WorkflowJsonList,
};
pub(crate) use job_inputs::stage_job_input_with_repair;
pub(crate) use job_inputs::{
    WorkflowJobInput, bind_job_input, load_workflow_job_input, stage_job_input,
};
pub use launch::{
    ManualWorkflowLaunch, TypedStartConfiguration, WorkflowLaunchEvent, WorkflowLaunchOrigin,
    WorkflowLaunchReceipt, WorkflowLaunchRequest, WorkflowLaunchSource, WorkflowStartSchema,
};
pub(crate) use launch::{launch_workflow_in_tx, validate_workflow_launch_source};
pub(crate) use launch::{pinned_workflow_checklist_for_job, require_launch_checklist_item};
pub(crate) use operator::waiting_for_delivery_commit;
pub use operator::{WorkflowAgentTarget, WorkflowOperatorJob, WorkflowPlanProposal};
pub use plan_revision::{WorkflowPlanHistory, WorkflowPlanRevisionRequest};
pub use registry::{
    ArchivedNode, ArtifactKind, BuiltinNode, ConfigurationKind, Effect, Executor,
    NodeConfiguration, NodeDefinition, Port, PortType, builtin_registry,
};
pub use repetition::{RepeatFeedback, RepeatUntilConfiguration};
pub use runtime::{NodeAttempt, NodePin, WorkflowAgentSubmission, WorkflowRun};
pub(crate) use runtime::{load_definition as load_workflow_definition, load_node_attempt};
pub use subflow::WorkflowSubflow;
pub(crate) use unit_execution::validate_unit_acceptance;
pub use units::{WorkflowDeliveryUnit, WorkflowPlanUnit};
pub(crate) use units::{
    bind_delivery_commit, load_delivery_unit, load_delivery_units, validate_selected_plan_revision,
    validate_unit_capture,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subflows: Vec<WorkflowSubflow>,
}

impl WorkflowDefinition {
    pub fn unsupported_node(&self) -> Option<&PinnedNode> {
        self.nodes
            .iter()
            .find(|node| !node.instance.kind.is_supported())
    }

    pub(crate) fn ensure_supported_nodes(&self) -> Result<()> {
        if let Some(node) = self.unsupported_node() {
            anyhow::bail!(
                "Node {} ({}) was removed. Replace it in the Builder before running this workflow.",
                node.instance.id,
                node.definition.title
            );
        }
        Ok(())
    }

    fn normalize_legacy_prompt_nodes(&mut self) {
        let generated_joins = self
            .subflows
            .iter()
            .filter_map(|scope| {
                let failure = scope.outputs.get("failed")?;
                let execute = format!("{}::execute", scope.path);
                (failure.node == format!("{}::failure", scope.path)
                    && failure.port == "joined"
                    && self.nodes.iter().any(|node| {
                        node.instance.id == execute
                            && structured_agent::configuration(node).is_some()
                    }))
                .then(|| failure.node.clone())
            })
            .collect::<HashSet<_>>();
        let generated_verdicts = self.generated_legacy_review_verdicts();
        for node in &mut self.nodes {
            if generated_joins.contains(&node.instance.id)
                && node.instance.kind == BuiltinNode::Archived(ArchivedNode::Join)
                && node.instance.configuration == NodeConfiguration::None
                && node.definition.executor == Executor::JoinBranches
            {
                node.instance.kind = BuiltinNode::JoinResults;
                node.instance.configuration = NodeConfiguration::JoinFailures;
                node.definition.kind = BuiltinNode::JoinResults;
                registry::configure_failure_join(&mut node.definition);
            }
            if generated_verdicts.contains(&node.instance.id) {
                node.instance.kind = BuiltinNode::JoinResults;
                node.instance.configuration = NodeConfiguration::LegacyReviewVerdict;
                node.definition.kind = BuiltinNode::JoinResults;
                registry::configure_legacy_review_verdict(&mut node.definition);
            }
        }
    }

    fn generated_legacy_review_verdicts(&self) -> HashSet<String> {
        let nodes: HashMap<_, _> = self
            .nodes
            .iter()
            .map(|node| (node.instance.id.as_str(), node))
            .collect();
        self.subflows.iter().filter_map(|scope| {
            use role_templates::TemplateReviewPolicy;

            let execute = format!("{}::execute", scope.path);
            let reviewer = format!("{}::reviewer", scope.path);
            let verdict = format!("{}::verdict", scope.path);
            let failure = format!("{}::failure", scope.path);
            let end = format!("{}::return", scope.path);
            let old = *nodes.get(verdict.as_str())?;
            let execute_role = &structured_agent::configuration(nodes.get(execute.as_str())?)?.effective_role;
            let review_config = structured_agent::configuration(nodes.get(reviewer.as_str())?)?;
            let targets = |name: &str, node: &str, port: &str| {
                scope.outputs.get(name).is_some_and(|endpoint| endpoint.node == node && endpoint.port == port)
            };
            let wired = |from: &str, output: &str, to: &str, input: &str| {
                self.wires.iter().any(|wire| {
                    wire.from.node == from && wire.from.port == output
                        && wire.to.node == to && wire.to.port == input
                })
            };
            (old.instance.kind == BuiltinNode::Archived(ArchivedNode::Condition)
                && old.definition.kind == old.instance.kind
                && old.definition.executor == Executor::EvaluateCondition
                && old.definition.configuration == ConfigurationKind::Condition
                && matches!(&old.instance.configuration, NodeConfiguration::Condition {
                    predicate: ConditionPredicate::Equals { pointer, value: serde_json::Value::Bool(true) }
                } if pointer == "/evidence/approved")
                && execute_role.review_policy == TemplateReviewPolicy::CrossReview
                && review_config.effective_role.review_policy == TemplateReviewPolicy::Independent
                && !review_config.review_verdict
                && scope.nodes.contains(&verdict)
                && targets("completed", &end, "completed")
                && targets("result", &end, "result")
                && targets("failed", &failure, "joined")
                && self.wires.iter().filter(|wire| wire.to.node == verdict).count() == 2
                && self.wires.iter().filter(|wire| wire.from.node == verdict).count() == 2
                && wired(&reviewer, "completed", &verdict, "ready")
                && wired(&reviewer, "result", &verdict, "value")
                && wired(&verdict, "matched", &end, "ready")
                && wired(&verdict, "unmatched", &failure, "branches"))
            .then_some(verdict)
        }).collect()
    }
}

impl NodeInstance {
    pub(super) fn validate_authored(&self) -> Result<()> {
        ensure!(
            self.kind.is_supported(),
            "Node {} uses a retired node type",
            self.id
        );
        ensure!(
            !self.configuration.is_generated(),
            "The engine generates the internal execution settings for {}",
            self.id
        );
        Ok(())
    }
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
    for node in &graph.nodes {
        node.validate_authored()?;
        ensure!(
            !matches!(node.kind, BuiltinNode::Subflow | BuiltinNode::Loop),
            "Select a pinned Function or AI Node Agent through an authoring package"
        );
    }
    compile_resolved_graph(id, graph, definition_for, true)
}

fn compile_resolved_graph<'definition>(
    id: String,
    graph: WorkflowGraph,
    definition_for: impl Fn(&NodeInstance) -> Option<&'definition NodeDefinition>,
    enforce_release: bool,
) -> Result<WorkflowDefinition> {
    ensure!(
        !id.trim().is_empty() && !graph.name.trim().is_empty(),
        "a workflow needs an ID and name"
    );
    let mut definitions = graph
        .nodes
        .iter()
        .map(|instance| {
            ensure!(
                instance.kind.is_supported(),
                "Node {} uses a retired node type",
                instance.id
            );
            let definition = definition_for(instance)
                .filter(|definition| definition.kind == instance.kind)
                .context("unregistered built-in node")?;
            Ok((instance.id.clone(), definition.clone()))
        })
        .collect::<Result<_>>()?;
    resolve_port_schemas(&graph, &mut definitions)?;
    let mut nodes = Vec::with_capacity(graph.nodes.len());
    for instance in graph.nodes {
        let definition = &definitions[&instance.id];
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
        let pinned = PinnedNode {
            instance,
            revision,
            definition: definition.clone(),
        };
        if let NodeConfiguration::AgentRole { requirements, .. } = &pinned.instance.configuration {
            ensure!(
                !requirements.review_reuse && !requirements.prompt_delegate,
                "Node {} requires retired Prompt or evidence nodes",
                pinned.instance.id
            );
        }
        stage_goal::StageGoalPin::for_stage(&pinned)?;
        stage_plan::StagePlanConfiguration::for_stage(&pinned)?;
        nodes.push(pinned);
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
            .filter(|node| matches!(
                node.instance.kind,
                BuiltinNode::Start | BuiltinNode::TypedStart
            ))
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
        ensure!(
            input.value.accepts(output.value),
            "incompatible workflow ports"
        );
        if let Some(expected) = nodes[to].definition.input_schemas.get(&wire.to.port) {
            let source = nodes[from]
                .definition
                .output_schemas
                .get(&wire.from.port)
                .with_context(|| {
                    format!(
                        "{}.{} needs a Schema to connect to {}.{}",
                        wire.from.node, wire.from.port, wire.to.node, wire.to.port
                    )
                })?;
            schema::compatible(source, expected).with_context(|| {
                format!(
                    "Incompatible Schemas on {}.{} → {}.{}",
                    wire.from.node, wire.from.port, wire.to.node, wire.to.port
                )
            })?;
        }
        if wire.from.port == "event"
            && let NodeConfiguration::TypedStart(start) = &nodes[from].instance.configuration
        {
            ensure!(
                start.accepts_automation(),
                "Make Start fields optional to connect the event port"
            );
        }
        if wire.from.port == "document"
            && let NodeConfiguration::TypedStart(start) = &nodes[from].instance.configuration
        {
            ensure!(
                !start.prompt_or_markdown(),
                "Connect the Start inputs port to read the normalized request"
            );
        }
        ensure!(
            connections.insert((&wire.from, &wire.to)),
            "workflow wire already exists"
        );
        let accepts_multiple = matches!(
            nodes[to].instance.configuration,
            NodeConfiguration::JoinFailures
        ) && input.name == "branches";
        ensure!(
            incoming.insert((wire.to.node.as_str(), wire.to.port.as_str())) || accepts_multiple,
            "an input port accepts one wire"
        );
        dependents[from].push(to);
        inputs_remaining[to] += 1;
    }
    for node in &nodes {
        for port in &node.definition.inputs {
            let connected = incoming.contains(&(node.instance.id.as_str(), port.name.as_str()));
            let optional = orchestration::optional_input(node, &port.name)
                || (node.instance.kind == BuiltinNode::End
                    && port.name == "result"
                    && !node.definition.input_schemas.contains_key("result"))
                || (node.instance.kind == BuiltinNode::Wait && port.name == "input")
                || (matches!(&node.instance.configuration, NodeConfiguration::Human(config) if !config.requires_input())
                    && port.name == "input")
                || (node.instance.kind == BuiltinNode::Agent
                    && generic_agent::optional_input(&port.name));
            ensure!(
                connected || optional,
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
    orchestration::validate_graph(&nodes, &graph.wires)?;
    if enforce_release {
        control::validate_release_wires(&nodes, &graph.wires)?;
    }
    let revision = digest(&serde_json::to_vec(&(&graph.name, &nodes, &graph.wires))?);
    let definition = WorkflowDefinition {
        version: WorkflowRevision { id, revision },
        name: graph.name,
        nodes,
        wires: graph.wires,
        subflows: Vec::new(),
    };
    if definition.nodes.iter().any(|node| {
        matches!(&node.instance.configuration,
            NodeConfiguration::TypedStart(configuration) if configuration.conversation
        )
    }) {
        conversation_turns::validate_conversation_template(&definition)?;
    }
    Ok(definition)
}

fn resolve_port_schemas(
    graph: &WorkflowGraph,
    definitions: &mut std::collections::BTreeMap<String, NodeDefinition>,
) -> Result<()> {
    for node in &graph.nodes {
        if let Some(definition) = definitions.get_mut(&node.id) {
            orchestration::define(node, definition)?;
        }
    }
    schema::infer_ports(graph, definitions);
    Ok(())
}
