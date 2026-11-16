use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{
    BuiltinNode, ConfigurationKind, Endpoint, NodeConfiguration, NodeDefinition, NodeInstance,
    Port, PortType, Wire, WorkflowDefinition, WorkflowGraph,
    authoring::package::PackageArtifactVersion,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSubflow {
    pub path: String,
    pub name: String,
    pub version: PackageArtifactVersion,
    pub nodes: Vec<String>,
    pub outputs: BTreeMap<String, Endpoint>,
}

pub(super) struct BoundSubflow {
    pub name: String,
    pub version: PackageArtifactVersion,
    pub workflow: WorkflowDefinition,
    pub inputs: BTreeMap<String, Endpoint>,
    pub outputs: BTreeMap<String, Endpoint>,
}

pub(super) struct ExpandedGraph {
    pub graph: WorkflowGraph,
    pub definitions: BTreeMap<String, NodeDefinition>,
    pub subflows: Vec<WorkflowSubflow>,
    pub outputs: BTreeMap<String, Endpoint>,
}

pub(super) fn valid_entry_ports(ports: &[Port]) -> bool {
    !ports.is_empty()
        && ports
            .iter()
            .all(|port| !port.name.trim().is_empty() && port.value != PortType::Evidence)
        && ports
            .iter()
            .map(|port| &port.name)
            .collect::<HashSet<_>>()
            .len()
            == ports.len()
}

pub(super) fn entry_definition(
    node: &NodeInstance,
    definition: &NodeDefinition,
) -> Result<NodeDefinition> {
    let mut definition = definition.clone();
    if let NodeConfiguration::SubflowEntry { outputs } = &node.configuration {
        ensure!(
            node.kind == BuiltinNode::Start && valid_entry_ports(outputs),
            "A subflow entry needs unique typed output ports on Start"
        );
        definition.configuration = ConfigurationKind::SubflowEntry;
        definition.outputs.clone_from(outputs);
    }
    Ok(definition)
}

pub(super) fn validate_interface(
    definition: &WorkflowDefinition,
    inputs: &BTreeMap<String, Endpoint>,
    outputs: &BTreeMap<String, Endpoint>,
) -> Result<()> {
    let root = start(definition)?;
    let bindings: HashSet<_> = inputs.values().collect();
    ensure!(
        bindings.len() == inputs.len(),
        "Each Start port has a single public input"
    );
    for (name, endpoint) in inputs {
        ensure!(
            endpoint.node == root,
            "Subflow input {name} needs a Start output"
        );
        output(definition, endpoint)
            .with_context(|| format!("Subflow input {name} has no typed Start port"))?;
    }
    for (name, endpoint) in outputs {
        output(definition, endpoint)
            .with_context(|| format!("Subflow output {name} has no typed source"))?;
        ensure!(
            endpoint.node != root || bindings.contains(endpoint),
            "Subflow output {name} forwards an undeclared input"
        );
    }
    for wire in definition
        .wires
        .iter()
        .filter(|wire| wire.from.node == root)
    {
        ensure!(
            bindings.contains(&wire.from),
            "Expose Start port {} as a public subflow input",
            wire.from.port
        );
    }
    Ok(())
}

pub(super) fn expand(
    mut graph: WorkflowGraph,
    mut definitions: BTreeMap<String, NodeDefinition>,
    calls: BTreeMap<String, BoundSubflow>,
    exported: &BTreeMap<String, Endpoint>,
) -> Result<ExpandedGraph> {
    if calls.is_empty() {
        return Ok(ExpandedGraph {
            graph,
            definitions,
            subflows: Vec::new(),
            outputs: exported.clone(),
        });
    }
    ensure!(
        graph.nodes.iter().all(|node| !node.id.contains("::")),
        "Use node identifiers without :: in a graph that invokes subflows"
    );
    let expanded_size = graph.nodes.len()
        + calls
            .values()
            .map(|call| call.workflow.nodes.len())
            .sum::<usize>();
    ensure!(
        expanded_size <= 4096,
        "The expanded workflow exceeds the subflow size limit"
    );
    let mut input_sources = BTreeMap::new();
    for wire in graph
        .wires
        .iter()
        .filter(|wire| calls.contains_key(&wire.to.node))
    {
        let call = &calls[&wire.to.node];
        let target = call
            .inputs
            .get(&wire.to.port)
            .with_context(|| format!("Unknown subflow input {}.{}", wire.to.node, wire.to.port))?;
        let source_type = if let Some(upstream) = calls.get(&wire.from.node) {
            let upstream_port = upstream.outputs.get(&wire.from.port).with_context(|| {
                format!(
                    "Unknown subflow output {}.{}",
                    wire.from.node, wire.from.port
                )
            })?;
            output(&upstream.workflow, upstream_port)?
        } else {
            definitions
                .get(&wire.from.node)
                .and_then(|definition| {
                    definition
                        .outputs
                        .iter()
                        .find(|port| port.name == wire.from.port)
                })
                .with_context(|| {
                    format!(
                        "Unknown workflow output {}.{}",
                        wire.from.node, wire.from.port
                    )
                })?
                .value
        };
        ensure!(
            output(&call.workflow, target)?.accepts(source_type),
            "Subflow input {}.{} has an incompatible port type",
            wire.to.node,
            wire.to.port
        );
        ensure!(
            input_sources
                .insert((&wire.to.node, &wire.to.port), &wire.from)
                .is_none(),
            "Subflow input {}.{} accepts a single wire",
            wire.to.node,
            wire.to.port
        );
    }
    for (slot, call) in &calls {
        for public_input in call.inputs.keys() {
            ensure!(
                input_sources.contains_key(&(slot, public_input)),
                "Subflow input {slot}.{public_input} needs a connection"
            );
        }
    }
    let connector = Connector {
        calls: &calls,
        input_sources,
    };
    let outputs = exported
        .iter()
        .map(|(name, endpoint)| Ok((name.clone(), connector.source(endpoint)?)))
        .collect::<Result<_>>()?;
    let mut wires = graph
        .wires
        .iter()
        .filter(|wire| !calls.contains_key(&wire.to.node))
        .map(|wire| {
            Ok(Wire {
                from: connector.source(&wire.from)?,
                to: wire.to.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    graph.nodes.retain(|node| !calls.contains_key(&node.id));
    let mut scopes = Vec::new();
    for (slot, call) in &calls {
        definitions.remove(slot);
        let root = start(&call.workflow)?;
        let qualified = call
            .workflow
            .nodes
            .iter()
            .filter(|node| node.instance.id != root)
            .map(|pinned| {
                let mut node = pinned.instance.clone();
                node.id = qualify(slot, &node.id);
                definitions.insert(node.id.clone(), pinned.definition.clone());
                node
            })
            .collect::<Vec<_>>();
        let outputs = call
            .outputs
            .keys()
            .map(|port| {
                let source = connector.source(&Endpoint {
                    node: slot.clone(),
                    port: port.clone(),
                })?;
                Ok((port.clone(), source))
            })
            .collect::<Result<_>>()?;
        scopes.push(WorkflowSubflow {
            path: slot.clone(),
            name: call.name.clone(),
            version: call.version.clone(),
            nodes: qualified.iter().map(|node| node.id.clone()).collect(),
            outputs,
        });
        for nested in &call.workflow.subflows {
            scopes.push(WorkflowSubflow {
                path: qualify(slot, &nested.path),
                name: nested.name.clone(),
                version: nested.version.clone(),
                nodes: nested
                    .nodes
                    .iter()
                    .map(|node| qualify(slot, node))
                    .collect(),
                outputs: nested
                    .outputs
                    .iter()
                    .map(|(port, endpoint)| {
                        Ok((
                            port.clone(),
                            connector.child_source(slot, root, call, endpoint)?,
                        ))
                    })
                    .collect::<Result<_>>()?,
            });
        }
        graph.nodes.extend(qualified);
        for wire in &call.workflow.wires {
            wires.push(Wire {
                from: connector.child_source(slot, root, call, &wire.from)?,
                to: qualify_endpoint(slot, &wire.to),
            });
        }
    }
    graph.wires = wires;
    Ok(ExpandedGraph {
        graph,
        definitions,
        subflows: scopes,
        outputs,
    })
}

struct Connector<'a> {
    calls: &'a BTreeMap<String, BoundSubflow>,
    input_sources: BTreeMap<(&'a String, &'a String), &'a Endpoint>,
}

impl Connector<'_> {
    fn source(&self, start_from: &Endpoint) -> Result<Endpoint> {
        let mut from = start_from;
        let mut seen = HashSet::new();
        while let Some(call) = self.calls.get(&from.node) {
            ensure!(
                seen.insert(from),
                "Subflow output forwarding contains a cycle"
            );
            let endpoint = call
                .outputs
                .get(&from.port)
                .with_context(|| format!("Unknown subflow output {}.{}", from.node, from.port))?;
            if endpoint.node != start(&call.workflow)? {
                return Ok(qualify_endpoint(&from.node, endpoint));
            }
            from = self.input_binding(&from.node, call, endpoint)?;
        }
        Ok(from.clone())
    }

    fn child_source(
        &self,
        slot: &String,
        root: &str,
        call: &BoundSubflow,
        endpoint: &Endpoint,
    ) -> Result<Endpoint> {
        if endpoint.node == root {
            return self.source(self.input_binding(slot, call, endpoint)?);
        }
        Ok(qualify_endpoint(slot, endpoint))
    }

    fn input_binding(
        &self,
        slot: &String,
        call: &BoundSubflow,
        endpoint: &Endpoint,
    ) -> Result<&Endpoint> {
        let (port, _) = call
            .inputs
            .iter()
            .find(|(_, start)| *start == endpoint)
            .context("Subflow source is missing its public input")?;
        self.input_sources
            .get(&(slot, port))
            .copied()
            .context("Subflow source is missing its outer wire")
    }
}

fn qualify(slot: &str, node: &str) -> String {
    format!("{slot}::{node}")
}

fn qualify_endpoint(slot: &str, endpoint: &Endpoint) -> Endpoint {
    Endpoint {
        node: qualify(slot, &endpoint.node),
        port: endpoint.port.clone(),
    }
}

fn start(workflow: &WorkflowDefinition) -> Result<&str> {
    workflow
        .nodes
        .iter()
        .find(|node| node.instance.kind == BuiltinNode::Start)
        .map(|node| node.instance.id.as_str())
        .context("Subflow lost its Start entry")
}

fn output(workflow: &WorkflowDefinition, endpoint: &Endpoint) -> Result<PortType> {
    workflow
        .nodes
        .iter()
        .find(|node| node.instance.id == endpoint.node)
        .and_then(|node| {
            node.definition
                .outputs
                .iter()
                .find(|port| port.name == endpoint.port)
        })
        .map(|port| port.value)
        .context("The pinned subflow port is unavailable")
}
