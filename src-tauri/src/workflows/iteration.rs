use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, Effect, Endpoint, NodeConfiguration, NodeInstance,
    Port, PortType, Wire, WorkflowArtifact, WorkflowGraph, builtin_registry,
    compile_resolved_graph,
    subflow::{self, BoundSubflow},
};

pub const MAX_FOR_EACH_ITEMS: u16 = 32;
const MAX_LIST_BYTES: usize = 192 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildListConfiguration {
    Values { values: Vec<Value> },
    Evidence { pointer: String },
}

impl BuildListConfiguration {
    pub(super) fn valid(&self) -> bool {
        match self {
            Self::Values { values } => validate_values(values).is_ok(),
            Self::Evidence { pointer } => {
                pointer.starts_with('/')
                    && pointer.len() <= 2048
                    && pointer
                        .split('~')
                        .skip(1)
                        .all(|part| part.starts_with(['0', '1']))
            }
        }
    }

    pub(super) fn requires_evidence(&self) -> bool {
        matches!(self, Self::Evidence { .. })
    }

    pub(super) fn build(&self, evidence: Option<&WorkflowArtifact>) -> Result<WorkflowJsonList> {
        let encoded = evidence
            .map(|source| serde_json::to_value(&source.value))
            .transpose()?;
        self.build_from_value(
            evidence
                .zip(encoded.as_ref())
                .map(|(source, value)| (source.id.as_str(), value)),
        )
    }

