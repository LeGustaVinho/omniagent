use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json};

use crate::workflows::{
    ArtifactKind, ArtifactValue, BuiltinNode, CommandOutputFormat, Endpoint, NodeConfiguration,
    PinnedNode, PortType, WorkflowArtifact, WorkflowDefinition, WorkflowIterationResults,
    iteration,
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
    #[serde(rename = "failurePort", skip_serializing_if = "Option::is_none")]
    failure_port: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<TraceGraph>,
    subflows: Vec<TraceSubflow>,
    outputs: BTreeMap<String, TraceExport>,
}

#[derive(Serialize)]
struct TraceGraph {
    nodes: Vec<TraceGraphNode>,
    wires: Vec<crate::workflows::Wire>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    layout: BTreeMap<String, WorkflowNodePosition>,
}

#[derive(Serialize)]
struct TraceGraphNode {
    id: String,
    title: String,
    inputs: Vec<crate::workflows::Port>,
    outputs: Vec<crate::workflows::Port>,
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
    #[serde(skip)]
    failed: bool,
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
            let (steps, subflows, outputs, graph) = match &member.body {
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
                    let graph = TraceGraph {
                        nodes: definition
                            .nodes
                            .into_iter()
                            .map(|node| TraceGraphNode {
                                id: node.instance.id,
                                title: node.definition.title,
                                inputs: node.definition.inputs,
                                outputs: node.definition.outputs,
                            })
                            .collect(),
                        wires: definition.wires,
                        layout: document
                            .layouts
                            .get(&member_key)
                            .cloned()
                            .unwrap_or_default(),
                    };
                    (steps, subflows, outputs, Some(graph))
                }
                PackageArtifactBody::Command { output, .. } => (
                    vec![simulate_single(member, &scenario, Some(*output))?],
                    Vec::new(),
                    BTreeMap::new(),
                    None,
                ),
                PackageArtifactBody::McpTool { .. } => (
                    vec![simulate_single(member, &scenario, None)?],
                    Vec::new(),
                    BTreeMap::new(),
                    None,
                ),
                _ => anyhow::bail!("The selected artifact has no simulation entry point"),
            };
            Ok(WorkflowSimulation {
                revision,
                member_key,
                inputs: scenario.inputs,
                steps,
                graph,
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
                || (step.state == TraceState::Failed && !step.outputs.is_empty()) =>
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
    let mut delivery_attempts = 0;
    while let Some(index) = ready.pop_front() {
        let node = &definition.nodes[index];
        let mut data = BTreeMap::new();
        let mut result_ordinal = 0;
        let mut evidence_ordinal = 0;
        let inputs = incoming[index]
            .iter()
            .map(|wire| {
                let key = if wire.to.port == "result"
                    && node.instance.kind == BuiltinNode::ForEachCollect
                {
                    result_ordinal += 1;
                    format!("result.{result_ordinal}")
                } else if wire.to.port == "evidence"
                    && node.instance.kind == BuiltinNode::CollectEvidence
                {
                    evidence_ordinal += 1;
                    format!("evidence.{evidence_ordinal}")
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
                let failed = source.is_some_and(|step| step.state == TraceState::Failed);
                let signal = if failed
                    && node.instance.kind == BuiltinNode::CollectEvidence
                    && wire.to.port == "evidence"
                {
                    Signal::Skipped
                } else {
                    signal(source, &wire.from.port)
                };
                TraceInput {
                    port: wire.to.port.clone(),
                    from: wire.from.clone(),
                    state: signal,
                    failed,
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
            delivery_attempts,
        )?;
        if node.instance.kind == BuiltinNode::DeliveryAttempt
            && step.outputs.contains_key("admitted")
        {
            delivery_attempts += 1;
        }
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
            | BuiltinNode::SetFields
            | BuiltinNode::ValidateOutput
            | BuiltinNode::CollectEvidence
            | BuiltinNode::QualityGate
            | BuiltinNode::FilterMapList
            | BuiltinNode::BuildText
            | BuiltinNode::TypedStart
            | BuiltinNode::SummarizeResult
    ) || kind.generated_iteration()
}

fn trace_node(
    node: &PinnedNode,
    body: &PackageArtifactBody,
    scenario: &WorkflowSimulationScenario,
    inputs: Vec<TraceInput>,
    data: &BTreeMap<String, &Value>,
    routed_failure: bool,
    delivery_attempts: u16,
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
    if let NodeConfiguration::RepeatOutput(configuration) = &node.instance.configuration {
        use crate::workflows::repetition::{
            RepeatOutputConfiguration, RepeatSelection, RepeatSignal,
        };
        match configuration.select(|port| {
            match step
                .inputs
                .iter()
                .find(|input| input.port == port)
                .map(|input| input.state)
            {
                Some(Signal::Selected) => RepeatSignal::Selected,
                Some(Signal::Skipped) => RepeatSignal::Skipped,
                _ => RepeatSignal::Waiting,
            }
        })? {
            RepeatSelection::Waiting => {
                step.reason = Some("Waiting for the approved iteration and its exports.".into());
            }
            RepeatSelection::Skipped => step.state = TraceState::Skipped,
            RepeatSelection::MissingOutput => {
                return Ok(failed_iteration(
                    step,
                    "The approved iteration did not produce every configured export",
                ));
            }
            RepeatSelection::Approved(round) => {
                step.state = TraceState::Completed;
                step.outputs.insert("completed".into(), control_output());
                for (name, kind) in &configuration.exports {
                    let input = RepeatOutputConfiguration::value_port(round, name);
                    let value = data
                        .get(&input)
                        .context("The approved iteration lost its simulated export")?;
                    step.outputs.insert(
                        name.clone(),
                        TraceOutput {
                            port_type: PortType::Data(*kind),
                            value: Some((*value).clone()),
                        },
                    );
                }
            }
        }
        return Ok(step);
    }
    let selected_list = (kind == ForEachCollect)
        .then(|| simulated_list(&step.inputs, data))
        .transpose()?
        .flatten();
    let count = selected_list
        .as_ref()
        .map(iteration::list_value)
        .transpose()?
        .map_or(0, |list| list.values.len());
    let mut finished = 0;
    let mut results = 0;
    let relevant: Vec<_> = step
        .inputs
        .iter()
        .filter(|input| {
            if kind != ForEachCollect {
                return true;
            }
            match input.port.as_str() {
                "finished" => {
                    finished += 1;
                    finished <= count
                }
                "result" => {
                    results += 1;
                    results <= count
                }
                _ => true,
            }
        })
        .collect();
    let skipped = relevant.iter().any(|input| {
        input.state == Signal::Skipped && (kind != CollectEvidence || input.port == "ready")
    });
    let waiting = relevant.iter().any(|input| input.state == Signal::Waiting);
    let selected = relevant.iter().any(|input| input.state == Signal::Selected);
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
        Start | TypedStart => return trace_start(step, node, body, scenario),
        QualityGate => {
            let NodeConfiguration::QualityGate(config) = &node.instance.configuration else {
                anyhow::bail!("Invalid pinned quality gate");
            };
            let ArtifactValue::EvidenceSet(evidence) =
                serde_json::from_value((*data["evidence"]).clone())?
            else {
                anyhow::bail!("The quality gate lost its sample evidence set");
            };
            let (port, report) = config.evaluate(&evidence)?;
            values.insert(
                "decision".into(),
                serde_json::to_value(ArtifactValue::JsonObject(report))?,
            );
            Some(port)
        }
        LoadContext => {
            values.insert("context".into(), (*data["document"]).clone());
            Some("loaded")
        }
        Result => {
            values.insert("delivery".into(), (*data["git"]).clone());
            Some("completed")
        }
        SummarizeResult => {
            let summary = super::super::super::data_nodes::data_view(data["summary"])
                .as_str()
                .context("Result requires text")?;
            let artifacts = step
                .inputs
                .iter()
                .filter(|input| input.port == "evidence" && input.state == Signal::Selected)
                .map(|input| simulation_artifact_id(&input.from))
                .collect();
            values.insert(
                "summary".into(),
                serde_json::to_value(ArtifactValue::ResultSummary {
                    summary: summary.into(),
                    artifacts,
                })?,
            );
            Some("completed")
        }
        CollectEvidence => {
            use super::super::super::data_nodes::{
                EvidenceOutcome, WorkflowEvidence, evidence_text,
            };
            let evidence = step
                .inputs
                .iter()
                .filter(|input| input.port == "evidence")
                .enumerate()
                .map(|(index, input)| {
                    let outcome = match input.state {
                        Signal::Selected => EvidenceOutcome::Completed {
                            artifact_id: simulation_artifact_id(&input.from),
                            value: data
                                .get(&format!("evidence.{}", index + 1))
                                .map(|value| (*value).clone())
                                .context("The selected sample evidence is missing")?,
                        },
                        Signal::Skipped if input.failed => EvidenceOutcome::Failed,
                        Signal::Skipped => EvidenceOutcome::Skipped,
                        Signal::Waiting => anyhow::bail!("The sample evidence is still pending"),
                    };
                    Ok(WorkflowEvidence {
                        source: input.from.clone(),
                        outcome,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let text = match evidence_text(&evidence) {
                Ok(text) => text,
                Err(error) => return Ok(failed_iteration(step, &error.to_string())),
            };
            values.insert(
                "text".into(),
                serde_json::to_value(ArtifactValue::RenderedText(text))?,
            );
            values.insert(
                "evidence".into(),
                serde_json::to_value(ArtifactValue::EvidenceSet(evidence))?,
            );
            Some("completed")
        }
        SetFields | FilterMapList | BuildText | ValidateOutput => {
            use super::super::super::data_nodes::data_view;
            let transformed = match &node.instance.configuration {
                NodeConfiguration::ValidateOutput(config) => (|| {
                    let source = data.get("source").context("The sample output is missing")?;
                    let payload = source
                        .get("evidence")
                        .context("The sample output has no value")?;
                    let values = match source.get("kind").and_then(Value::as_str) {
                        Some("agent_response" | "rendered_text") => config.parse(
                            payload
                                .as_str()
                                .context("Enter the sample response as JSON text")?,
                        )?,
                        Some("json_object") => config.evaluate(payload.clone())?,
                        Some("command_result") => {
                            let output: super::super::super::WorkflowCommandResult =
                                serde_json::from_value(payload.clone())?;
                            match output.output {
                                super::super::super::command::CommandData::Json(value) => {
                                    config.evaluate(value)?
                                }
                                super::super::super::command::CommandData::Text(text) => {
                                    config.parse(&text)?
                                }
                            }
                        }
                        _ => anyhow::bail!(
                            "Sample a saved Agent response, JSON object, text, or Command result"
                        ),
                    };
                    Ok(("object", ArtifactValue::JsonObject(values)))
                })(),
                NodeConfiguration::SetFields(config) => config
                    .evaluate(data.get("source").copied().map(data_view))
                    .map(|value| ("object", ArtifactValue::JsonObject(value))),
                NodeConfiguration::FilterMapList(config) => (|| {
                    let items = simulated_list(&step.inputs, data)?
                        .context("The list input is unavailable")?;
                    Ok((
                        "items",
                        ArtifactValue::JsonList(
                            config.evaluate(iteration::list_value(&items)?, &items.id)?,
                        ),
                    ))
                })(),
                NodeConfiguration::BuildText(config) => (|| {
                    let fields = data
                        .get("values")
                        .map(|fields| {
                            data_view(fields)
                                .as_object()
                                .context("The named template inputs must be an object")
                        })
                        .transpose()?;
                    Ok((
                        "text",
                        ArtifactValue::RenderedText(config.evaluate(fields)?),
                    ))
                })(),
                _ => anyhow::bail!("The node has an invalid transform configuration"),
            };
            match transformed {
                Ok((port, value)) => {
                    values.insert(port.into(), serde_json::to_value(value)?);
                    Some("completed")
                }
                Err(error) => {
                    step.state = TraceState::Failed;
                    step.reason = Some(crate::diagnostics::preview(&error.to_string()));
                    step.outputs.insert("failed".into(), control_output());
                    return Ok(step);
                }
            }
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
        DeliveryAttempt => {
            let NodeConfiguration::DeliveryAttempt { limit, .. } = node.instance.configuration
            else {
                anyhow::bail!("Missing pinned delivery attempt budget");
            };
            if delivery_attempts >= limit {
                step.state = TraceState::Failed;
                step.reason = Some("The task reached its shared delivery attempt limit.".into());
                step.outputs.insert("exhausted".into(), control_output());
                return Ok(step);
            }
            Some("admitted")
        }
        IterationLimit => {
            step.state = TraceState::Failed;
            step.reason = Some("The workflow reached its configured iteration limit.".into());
            step.outputs.insert("failed".into(), control_output());
            return Ok(step);
        }
        BuildList => {
            let NodeConfiguration::BuildList(configuration) = &node.instance.configuration else {
                anyhow::bail!("Missing pinned list configuration");
            };
            let id = step
                .inputs
                .iter()
                .find(|input| input.port == "value")
                .map(|input| simulation_artifact_id(&input.from));
            match configuration.build_from_value(id.as_deref().zip(data.get("value").copied())) {
                Ok(list) => {
                    values.insert(
                        "items".into(),
                        serde_json::to_value(ArtifactValue::JsonList(list))?,
                    );
                    Some("completed")
                }
                Err(_) => {
                    return Ok(failed_iteration(
                        step,
                        "The configured list source did not produce a bounded JSON array.",
                    ));
                }
            }
        }
        ForEachBegin => {
            let NodeConfiguration::ForEachBegin(configuration) = &node.instance.configuration
            else {
                anyhow::bail!("Missing pinned iteration size");
            };
            let source = simulated_list(&step.inputs, data)?
                .context("The simulation has no iteration list")?;
            let Some(list) = iteration::bind_list(&source, configuration.max_items)? else {
                return Ok(failed_iteration(
                    step,
                    "The list exceeds the configured For each limit.",
                ));
            };
            values.insert(
                "items".into(),
                serde_json::to_value(ArtifactValue::JsonList(list))?,
            );
            Some("ready")
        }
        ForEachGate => {
            let NodeConfiguration::ForEachGate(configuration) = &node.instance.configuration else {
                anyhow::bail!("Missing pinned iteration ordinal");
            };
            let source = simulated_list(&step.inputs, data)?
                .context("The simulation has no iteration list")?;
            let Some(item) = iteration::item_for(&source, configuration.ordinal)? else {
                step.state = TraceState::Skipped;
                step.reason = Some("The sample list has no item at this ordinal.".into());
                return Ok(step);
            };
            values.insert(
                "item".into(),
                serde_json::to_value(ArtifactValue::JsonItem(item))?,
            );
            Some("iterate")
        }
        ForEachCollect => {
            let NodeConfiguration::ForEachCollect(configuration) = &node.instance.configuration
            else {
                anyhow::bail!("Missing pinned iteration result type");
            };
            let source = selected_list.context("The simulation has no iteration list")?;
            let results = relevant
                .iter()
                .filter(|input| input.port == "result")
                .enumerate()
                .map(|(index, input)| {
                    let sample = data
                        .get(&format!("result.{}", index + 1))
                        .context("The simulation is missing an iteration result")?;
                    ensure!(
                        serde_json::from_value::<ArtifactKind>(sample["kind"].clone())?
                            == configuration.result_kind,
                        "The simulated iteration result has an incompatible type"
                    );
                    Ok(simulation_artifact_id(&input.from))
                })
                .collect::<anyhow::Result<_>>()?;
            let result = WorkflowIterationResults {
                list_artifact_id: source.id,
                result_kind: configuration.result_kind,
                results,
            };
            values.insert(
                "results".into(),
                serde_json::to_value(ArtifactValue::IterationResults(result))?,
            );
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
            if let Some(port) = &outcome.failure_port {
                ensure!(
                    outcome.state == TraceState::Failed
                        && matches!(port.as_str(), "failed" | "stale" | "blocked")
                        && node.definition.outputs.iter().any(|output| output.value == PortType::Control && output.name == *port),
                    "Choose a declared failure port for the failed sample"
                );
            }
            if outcome.state != TraceState::Completed {
                ensure!(
                    outcome.outputs.is_empty(),
                    "A failed or waiting stage cannot supply data outputs"
                );
                if let Some(port) = &outcome.failure_port {
                    step.outputs.insert(port.clone(), control_output());
                } else if outcome.state == TraceState::Failed && routed_failure {
                    step.outputs.insert("failed".into(), control_output());
                }
                return Ok(step);
            }
            values.clone_from(&outcome.outputs);
            if matches!(kind, RequestInput | Approval) {
                Some(super::super::super::decision_nodes::simulate_stage_decision(node, &values)?)
            } else if kind == VerifyTestPhase {
                let verdict = values
                    .get("evidence")
                    .and_then(|sample| sample.pointer("/result/verdict"))
                    .and_then(Value::as_str);
                Some(match verdict {
                    Some("verified") => "verified",
                    Some("failed") => "failed",
                    Some("blocked") => "blocked",
                    _ => anyhow::bail!("A test phase sample needs evidence.result.verdict"),
                })
            } else if kind == VerifyIntegratedChecks {
                let verdict = values
                    .get("evidence")
                    .and_then(|sample| sample.get("verdict"))
                    .and_then(Value::as_str);
                Some(match verdict {
                    Some("verified") => "verified",
                    Some("failed") => "failed",
                    Some("blocked") => "blocked",
                    _ => anyhow::bail!("An integrated checks sample needs evidence.verdict"),
                })
            } else if kind == WaitForCi {
                let ci = values.get("ci").context("A CI sample needs its snapshot")?;
                Some(
                    match ci.pointer("/snapshot/state").and_then(Value::as_str) {
                        Some("passed")
                            if ci.pointer("/pull/requiresHuman").and_then(Value::as_bool)
                                == Some(true) =>
                        {
                            "approval_required"
                        }
                        Some("passed") => "passed",
                        Some("failed") => "failed",
                        _ => anyhow::bail!(
                            "A completed CI sample needs a passing or failed snapshot"
                        ),
                    },
                )
            } else {
                node.definition
                    .outputs
                    .iter()
                    .find(|port| port.value == PortType::Control && port.name != "failed")
                    .map(|port| port.name.as_str())
            }
        }
    };
    step.state = TraceState::Completed;
    if let Some(control) = control {
        step.outputs.insert(control.into(), control_output());
    }
    for port in &node.definition.outputs {
        if let PortType::Data(kind) = port.value {
            if matches!(&node.instance.configuration, NodeConfiguration::FindWork { query }
                if query.result == super::super::super::agile_nodes::FindWorkResultMode::List && matches!(port.name.as_str(), "task" | "target"))
            {
                continue;
            }
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

fn simulated_list(
    inputs: &[TraceInput],
    data: &BTreeMap<String, &Value>,
) -> Result<Option<WorkflowArtifact>> {
    let Some(value) = data.get("items") else {
        return Ok(None);
    };
    let from = &inputs
        .iter()
        .find(|input| input.port == "items")
        .context("Missing simulated list source")?
        .from;
    let artifact = WorkflowArtifact {
        id: simulation_artifact_id(from),
        workflow_run_id: "simulation".into(),
        node_instance_id: from.node.clone(),
        port: from.port.clone(),
        unit_id: None,
        value: serde_json::from_value((*value).clone())?,
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
        _ if node.instance.kind == BuiltinNode::TypedStart && port == "inputs" => Some("inputs"),
        _ if node.instance.kind == BuiltinNode::TypedStart && port == "task" => Some("task"),
        _ if node.instance.kind == BuiltinNode::TypedStart && port == "event" => Some("event"),
        _ if node.instance.kind == BuiltinNode::TypedStart && port == "plan" => Some("plan"),
        _ if port == "document" => Some("document"),
        _ => None,
    };
    let empty = serde_json::Map::new();
    let mut typed_values = None;
    if let NodeConfiguration::TypedStart(config) = &node.instance.configuration {
        let values = scenario
            .inputs
            .get("inputs")
            .map(|value| {
                value
                    .as_object()
                    .context("Sample inputs must be a named object")
            })
            .transpose()?
            .unwrap_or(&empty);
        let markdown = scenario
            .inputs
            .get("document")
            .filter(|_| config.prompt_or_markdown())
            .map(|sample| {
                sample
                    .as_str()
                    .context("Enter sample Markdown text for the Start request")
            })
            .transpose()?;
        typed_values = Some(Value::Object(config.launch_values(values, markdown)?));
    }
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
        if port.name == "document"
            && matches!(&node.instance.configuration, NodeConfiguration::TypedStart(config) if config.prompt_or_markdown())
        {
            continue;
        }
        let sample = input_name(&port.name).map(|name| {
            let value = if name == "inputs" {
                typed_values.as_ref().or_else(|| scenario.inputs.get(name))
            } else {
                scenario.inputs.get(name)
            };
            (name, value)
        });
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
            (_, Some((_, None))) if node.instance.kind == BuiltinNode::TypedStart => {}
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
