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
mod execution;
mod generic_agent;
#[cfg(test)]
mod git_integration;
pub mod github;
pub mod inspection;
pub(crate) mod integrated_checks;
mod iteration;
mod job_inputs;
mod launch;
pub mod maintenance;
pub mod mcp_tools;
mod merge_execution;
mod operator;
mod plan_revision;
#[cfg(test)]
mod planning_integration;
mod registry;
mod repetition;
pub mod role_templates;
mod runtime;
pub(crate) use runtime::load_definition;
pub mod settings_proposals;
pub mod stage_goal;
#[cfg(test)]
mod stage_models;
pub mod stage_plan;
pub mod stage_requirements;
mod subflow;
pub mod templates;
pub(crate) mod test_phase;
#[cfg(test)]
mod tests;
pub mod triggers;
mod unit_execution;
mod units;

pub use approval::{
    ApprovalDecision, ApprovalDecisionSource, ApprovalFacts, ApprovalRuleEvidence,
    ApprovalRuleOutcome, ApprovalRuleSettings, BacklogApprovalScope, UnmetApproval,
    WorkflowApprovalPreview, WorkflowApprovalStage, WorkflowApprovalSubmission,
};
pub(crate) use approval::{
    active_delivery_workflow, detach_active_delivery_workflow, workflow_release_authorized,
};
pub use artifacts::{
    ApprovalReference, ArtifactValue, CandidateReference, ScopedEvidence, TaskReference,
    WorkflowAcceptanceReference, WorkflowArtifact, WorkflowMergeReference,
};
pub use builtin::{DeliveryRoles, builtin_delivery_graph};
pub(crate) use command::publish_command_result;
pub use command::{
    CommandConfiguration, CommandData, CommandOutputFormat, CommandRegistration,
    CommandRegistrationRequest, CommandTarget, WorkflowCommandResult, WorkflowCommandSubmission,
};
pub use control::{ConditionPredicate, ControlStageReceipt};
pub(crate) use control::{refresh_from_attempt, validate_attempt_control};
pub use execution::{NodeInputs, WorkflowHostRequest, WorkflowHostSubmission};
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
    ManualWorkflowLaunch, RepositoryNotePlanning, TypedStartConfiguration, WorkflowLaunchEvent,
    WorkflowLaunchOrigin, WorkflowLaunchReceipt, WorkflowLaunchRequest, WorkflowLaunchSource,
    WorkflowStartSchema,
};
pub(crate) use launch::{launch_workflow_in_tx, validate_workflow_launch_source};
pub(crate) use launch::{pinned_workflow_checklist_for_job, require_launch_checklist_item};
pub(crate) use operator::waiting_for_delivery_commit;
pub use operator::{WorkflowAgentTarget, WorkflowOperatorJob, WorkflowPlanProposal};
pub use plan_revision::{WorkflowPlanHistory, WorkflowPlanRevisionRequest};
pub use registry::{
    ArtifactKind, BuiltinNode, ConfigurationKind, Effect, Executor, NodeConfiguration,
    NodeDefinition, Port, PortType, builtin_registry,
};
pub use repetition::{RepeatFeedback, RepeatUntilConfiguration};
pub use runtime::{NodeAttempt, NodePin, WorkflowAgentSubmission, WorkflowRun};
pub(crate) use runtime::{load_definition as load_workflow_definition, load_node_attempt};
pub use subflow::WorkflowSubflow;
pub(crate) use unit_execution::validate_unit_acceptance;
pub use unit_execution::{WorkflowUnitOutcome, WorkflowUnitRequest, WorkflowUnitSubmission};
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
        graph.nodes.iter().all(|node| !matches!(
            node.kind,
            BuiltinNode::Subflow | BuiltinNode::RepeatUntil | BuiltinNode::ForEach
        )),
        "Select a pinned subflow in an authoring package for reusable calls, repeats and iteration"
    );
    ensure!(
        graph
            .nodes
            .iter()
            .all(|node| !node.kind.generated_iteration()),
        "For each generates its internal stages during publication"
    );
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
        let pinned = PinnedNode {
            instance,
            revision,
            definition: definition.clone(),
        };
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
        if wire.to.port == "value"
            && matches!(
                nodes[to].instance.configuration,
                NodeConfiguration::Condition {
                    predicate: ConditionPredicate::CandidateChecksPassed
                }
            )
        {
            ensure!(
                output.value == PortType::Data(ArtifactKind::Checks),
                "A candidate checks condition needs a checks output"
            );
        }
        ensure!(
            !matches!(&nodes[from].instance.configuration, NodeConfiguration::FindWork { query }
                if query.result == agile_nodes::FindWorkResultMode::List && matches!(wire.from.port.as_str(), "task" | "target")),
            "Find work needs Single mode to connect a work target"
        );
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
        let accepts_multiple = match nodes[to].instance.kind {
            BuiltinNode::Join => input.name == "branches",
            BuiltinNode::SummarizeResult | BuiltinNode::CollectEvidence => input.name == "evidence",
            BuiltinNode::ForEachCollect => matches!(input.name.as_str(), "finished" | "result"),
            _ => false,
        };
        ensure!(
            incoming.insert((wire.to.node.as_str(), wire.to.port.as_str())) || accepts_multiple,
            "an input port accepts one wire"
        );
        dependents[from].push(to);
        inputs_remaining[to] += 1;
    }
    for node in &nodes {
        if let NodeConfiguration::StagedChecks {
            strategy_checks, ..
        } = &node.instance.configuration
        {
            ensure!(
                strategy_checks.is_empty()
                    || node
                        .definition
                        .inputs
                        .iter()
                        .any(|port| port.name == "strategy"),
                "Update the verification node to a version that accepts approved test strategy"
            );
        }
        if let NodeConfiguration::AgentRole { requirements, .. } = &node.instance.configuration
            && requirements.review_reuse
        {
            let source = |port| {
                graph
                    .wires
                    .iter()
                    .find(|wire| wire.to.node == node.instance.id && wire.to.port == port)
                    .map(|wire| &wire.from)
            };
            let verified_subject =
                source("prompt")
                    .zip(source("review_subject"))
                    .is_some_and(|(prompt, subject)| {
                        prompt.node == subject.node
                            && prompt.port == "text"
                            && subject.port == "evidence"
                            && indexes.get(prompt.node.as_str()).is_some_and(|&index| {
                                nodes[index].instance.kind == BuiltinNode::CollectEvidence
                            })
                    });
            ensure!(
                node.instance.kind == BuiltinNode::Agent
                    && requirements.goal.is_none()
                    && requirements.plan.is_none()
                    && source("comparison").is_none()
                    && verified_subject,
                "Review reuse on {} requires prompt and review subject from the same Collect Evidence node",
                node.instance.id
            );
        }
        for port in &node.definition.inputs {
            let connected = incoming.contains(&(node.instance.id.as_str(), port.name.as_str()));
            if matches!(
                (node.instance.kind, port.name.as_str()),
                (
                    BuiltinNode::Approval | BuiltinNode::SummarizeResult,
                    "evidence"
                ) | (BuiltinNode::Agent, "comparison")
                    | (
                        BuiltinNode::Implement | BuiltinNode::ReviewCandidate,
                        "brief"
                    )
                    | (BuiltinNode::OpenOrUpdatePullRequest, "risk")
                    | (BuiltinNode::MergePullRequest, "approval")
                    | (BuiltinNode::MaterializeBacklog, "design" | "strategy")
            ) {
                continue;
            }
            use agile_nodes::ManageWorkConfiguration as Work;
            let conditionally_required = match (&node.instance.configuration, port.name.as_str()) {
                (NodeConfiguration::AgentRole { requirements, .. }, "review_subject") => {
                    Some(requirements.review_reuse)
                }
                (NodeConfiguration::ManageWork { action }, "task") => {
                    Some(matches!(action, Work::Link | Work::CreateSubtask))
                }
                (NodeConfiguration::ManageWork { action }, "title") => {
                    Some(matches!(action, Work::CreateTask | Work::CreateSubtask))
                }
                (NodeConfiguration::LoadWorkContext { mode }, "task") => Some(mode.needs_task()),
                (NodeConfiguration::LoadWorkContext { mode }, "document") => {
                    Some(mode.needs_document())
                }
                (NodeConfiguration::SetFields(configuration), "source") => {
                    Some(configuration.requires_source())
                }
                (NodeConfiguration::BuildText(configuration), "values") => {
                    Some(configuration.requires_values())
                }
                (NodeConfiguration::McpTool(configuration), "arguments") => {
                    Some(!configuration.argument_mappings.is_empty())
                }
                (NodeConfiguration::BuildList(configuration), "value") => {
                    Some(configuration.requires_evidence())
                }
                (NodeConfiguration::VerifyTestPhase(configuration), name) => {
                    configuration.required_input(name)
                }
                (
                    NodeConfiguration::StagedChecks {
                        strategy_checks, ..
                    },
                    "strategy",
                ) => Some(!strategy_checks.is_empty()),
                (_, "strategy") if node.instance.kind == BuiltinNode::VerifyCandidate => {
                    Some(false)
                }
                (_, "quality" | "build") if node.instance.kind == BuiltinNode::AcceptCandidate => {
                    Some(matches!(
                        node.instance.configuration,
                        NodeConfiguration::CandidateAcceptance(_)
                    ))
                }
                _ => None,
            };
            if let Some(required) = conditionally_required {
                ensure!(
                    connected == required,
                    "Node {} requires input {} exactly when configured to use upstream evidence",
                    node.instance.id,
                    port.name
                );
                continue;
            }
            ensure!(
                connected,
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
    let mut attempt_limits = nodes
        .iter()
        .filter_map(|node| match node.instance.configuration {
            NodeConfiguration::DeliveryAttempt { limit, .. } => Some(limit),
            _ => None,
        });
    if let Some(limit) = attempt_limits.next() {
        ensure!(
            attempt_limits.all(|other| other == limit),
            "Workflow delivery gates must share the same attempt budget"
        );
    }
    if enforce_release {
        control::validate_release_wires(&nodes, &graph.wires)?;
        maintenance::validate_activation_wires(&nodes, &graph.wires)?;
    }
    let revision = digest(&serde_json::to_vec(&(&graph.name, &nodes, &graph.wires))?);
    let definition = WorkflowDefinition {
        version: WorkflowRevision { id, revision },
        name: graph.name,
        nodes,
        wires: graph.wires,
        subflows: Vec::new(),
    };
    let unit_nodes = control::unit_nodes(&definition);
    ensure!(
        definition.nodes.iter().all(|node| !matches!(
            node.instance.kind,
            BuiltinNode::BuildList
                | BuiltinNode::ForEachBegin
                | BuiltinNode::ForEachGate
                | BuiltinNode::ForEachCollect
        ) || !unit_nodes.contains(node.instance.id.as_str())),
        "List execution currently requires a workflow outside a logical delivery unit"
    );
    Ok(definition)
}