    pub(super) fn build_from_value(
        &self,
        evidence: Option<(&str, &Value)>,
    ) -> Result<WorkflowJsonList> {
        let Some((source_id, value)) = evidence else {
            let Self::Values { values } = self else {
                anyhow::bail!("The list has no pinned evidence source");
            };
            return WorkflowJsonList::new(values.clone(), None);
        };
        let Self::Evidence { pointer } = self else {
            anyhow::bail!("Unexpected evidence for a configured list");
        };
        let values = value
            .pointer(pointer)
            .and_then(Value::as_array)
            .cloned()
            .context("The list pointer must select a JSON array")?;
        WorkflowJsonList::new(values, Some(source_id.into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachConfiguration {
    pub max_items: u16,
    pub ready_input: String,
    pub item_input: String,
    pub completion_output: String,
    pub result_output: String,
}

impl ForEachConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_limit(self.max_items)
            && self.ready_input != self.item_input
            && self.completion_output != self.result_output
            && [
                &self.ready_input,
                &self.item_input,
                &self.completion_output,
                &self.result_output,
            ]
            .iter()
            .all(|name| !name.trim().is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachBeginConfiguration {
    pub max_items: u16,
}

impl ForEachBeginConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_limit(self.max_items)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachGateConfiguration {
    pub ordinal: u16,
}

impl ForEachGateConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_limit(self.ordinal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForEachCollectConfiguration {
    pub max_items: u16,
    pub result_kind: ArtifactKind,
}

impl ForEachCollectConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_limit(self.max_items)
    }
}

fn valid_limit(limit: u16) -> bool {
    (1..=MAX_FOR_EACH_ITEMS).contains(&limit)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowJsonList {
    pub values: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_artifact_id: Option<String>,
}

impl WorkflowJsonList {
    pub fn new(values: Vec<Value>, source_artifact_id: Option<String>) -> Result<Self> {
        let list = Self {
            values,
            source_artifact_id,
        };
        list.validate()?;
        Ok(list)
    }

    pub(super) fn validate(&self) -> Result<()> {
        validate_values(&self.values)
    }
}

fn validate_values(values: &[Value]) -> Result<()> {
    ensure!(
        values.len() <= usize::from(MAX_FOR_EACH_ITEMS),
        "List exceeds the maximum supported item count"
    );
    ensure!(
        serde_json::to_vec(values)?.len() <= MAX_LIST_BYTES,
        "List exceeds the workflow data size limit"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowJsonItem {
    pub list_artifact_id: String,
    pub ordinal: u16,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowIterationResults {
    pub list_artifact_id: String,
    pub result_kind: ArtifactKind,
    pub results: Vec<String>,
}

pub(super) fn list_value(artifact: &WorkflowArtifact) -> Result<&WorkflowJsonList> {
    let ArtifactValue::JsonList(list) = &artifact.value else {
        anyhow::bail!("For each needs a typed JSON list artifact");
    };
    Ok(list)
}

pub(super) fn bind_list(
    artifact: &WorkflowArtifact,
    max_items: u16,
) -> Result<Option<WorkflowJsonList>> {
    ensure!(valid_limit(max_items), "Invalid For each item limit");
    let list = list_value(artifact)?;
    list.validate()?;
    if list.values.len() > usize::from(max_items) {
        return Ok(None);
    }
    let bounded = WorkflowJsonList::new(list.values.clone(), Some(artifact.id.clone()))?;
    Ok(Some(bounded))
}

pub(super) fn item_for(list: &WorkflowArtifact, ordinal: u16) -> Result<Option<WorkflowJsonItem>> {
    ensure!(valid_limit(ordinal), "Invalid For each item ordinal");
    Ok(list_value(list)?
        .values
        .get(usize::from(ordinal - 1))
        .map(|value| WorkflowJsonItem {
            list_artifact_id: list.id.clone(),
            ordinal,
            value: value.clone(),
        }))
}

pub(super) fn collect_results<'a>(
    list: &WorkflowArtifact,
    configuration: &ForEachCollectConfiguration,
    results: impl IntoIterator<Item = (u16, &'a WorkflowArtifact)>,
) -> Result<WorkflowIterationResults> {
    ensure!(configuration.valid(), "Invalid For each collection limit");
    let count = list_value(list)?.values.len();
    ensure!(
        count <= usize::from(configuration.max_items),
        "The pinned list exceeds the For each item limit"
    );
    let mut ordered = vec![None; count];
    for (ordinal, artifact) in results {
        ensure!(
            artifact.workflow_run_id == list.workflow_run_id && artifact.unit_id == list.unit_id,
            "Iteration result is outside the pinned workflow scope"
        );
        ensure!(
            artifact.value.kind() == configuration.result_kind,
            "Iteration result has an incompatible type"
        );
        let index = ordinal.checked_sub(1).map(usize::from);
        let slot = index
            .and_then(|index| ordered.get_mut(index))
            .context("Unexpected For each result ordinal")?;
        ensure!(
            slot.replace(artifact.id.clone()).is_none(),
            "For each has multiple results for one ordinal"
        );
    }
    let results = ordered
        .into_iter()
        .collect::<Option<_>>()
        .context("For each is missing results")?;
    Ok(WorkflowIterationResults {
        list_artifact_id: list.id.clone(),
        result_kind: configuration.result_kind,
        results,
    })
}

pub(super) fn expand_for_each(
    body: BoundSubflow,
    configuration: &ForEachConfiguration,
) -> Result<BoundSubflow> {
    ensure!(
        configuration.valid(),
        "A For each needs typed ports and 1 to 32 items"
    );
    let count = usize::from(configuration.max_items);
    ensure!(
        body.workflow
            .nodes
            .len()
            .saturating_add(2)
            .saturating_mul(count)
            + 3
            <= 4096,
        "The For each body exceeds the expanded workflow size limit"
    );
    subflow::validate_interface(&body.workflow, &body.inputs, &body.outputs)?;
    let (ready, item, completed, result) = (
        body.inputs
            .get(&configuration.ready_input)
            .context("For each ready input is unavailable in the pinned body")?,
        body.inputs
            .get(&configuration.item_input)
            .context("For each item input is unavailable in the pinned body")?,
        body.outputs
            .get(&configuration.completion_output)
            .context("For each completion output is unavailable in the pinned body")?,
        body.outputs
            .get(&configuration.result_output)
            .context("For each result output is unavailable in the pinned body")?,
    );
    ensure!(
        subflow::output(&body.workflow, ready)? == PortType::Control,
        "For each ready input must be a control port"
    );
    ensure!(
        subflow::output(&body.workflow, item)? == PortType::Data(ArtifactKind::JsonItem),
        "For each item input must receive a typed JSON item"
    );
    ensure!(
        subflow::output(&body.workflow, completed)? == PortType::Control,
        "For each completion output must be a control port"
    );
    let PortType::Data(result_kind) = subflow::output(&body.workflow, result)? else {
        anyhow::bail!("For each result output must have a data type");
    };
    ensure!(
        body.inputs.keys().filter(|name| *name != &configuration.ready_input && *name != &configuration.item_input)
            .all(|name| !matches!(name.as_str(), "ready" | "items")),
        "Public For each ports ready and items are reserved"
    );
    validate_loop_body(&body, ready, Some(item), completed, result)?;

    let mut start_ports = vec![
        Port {
            name: "ready".into(),
            value: PortType::Control,
        },
        Port {
            name: "items".into(),
            value: PortType::Data(ArtifactKind::JsonList),
        },
    ];
    for (name, port) in &body.inputs {
        if *name != configuration.item_input && *name != configuration.ready_input {
            start_ports.push(Port {
                name: name.clone(),
                value: subflow::output(&body.workflow, port)?,
            });
        }
    }
    let name = format!("For each {}", body.name);
    let mut graph = WorkflowGraph {
        name: name.clone(),
        nodes: vec![
            node(
                "start",
                BuiltinNode::Start,
                NodeConfiguration::SubflowEntry {
                    outputs: start_ports.clone(),
                },
            ),
            node(
                "begin",
                BuiltinNode::ForEachBegin,
                NodeConfiguration::ForEachBegin(ForEachBeginConfiguration {
                    max_items: configuration.max_items,
                }),
            ),
            node(
                "collect",
                BuiltinNode::ForEachCollect,
                NodeConfiguration::ForEachCollect(ForEachCollectConfiguration {
                    max_items: configuration.max_items,
                    result_kind,
                }),
            ),
        ],
        wires: vec![
            wire("start", "ready", "begin", "ready"),
            wire("start", "items", "begin", "items"),
            wire("begin", "ready", "collect", "ready"),
            wire("begin", "items", "collect", "items"),
        ],
    };
    let mut calls = BTreeMap::new();
    let mut prior_ready = endpoint("begin", "ready");
    let mut prior_result = endpoint("begin", "items");
    for ordinal in 1..=configuration.max_items {
        let gate = format!("gate-{ordinal}");
        let iteration = format!("iteration-{ordinal}");
        graph.nodes.push(node(
            &gate,
            BuiltinNode::ForEachGate,
            NodeConfiguration::ForEachGate(ForEachGateConfiguration { ordinal }),
        ));
        graph.nodes.push(node(
            &iteration,
            BuiltinNode::Subflow,
            NodeConfiguration::None,
        ));
        graph.wires.extend([
            Wire {
                from: prior_ready,
                to: endpoint(&gate, "ready"),
            },
            wire("begin", "items", &gate, "items"),
            Wire {
                from: prior_result,
                to: endpoint(&gate, "previous"),
            },
            wire(&gate, "iterate", &iteration, &configuration.ready_input),
            wire(&gate, "item", &iteration, &configuration.item_input),
            wire(
                &iteration,
                &configuration.completion_output,
                "collect",
                "finished",
            ),
            wire(
                &iteration,
                &configuration.result_output,
                "collect",
                "result",
            ),
        ]);
        for input in body.inputs.keys().filter(|port| {
            *port != &configuration.ready_input && *port != &configuration.item_input
        }) {
            graph.wires.push(wire("start", input, &iteration, input));
        }
        prior_ready = endpoint(&iteration, &configuration.completion_output);
        prior_result = endpoint(&iteration, &configuration.result_output);
        calls.insert(iteration, body.clone());
    }
    let registry: HashMap<_, _> = builtin_registry()
        .into_iter()
        .map(|pin| (pin.kind, pin))
        .collect();
    let definitions = graph
        .nodes
        .iter()
        .map(|node| {
            Ok((
                node.id.clone(),
                subflow::entry_definition(node, &registry[&node.kind])?,
            ))
        })
        .collect::<Result<_>>()?;
    let exported = [
        ("completed".into(), endpoint("collect", "completed")),
        ("failed".into(), endpoint("begin", "failed")),
        ("results".into(), endpoint("collect", "results")),
    ]
    .into();
    let expanded = subflow::expand(graph, definitions, calls, &exported)?;
    let mut workflow = compile_resolved_graph(
        body.version.id.clone(),
        expanded.graph,
        |node| expanded.definitions.get(&node.id),
        false,
    )?;
    workflow.subflows = expanded.subflows;
    let inputs = start_ports
        .into_iter()
        .map(|port| (port.name.clone(), endpoint("start", &port.name)))
        .collect();
    subflow::validate_interface(&workflow, &inputs, &expanded.outputs)?;
    Ok(BoundSubflow {
        name,
        version: body.version,
        workflow,
        inputs,
        outputs: expanded.outputs,
    })
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

fn wire(source: &str, output: &str, target: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(source, output),
        to: endpoint(target, input),
    }
}

pub(super) fn validate_loop_body(
    body: &BoundSubflow,
    ready: &Endpoint,
    item: Option<&Endpoint>,
    completion: &Endpoint,
    result: &Endpoint,
) -> Result<()> {
    let (loop_name, gate) = if item.is_some() {
        ("For each", "item")
    } else {
        ("Repeat", "ready")
    };
    let root = ready.node.as_str();
    let kinds: HashMap<_, _> = body
        .workflow
        .nodes
        .iter()
        .map(|node| (node.instance.id.as_str(), &node.definition))
        .collect();
    ensure!(
        item.is_none() || kinds.values().all(|node| !node.kind.per_unit()),
        "For each currently needs a body outside a logical delivery unit"
    );
    let mut incoming: HashMap<&str, Vec<&Wire>> = HashMap::new();
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    for wire in &body.workflow.wires {
        incoming
            .entry(wire.to.node.as_str())
            .or_default()
            .push(wire);
        outgoing
            .entry(wire.from.node.as_str())
            .or_default()
            .push(wire.to.node.as_str());
    }
    let dependencies = |node| incoming.get(node).map(Vec::as_slice).unwrap_or_default();
    let mut remaining: HashMap<_, _> = kinds
        .keys()
        .map(|id| (*id, dependencies(*id).len()))
        .collect();
    let mut queue: VecDeque<_> = remaining
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut gated = HashSet::new();
    let source_gated = |source: &Endpoint, gated: &HashSet<&str>| {
        source == ready || Some(source) == item || gated.contains(source.node.as_str())
    };
    let has_effect = |definition: &super::NodeDefinition| {
        !matches!(definition.effect, Effect::None | Effect::ReadEvidence)
    };
    let mut visited = 0;
    while let Some(node) = queue.pop_front() {
        visited += 1;
        if node != root {
            let mut required = dependencies(node)
                .iter()
                .filter(|wire| match kinds[node].kind {
                    BuiltinNode::ForEachCollect => {
                        matches!(wire.to.port.as_str(), "ready" | "items")
                    }
                    BuiltinNode::RepeatOutput => kinds[node]
                        .inputs
                        .iter()
                        .any(|port| port.name == wire.to.port && port.value == PortType::Control),
                    _ => true,
                });
            let dominated = if matches!(
                kinds[node].kind,
                BuiltinNode::Join | BuiltinNode::RepeatOutput
            ) {
                required
                    .next()
                    .is_some_and(|wire| source_gated(&wire.from, &gated))
                    && required.all(|wire| source_gated(&wire.from, &gated))
            } else {
                required.any(|wire| source_gated(&wire.from, &gated))
            };
            if dominated {
                gated.insert(node);
            } else {
                ensure!(
                    item.is_none() && !has_effect(kinds[node]),
                    "{loop_name} body node {node} can run outside its {gate} gate"
                );
            }
        }
        for dependent in outgoing.get(node).into_iter().flatten() {
            let count = remaining
                .get_mut(dependent)
                .with_context(|| format!("{loop_name} body has an unknown node"))?;
            *count -= 1;
            if *count == 0 {
                queue.push_back(dependent);
            }
        }
    }
    ensure!(visited == kinds.len(), "{loop_name} body contains a cycle");
    ensure!(
        source_gated(completion, &gated) && source_gated(result, &gated),
        "{loop_name} outputs need to depend on their {gate} gate"
    );

    // A selected ordinary output proves that every input was selected. A Join only
    // proves its inputs settled, and an ordinary skipped node may still await an input.
    let mut waiting = vec![
        (completion.node.as_str(), true),
        (result.node.as_str(), true),
    ];
    let mut visited = HashSet::new();
    while let Some((node, selected)) = waiting.pop() {
        if !visited.insert((node, selected)) {
            continue;
        }
        let waits_for_settlement = matches!(
            kinds[node].kind,
            BuiltinNode::Join | BuiltinNode::ForEachCollect | BuiltinNode::RepeatOutput
        );
        if selected || waits_for_settlement || dependencies(node).len() == 1 {
            waiting.extend(
                dependencies(node)
                    .iter()
                    .filter(|wire| {
                        kinds[node].kind != BuiltinNode::RepeatOutput
                            || kinds[node].inputs.iter().any(|port| {
                                port.name == wire.to.port && port.value == PortType::Control
                            })
                    })
                    .map(|wire| (wire.from.node.as_str(), selected && !waits_for_settlement)),
            );
        }
    }
    for (node, definition) in kinds {
        if has_effect(definition) {
            ensure!(
                visited.contains(&(node, true)) || visited.contains(&(node, false)),
                "{loop_name} outputs can settle before body operation {node}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
