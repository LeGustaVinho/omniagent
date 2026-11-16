use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json};

use crate::workflows::{
    ArtifactKind, ArtifactValue, BuiltinNode, CommandOutputFormat, Endpoint, NodeConfiguration,
    PinnedNode, PortType, WorkflowArtifact, WorkflowDefinition, WorkflowIterationResults, iteration,
};

use super::{
    preview::{ReviewCatalog, review_document},
    *,
};

#[cfg(test)]
mod acceptance;
#[cfg(test)]
mod tests;

const MAX_SAMPLE_BYTES: usize = 128 * 1024;
const MAX_TRACE_NODES: usize = 512;

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowSimulationScenario {
    inputs: BTreeMap<String, Value>,
    outcomes: BTreeMap<String, SampleOutcome>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SampleOutcome {
    state: TraceState,
    #[serde(default)]
    outputs: BTreeMap<String, Value>,
    reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TraceState {
    Completed,
    Failed,
    Skipped,
    Waiting,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSimulation {
    revision: String,
    member_key: String,
    inputs: BTreeMap<String, Value>,
    steps: Vec<TraceStep>,
    subflows: Vec<TraceSubflow>,
    outputs: BTreeMap<String, TraceExport>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceStep {
    node_id: String,
    title: String,
    state: TraceState,
    inputs: Vec<TraceInput>,
    outputs: BTreeMap<String, TraceOutput>,
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceInput {
    port: String,
    from: Endpoint,
    state: Signal,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Signal {
    Selected,
    Skipped,
    Waiting,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceOutput {
    port_type: PortType,
    value: Option<Value>,
}

#[derive(Serialize)]
struct TraceExport {
    from: Endpoint,
    state: Signal,
}

#[derive(Serialize)]
struct TraceSubflow {
    path: String,
    name: String,
    revision: String,
    nodes: Vec<String>,
    outputs: BTreeMap<String, TraceExport>,
}

impl Store {
    pub async fn simulate_workflow_package_revision(
        &self,
        project: String,
        draft: String,
        revision: String,
        member_key: String,
        scenario: WorkflowSimulationScenario,
    ) -> Result<WorkflowSimulation> {
        ensure!(
            serde_json::to_vec(&scenario)?.len() <= MAX_SAMPLE_BYTES,
            "Sample data exceeds {MAX_SAMPLE_BYTES} UTF-8 bytes"
        );
        self.execute(move |db| {
            let tx = db.transaction()?;
            let document = review_document(&tx, &project, &draft, &revision)?;
            let catalog = ReviewCatalog::new(&tx, &project, &document, &revision);
            let member = catalog.draft_member(&member_key)?;
            let (steps, subflows, outputs) = match &member.body {
                PackageArtifactBody::Pipeline { .. } | PackageArtifactBody::Subflow { .. } => {
                    let (definition, _) = catalog.compile(member, &mut Vec::new())?;
                    let steps = simulate_graph(&definition, &member.body, &scenario)?;
                    let recorded: HashMap<_, _> = steps
                        .iter()
                        .map(|step| (step.node_id.as_str(), step))
                        .collect();
                    let subflows = definition
                        .subflows
                        .iter()
                        .map(|scope| TraceSubflow {
                            path: scope.path.clone(),
                            name: scope.name.clone(),
                            revision: scope.version.revision.clone(),
                            nodes: scope.nodes.clone(),
                            outputs: export(&scope.outputs, &recorded),
                        })
                        .collect();
                    let outputs = match &member.body {
                        PackageArtifactBody::Subflow { outputs, .. } => {
                            let expanded = outputs
                                .iter()
                                .map(|(name, endpoint)| {
                                    let source = definition
                                        .subflows
                                        .iter()
                                        .find(|scope| scope.path == endpoint.node)
                                        .and_then(|scope| scope.outputs.get(&endpoint.port))
                                        .unwrap_or(endpoint);
                                    (name.clone(), source.clone())
                                })
                                .collect();
                            export(&expanded, &recorded)
                        }
                        _ => BTreeMap::new(),
                    };
                    (steps, subflows, outputs)
                }
                PackageArtifactBody::Command { output, .. } => (
                    vec![simulate_single(member, &scenario, Some(*output))?],
                    Vec::new(),
                    BTreeMap::new(),
                ),
                PackageArtifactBody::McpTool { .. } => (
                    vec![simulate_single(member, &scenario, None)?],
                    Vec::new(),
                    BTreeMap::new(),
                ),
                _ => anyhow::bail!("The selected artifact has no simulation entry point"),
            };
            Ok(WorkflowSimulation {
                revision,
                member_key,
                inputs: scenario.inputs,
                steps,
                subflows,
                outputs,
            })
        })
        .await
    }
}

fn export(
    endpoints: &BTreeMap<String, Endpoint>,
    steps: &HashMap<&str, &TraceStep>,
) -> BTreeMap<String, TraceExport> {
    endpoints
        .iter()
        .map(|(port, from)| {
            (
                port.clone(),
                TraceExport {
                    from: from.clone(),
                    state: signal(steps.get(from.node.as_str()).copied(), &from.port),
                },
            )
        })
        .collect()
}

fn signal(step: Option<&TraceStep>, port: &str) -> Signal {
    match step {
        Some(step) if step.outputs.contains_key(port) => Signal::Selected,
        Some(step)
            if matches!(step.state, TraceState::Completed | TraceState::Skipped)
                || step.outputs.contains_key("failed") =>
        {
            Signal::Skipped
        }
        _ => Signal::Waiting,
    }
}

fn simulate_graph(
    definition: &WorkflowDefinition,
    body: &PackageArtifactBody,
    scenario: &WorkflowSimulationScenario,
) -> Result<Vec<TraceStep>> {
    ensure!(
        definition.nodes.len() <= MAX_TRACE_NODES,
        "Simulation supports up to {MAX_TRACE_NODES} expanded nodes"
    );
    let indexes = definition
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.instance.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    for id in scenario.outcomes.keys() {
        let node = indexes
            .get(id.as_str())
            .map(|index| &definition.nodes[*index])
            .with_context(|| format!("Unknown sample outcome node {id}"))?;
        ensure!(
            !intrinsic(node.instance.kind),
            "The node {id} computes its own simulated outcome"
        );
    }
    let mut incoming = vec![Vec::new(); indexes.len()];
    let mut outgoing = vec![Vec::new(); indexes.len()];
    let mut routed_failures = HashSet::new();
    for wire in &definition.wires {
        let source = indexes[wire.from.node.as_str()];
        let target = indexes[wire.to.node.as_str()];
        incoming[target].push(wire);
        outgoing[source].push(target);
        if wire.from.port == "failed" {
            routed_failures.insert(source);
        }
    }
    let mut degrees: Vec<_> = incoming.iter().map(Vec::len).collect();
    let mut ready: VecDeque<_> = degrees
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    let mut by_node = HashMap::new();
    let mut steps: Vec<TraceStep> = Vec::new();
    let mut trace_bytes = 0;
    while let Some(index) = ready.pop_front() {
        let node = &definition.nodes[index];
        let mut data = BTreeMap::new();
        let mut result_ordinal = 0;
        let inputs = incoming[index]
            .iter()
            .map(|wire| {
                let key = if wire.to.port == "result" && node.instance.kind == BuiltinNode::ForEachCollect {
                    result_ordinal += 1;
                    format!("result.{result_ordinal}")
                } else {
                    wire.to.port.clone()
                };
                let source = by_node
                    .get(wire.from.node.as_str())
                    .map(|position: &usize| &steps[*position]);
                if let Some(value) = source
                    .and_then(|step| step.outputs.get(&wire.from.port))
                    .and_then(|output| output.value.as_ref())
                {
                    data.insert(key, value);
                }
                TraceInput {
                    port: wire.to.port.clone(),
                    from: wire.from.clone(),
                    state: signal(source, &wire.from.port),
                }
            })
            .collect::<Vec<_>>();
        let step = trace_node(
            node,
            body,
            scenario,
            inputs,
            &data,
            routed_failures.contains(&index),
        )?;
        trace_bytes += serde_json::to_vec(&step)?.len() + 1;
        ensure!(
            trace_bytes <= 4 * 1024 * 1024,
            "The simulated trace exceeds four MiB; reduce the sample values"
        );
        by_node.insert(node.instance.id.as_str(), steps.len());
        steps.push(step);
        for &target in &outgoing[index] {
            degrees[target] -= 1;
            if degrees[target] == 0 {
                ready.push_back(target);
            }
        }
    }
    ensure!(
        steps.len() == definition.nodes.len(),
        "The expanded simulation graph contains a cycle"
    );
    Ok(steps)
}

pub(super) fn intrinsic(kind: BuiltinNode) -> bool {
    matches!(
        kind,
        BuiltinNode::Start
            | BuiltinNode::LoadContext
            | BuiltinNode::Condition
            | BuiltinNode::Parallel
            | BuiltinNode::Join
            | BuiltinNode::Result
            | BuiltinNode::IterationLimit
            | BuiltinNode::BuildList
    ) || kind.generated_iteration()
}

fn trace_node(
    node: &PinnedNode,
    body: &PackageArtifactBody,
    scenario: &WorkflowSimulationScenario,
    inputs: Vec<TraceInput>,
    data: &BTreeMap<String, &Value>,
    routed_failure: bool,
) -> Result<TraceStep> {
    use BuiltinNode::*;
    let kind = node.instance.kind;
    let mut step = TraceStep {
        node_id: node.instance.id.clone(),
        title: node.definition.title.clone(),
        state: TraceState::Waiting,
        inputs,
        outputs: BTreeMap::new(),
        reason: None,
    };
    let selected_list = (kind == ForEachCollect).then(|| simulated_list(&step.inputs, data)).transpose()?.flatten();
    let count = selected_list.as_ref().map(iteration::list_value).transpose()?.map_or(0, |list| list.values.len());
    let mut finished = 0;
    let mut results = 0;
    let relevant: Vec<_> = step.inputs.iter().filter(|input| {
        if kind != ForEachCollect { return true; }
        match input.port.as_str() {
            "finished" => { finished += 1; finished <= count }
            "result" => { results += 1; results <= count }
            _ => true,
        }
    }).collect();
    let skipped = relevant
        .iter()
        .any(|input| input.state == Signal::Skipped);
    let waiting = relevant
        .iter()
        .any(|input| input.state == Signal::Waiting);
    let selected = relevant
        .iter()
        .any(|input| input.state == Signal::Selected);
    if skipped && (kind != Join || !waiting && !selected) {
        step.state = TraceState::Skipped;
        step.reason = Some("The selected branches bypass this stage.".into());
        return Ok(step);
    }
    if waiting {
        step.reason = Some("Waiting for upstream evidence.".into());
        return Ok(step);
    }
    let mut values = BTreeMap::new();
    let control = match kind {
        Start => return trace_start(step, node, body, scenario),
        LoadContext => {
            values.insert("context".into(), (*data["document"]).clone());
            Some("loaded")
        }
        Result => {
            values.insert("delivery".into(), (*data["git"]).clone());
            Some("completed")
        }
        Condition => {
            let NodeConfiguration::Condition { predicate } = &node.instance.configuration else {
                anyhow::bail!("Invalid pinned Condition")
            };
            Some(if predicate.test(data["value"]) {
                "matched"
            } else {
                "unmatched"
            })
        }
        Parallel => Some("branches"),
        Join => Some("joined"),
        IterationLimit => {
            step.state = TraceState::Failed;
            step.reason = Some("The workflow reached its configured iteration limit.".into());
            step.outputs.insert("failed".into(), control_output());
            return Ok(step);
        }
        BuildList => {
            let NodeConfiguration::BuildList(configuration) = &node.instance.configuration else { anyhow::bail!("Missing pinned list configuration"); };
            let id = step.inputs.iter().find(|input| input.port == "value").map(|input| simulation_artifact_id(&input.from));
            match configuration.build_from_value(id.as_deref().zip(data.get("value").copied())) {
                Ok(list) => { values.insert("items".into(), serde_json::to_value(ArtifactValue::JsonList(list))?); Some("completed") }
                Err(_) => return Ok(failed_iteration(step, "The configured list source did not produce a bounded JSON array.")),
            }
        }
        ForEachBegin => {
            let NodeConfiguration::ForEachBegin(configuration) = &node.instance.configuration else { anyhow::bail!("Missing pinned iteration size"); };
            let source = simulated_list(&step.inputs, data)?.context("The simulation has no iteration list")?;
            let Some(list) = iteration::bind_list(&source, configuration.max_items)? else {
                return Ok(failed_iteration(step, "The list exceeds the configured For each limit."));
            };
            values.insert("items".into(), serde_json::to_value(ArtifactValue::JsonList(list))?);
            Some("ready")
        }
        ForEachGate => {
            let NodeConfiguration::ForEachGate(configuration) = &node.instance.configuration else { anyhow::bail!("Missing pinned iteration ordinal"); };
            let source = simulated_list(&step.inputs, data)?.context("The simulation has no iteration list")?;
            let Some(item) = iteration::item_for(&source, configuration.ordinal)? else {
                step.state = TraceState::Skipped;
                step.reason = Some("The sample list has no item at this ordinal.".into());
                return Ok(step);
            };
            values.insert("item".into(), serde_json::to_value(ArtifactValue::JsonItem(item))?);
            Some("iterate")
        }
        ForEachCollect => {
            let NodeConfiguration::ForEachCollect(configuration) = &node.instance.configuration else { anyhow::bail!("Missing pinned iteration result type"); };
            let source = selected_list.context("The simulation has no iteration list")?;
            let results = relevant.iter().filter(|input| input.port == "result").enumerate().map(|(index, input)| {
                let sample = data.get(&format!("result.{}", index + 1)).context("The simulation is missing an iteration result")?;
                ensure!(serde_json::from_value::<ArtifactKind>(sample["kind"].clone())? == configuration.result_kind,
                    "The simulated iteration result has an incompatible type");
                Ok(simulation_artifact_id(&input.from))
            }).collect::<anyhow::Result<_>>()?;
            let result = WorkflowIterationResults { list_artifact_id: source.id, result_kind: configuration.result_kind, results };
            values.insert("results".into(), serde_json::to_value(ArtifactValue::IterationResults(result))?);
            Some("completed")
        }
        _ => {
            let Some(outcome) = scenario.outcomes.get(&node.instance.id) else {
                step.reason = Some(
                    if kind == ApproveImplementation {
                        "Waiting for sample approval."
                    } else {
                        "Waiting for a sample outcome."
                    }
                    .into(),
                );
                return Ok(step);
            };
            step.state = outcome.state;
            step.reason.clone_from(&outcome.reason);
            ensure!(
                outcome.state != TraceState::Skipped,
                "Branch selection determines skipped stages"
            );
            if outcome.state != TraceState::Completed {
                ensure!(
                    outcome.outputs.is_empty(),
                    "A failed or waiting stage cannot supply data outputs"
                );
                if outcome.state == TraceState::Failed && routed_failure {
                    step.outputs.insert("failed".into(), control_output());
                }
                return Ok(step);
            }
            values.clone_from(&outcome.outputs);
            node.definition
                .outputs
                .iter()
                .find(|port| port.value == PortType::Control && port.name != "failed")
                .map(|port| port.name.as_str())
        }
    };
    step.state = TraceState::Completed;
    if let Some(control) = control {
        step.outputs.insert(control.into(), control_output());
    }
    for port in &node.definition.outputs {
        if let PortType::Data(kind) = port.value {
            let mut value = values.remove(&port.name).with_context(|| {
                format!(
                    "Sample outcome for {} needs output {}",
                    node.instance.id, port.name
                )
            })?;
            if !intrinsic(node.instance.kind) {
                value = match &node.instance.configuration {
                    NodeConfiguration::Command(command) => {
                        command_sample(&node.instance.id, command.output, value)?
                    }
                    NodeConfiguration::McpTool(_) => {
                        json!({ "runId": format!("simulation:{}", node.instance.id),
                        "output": { "rejected": false, "structured": value, "content": [] } })
                    }
                    _ => value,
                };
                value = json!({ "kind": kind, "evidence": value });
            }
            step.outputs.insert(
                port.name.clone(),
                TraceOutput {
                    port_type: port.value,
                    value: Some(value),
                },
            );
        }
    }
    ensure!(
        values.is_empty(),
        "Sample outcome for {} has unknown data outputs",
        node.instance.id
    );
    Ok(step)
}

fn simulation_artifact_id(endpoint: &Endpoint) -> String {
    format!("simulation:{}:{}", endpoint.node, endpoint.port)
}

fn simulated_list(inputs: &[TraceInput], data: &BTreeMap<String, &Value>) -> Result<Option<WorkflowArtifact>> {
    let Some(value) = data.get("items") else { return Ok(None); };
    let from = &inputs.iter().find(|input| input.port == "items").context("Missing simulated list source")?.from;
    let artifact = WorkflowArtifact {
        id: simulation_artifact_id(from), workflow_run_id: "simulation".into(), node_instance_id: from.node.clone(),
        port: from.port.clone(), unit_id: None, value: serde_json::from_value((*value).clone())?,
    };
    iteration::list_value(&artifact)?.validate()?;
    Ok(Some(artifact))
}

fn failed_iteration(mut step: TraceStep, reason: &str) -> TraceStep {
    step.state = TraceState::Failed;
    step.reason = Some(reason.into());
    step.outputs.insert("failed".into(), control_output());
    step
}

fn trace_start(
    mut step: TraceStep,
    node: &PinnedNode,
    body: &PackageArtifactBody,
    scenario: &WorkflowSimulationScenario,
) -> Result<TraceStep> {
    let input_name = |port: &str| match body {
        PackageArtifactBody::Subflow { inputs, .. } => inputs
            .iter()
            .find(|(_, endpoint)| endpoint.node == node.instance.id && endpoint.port == port)
            .map(|(name, _)| name.as_str()),
        _ if port == "document" => Some("document"),
        _ => None,
    };
    for name in scenario.inputs.keys() {
        ensure!(
            node.definition
                .outputs
                .iter()
                .any(|port| input_name(&port.name) == Some(name)),
            "Unknown sample input {name}"
        );
    }
    for port in &node.definition.outputs {
        let sample = input_name(&port.name).map(|name| (name, scenario.inputs.get(name)));
        match (port.value, sample) {
            (PortType::Control, None | Some((_, Some(Value::Bool(true))))) => {
                step.outputs.insert(port.name.clone(), control_output());
            }
            (PortType::Control, Some((_, Some(Value::Bool(false))))) => {}
            (PortType::Control, Some((name, Some(_)))) => {
                anyhow::bail!("Sample control input {name} needs a Boolean")
            }
            (PortType::Data(kind), Some((_, Some(value)))) => {
                step.outputs.insert(
                    port.name.clone(),
                    TraceOutput {
                        port_type: port.value,
                        value: Some(json!({ "kind": kind, "evidence": value })),
                    },
                );
            }
            (_, Some((name, None))) => {
                step.outputs.clear();
                step.reason = Some(format!("Waiting for sample input {name}."));
                return Ok(step);
            }
            _ => {}
        }
    }
    step.state = TraceState::Completed;
    Ok(step)
}

fn control_output() -> TraceOutput {
    TraceOutput {
        port_type: PortType::Control,
        value: None,
    }
}

fn command_sample(node: &str, format: CommandOutputFormat, value: Value) -> Result<Value> {
    ensure!(
        format != CommandOutputFormat::Text || value.is_string(),
        "Text Command {node} needs a text result sample"
    );
    Ok(
        json!({ "runId": format!("simulation:{node}"), "output": { "format": format, "value": value } }),
    )
}

fn simulate_single(
    member: &PackageMember,
    scenario: &WorkflowSimulationScenario,
    command: Option<CommandOutputFormat>,
) -> Result<TraceStep> {
    ensure!(
        scenario.outcomes.keys().all(|node| node == &member.key),
        "The sample outcome must use the member key {}",
        member.key
    );
    let mut step = TraceStep {
        node_id: member.key.clone(),
        title: member.name.clone(),
        state: TraceState::Waiting,
        inputs: Vec::new(),
        outputs: BTreeMap::new(),
        reason: Some("Waiting for a sample outcome.".into()),
    };
    if let Some(outcome) = scenario.outcomes.get(&member.key) {
        ensure!(
            outcome.state != TraceState::Skipped,
            "Branch selection determines skipped stages"
        );
        step.state = outcome.state;
        step.reason.clone_from(&outcome.reason);
        if outcome.state == TraceState::Completed {
            let result = outcome
                .outputs
                .get("result")
                .context("A completed sample needs a result output")?
                .clone();
            ensure!(
                outcome.outputs.len() == 1,
                "A tool sample produces one result output"
            );
            let value = command
                .map(|format| command_sample(&member.key, format, result.clone()))
                .transpose()?
                .unwrap_or(result);
            let port_type = if command.is_some() {
                PortType::Data(crate::workflows::ArtifactKind::CommandResult)
            } else {
                PortType::Evidence
            };
            step.outputs.insert(
                "result".into(),
                TraceOutput {
                    port_type,
                    value: Some(value),
                },
            );
        } else {
            ensure!(
                outcome.outputs.is_empty(),
                "A failed or waiting stage cannot supply data outputs"
            );
        }
    }
    Ok(step)
}
