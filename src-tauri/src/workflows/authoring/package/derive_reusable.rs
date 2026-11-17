use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::workflows::{
    BuiltinNode, Endpoint, NodeConfiguration, NodeDefinition, NodeInstance, Port, PortType, Wire,
    WorkflowGraph,
    stage_requirements::{RequiredStageSource, StageSourcePath},
};

use super::{
    PackageArtifactBody, PackageArtifactKind, PackageDestination, PackageMember, PackageReference,
    bounded_name, roles,
};

mod overrides;
mod store;
#[cfg(test)]
mod store_tests;
#[cfg(test)]
mod tests;
use overrides::override_fields;
pub(super) use overrides::resolve_preset_configuration;
pub use store::{ReusableSource, WorkflowReusableNode};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReusableDerivation {
    pub key: String,
    pub name: String,
    pub destination: PackageDestination,
    pub selection: ReusableSelection,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReusableSelection {
    Preset {
        node_id: String,
        editable_fields: Vec<String>,
    },
    Subflow {
        // A whole saved graph uses all stages except Start.
        node_ids: Option<Vec<String>>,
        // Each public input groups one boundary source and the selected targets it fed.
        inputs: BTreeMap<String, Vec<Wire>>,
        outputs: BTreeMap<String, Endpoint>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubflowBoundary {
    pub inputs: Vec<BoundaryInput>,
    pub outputs: Vec<BoundaryOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryInput {
    pub wire: Wire,
    pub value: PortType,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryOutput {
    pub endpoint: Endpoint,
    pub value: PortType,
    pub consumed_outside_selection: bool,
}

/// Construct this from the authorized immutable source and its resolved authoring definitions.
pub(super) struct DerivationSource<'a> {
    pub member: &'a PackageMember,
    pub members: &'a [PackageMember],
    pub definitions: &'a BTreeMap<String, NodeDefinition>,
    pub effective_definitions: &'a BTreeMap<String, NodeDefinition>,
}

pub(super) struct DerivedReusable {
    pub member: PackageMember,
    pub definitions: BTreeMap<String, NodeDefinition>,
}

impl DerivationSource<'_> {
    fn graph(&self) -> Result<&WorkflowGraph> {
        match &self.member.body {
            PackageArtifactBody::Pipeline { graph }
            | PackageArtifactBody::Subflow { graph, .. } => Ok(graph),
            _ => anyhow::bail!("Choose a saved pipeline or subflow as the source"),
        }
    }

    fn definition(&self, node: &NodeInstance) -> Result<&NodeDefinition> {
        self.definitions
            .get(&node.id)
            .filter(|pin| pin.kind == node.kind)
            .with_context(|| format!("The selected node {} lost its pinned definition", node.id))
    }

    fn effective_port(&self, endpoint: &Endpoint, input: bool) -> Result<PortType> {
        let definition = self
            .effective_definitions
            .get(&endpoint.node)
            .with_context(|| format!("No resolved definition for {}", endpoint.node))?;
        let ports = if input {
            &definition.inputs
        } else {
            &definition.outputs
        };
        ports
            .iter()
            .find(|port| port.name == endpoint.port)
            .map(|port| port.value)
            .with_context(|| {
                format!(
                    "The pinned port {}.{} is unavailable",
                    endpoint.node, endpoint.port
                )
            })
    }

    fn reference(
        &self,
        node_id: &str,
        destination: PackageDestination,
    ) -> Result<Option<(PackageReference, PackageArtifactKind)>> {
        let Some(reference) = self.member.references.get(node_id) else {
            return Ok(None);
        };
        let (scope, kind) = match reference {
            PackageReference::Draft { key } => {
                let target = self
                    .members
                    .iter()
                    .find(|member| member.key == *key)
                    .with_context(|| format!("The exact draft dependency {key} is unavailable"))?;
                (Some(target.destination), target.body.kind())
            }
            PackageReference::Published { version } => (Some(version.destination), version.kind),
            PackageReference::Parameter { kind, .. } => (None, *kind),
        };
        ensure!(
            destination != PackageDestination::Library
                || scope != Some(PackageDestination::Project),
            "Resolve the project dependency of {node_id} to a library artifact or named parameter first"
        );
        Ok(Some((reference.clone(), kind)))
    }
}

pub(super) fn describe_subflow_boundary(
    source: &DerivationSource<'_>,
    node_ids: Option<&[String]>,
) -> Result<SubflowBoundary> {
    let graph = source.graph()?;
    let selected = select_nodes(graph, node_ids)?;
    describe_boundary(source, graph, &selected)
}

fn describe_boundary(
    source: &DerivationSource<'_>,
    graph: &WorkflowGraph,
    selected: &HashSet<&str>,
) -> Result<SubflowBoundary> {
    let mut outside = HashSet::new();
    let mut inputs = Vec::new();
    for wire in &graph.wires {
        match (
            selected.contains(wire.from.node.as_str()),
            selected.contains(wire.to.node.as_str()),
        ) {
            (false, true) => {
                let value = source.effective_port(&wire.from, false)?;
                ensure!(
                    value != PortType::Evidence
                        && source.effective_port(&wire.to, true)?.accepts(value),
                    "The selected boundary has no compatible concrete type for {}.{}",
                    wire.to.node,
                    wire.to.port
                );
                inputs.push(BoundaryInput {
                    wire: wire.clone(),
                    value,
                });
            }
            (true, false) => {
                outside.insert(&wire.from);
            }
            _ => {}
        }
    }
    let mut outputs = Vec::new();
    for node in graph
        .nodes
        .iter()
        .filter(|node| selected.contains(node.id.as_str()))
    {
        let definition = source
            .effective_definitions
            .get(&node.id)
            .filter(|pin| pin.kind == node.kind)
            .with_context(|| format!("The resolved definition of {} is unavailable", node.id))?;
        outputs.extend(definition.outputs.iter().map(|port| {
            let endpoint = Endpoint {
                node: node.id.clone(),
                port: port.name.clone(),
            };
            BoundaryOutput {
                consumed_outside_selection: outside.contains(&endpoint),
                endpoint,
                value: port.value,
            }
        }));
    }
    Ok(SubflowBoundary { inputs, outputs })
}

pub(super) fn derive_member(
    source: &DerivationSource<'_>,
    request: ReusableDerivation,
) -> Result<DerivedReusable> {
    bounded_name(&request.key)?;
    bounded_name(&request.name)?;
    ensure!(
        !source
            .members
            .iter()
            .any(|member| member.key == request.key),
        "The package already has an artifact with that key"
    );
    let (body, references, definitions) = match request.selection {
        ReusableSelection::Preset {
            node_id,
            editable_fields,
        } => derive_preset(source, &node_id, editable_fields, request.destination)?,
        ReusableSelection::Subflow {
            node_ids,
            inputs,
            outputs,
        } => derive_subflow(
            source,
            &request.name,
            node_ids.as_deref(),
            inputs,
            outputs,
            request.destination,
        )?,
    };
    roles::validate_authoring_body(&body, request.destination)?;
    Ok(DerivedReusable {
        member: PackageMember {
            key: request.key,
            name: request.name,
            destination: request.destination,
            base: None,
            body,
            references,
        },
        definitions,
    })
}

type DerivedBody = (
    PackageArtifactBody,
    BTreeMap<String, PackageReference>,
    BTreeMap<String, NodeDefinition>,
);

fn derive_preset(
    source: &DerivationSource<'_>,
    node_id: &str,
    editable_fields: Vec<String>,
    destination: PackageDestination,
) -> Result<DerivedBody> {
    let node = source
        .graph()?
        .nodes
        .iter()
        .find(|node| node.id == node_id)
        .context("Select a node in the saved source")?;
    ensure!(
        !matches!(
            node.kind,
            BuiltinNode::Start
                | BuiltinNode::Subflow
                | BuiltinNode::RepeatUntil
                | BuiltinNode::ForEach
        ) && !node.kind.generated_iteration(),
        "Save a configured native stage as a preset"
    );
    ensure!(
        node.kind != BuiltinNode::Command || destination == PackageDestination::Project,
        "Save a configured Command preset in its project or declare a named library Command"
    );
    let pin = source.definition(node)?;
    let reference = source.reference(&node.id, destination)?;
    ensure!(
        !matches!(reference, Some((_, PackageArtifactKind::Preset))),
        "Open the pinned preset to derive a new configuration from it"
    );
    if destination == PackageDestination::Library {
        validate_library_configuration(node, reference.as_ref().map(|(_, kind)| *kind))?;
    }
    ensure!(
        node.configuration.matches(pin.configuration),
        "The configured node is incomplete; bind the required fields before saving a preset"
    );
    let fields = serde_json::to_value(&node.configuration)?;
    let unique = editable_fields.iter().collect::<BTreeSet<_>>();
    ensure!(
        unique.len() == editable_fields.len()
            && editable_fields
                .iter()
                .all(|key| key != "kind" && fields.get(key).is_some()),
        "Choose unique editable fields from the configured node"
    );
    ensure!(
        override_fields(&node.configuration, &editable_fields).len() == editable_fields.len(),
        "The selected fields include a local binding or fixed configuration"
    );
    let references = reference
        .into_iter()
        .map(|(reference, _)| ("$preset".into(), reference))
        .collect();
    Ok((
        PackageArtifactBody::Preset {
            node_kind: node.kind,
            configuration: node.configuration.clone(),
            editable_fields,
        },
        references,
        BTreeMap::from([("$preset".into(), pin.clone())]),
    ))
}

fn derive_subflow(
    source: &DerivationSource<'_>,
    name: &str,
    node_ids: Option<&[String]>,
    inputs: BTreeMap<String, Vec<Wire>>,
    mut outputs: BTreeMap<String, Endpoint>,
    destination: PackageDestination,
) -> Result<DerivedBody> {
    let graph = source.graph()?;
    let selected = select_nodes(graph, node_ids)?;
    let boundary = describe_boundary(source, graph, &selected)?;
    ensure!(
        !inputs.is_empty() && !outputs.is_empty(),
        "Map public inputs and outputs for the reusable subflow"
    );
    let mut entry_definition = crate::workflows::builtin_registry()
        .into_iter()
        .find(|definition| definition.kind == BuiltinNode::Start)
        .context("The subflow entry definition is unavailable")?;
    let entry = (1..=selected.len() + 1)
        .map(|ordinal| {
            if ordinal == 1 {
                "entry".into()
            } else {
                format!("entry-{ordinal}")
            }
        })
        .find(|candidate: &String| !selected.contains(candidate.as_str()))
        .context("A distinct subflow entry could not be allocated")?;
    let mut remaining = boundary
        .inputs
        .into_iter()
        .map(|input| ((input.wire.from, input.wire.to), input.value))
        .collect::<HashMap<_, _>>();
    let mut wires = graph
        .wires
        .iter()
        .filter(|wire| {
            selected.contains(wire.from.node.as_str()) && selected.contains(wire.to.node.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut public_inputs = BTreeMap::new();
    let mut entry_ports = Vec::new();
    for (name, links) in inputs {
        bounded_name(&name)?;
        let first = links
            .first()
            .context("A public input needs at least one boundary wire")?;
        let value = *remaining
            .get(&(first.from.clone(), first.to.clone()))
            .context("A public input must use an unused selected boundary wire")?;
        let contract = source
            .effective_definitions
            .get(&first.from.node)
            .and_then(|definition| definition.output_schemas.get(&first.from.port));
        ensure!(
            links.iter().all(|wire| wire.from == first.from),
            "One public input cannot combine different upstream sources"
        );
        let endpoint = Endpoint {
            node: entry.clone(),
            port: name.clone(),
        };
        for wire in links {
            ensure!(
                remaining.remove(&(wire.from, wire.to.clone())) == Some(value),
                "A public input must use an unused selected boundary wire"
            );
            wires.push(Wire {
                from: endpoint.clone(),
                to: wire.to,
            });
        }
        public_inputs.insert(name.clone(), endpoint);
        if let Some(schema) = contract {
            entry_definition
                .output_schemas
                .insert(name.clone(), schema.clone());
        }
        entry_ports.push(Port { name, value });
    }
    ensure!(
        remaining.is_empty(),
        "Map every input crossing the selected subflow boundary"
    );
    let available = boundary
        .outputs
        .iter()
        .map(|port| &port.endpoint)
        .collect::<HashSet<_>>();
    for (name, endpoint) in &outputs {
        bounded_name(name)?;
        ensure!(
            available.contains(endpoint),
            "Public output {name} needs a typed port on a selected node"
        );
    }
    let mut nodes = vec![NodeInstance {
        id: entry.clone(),
        kind: BuiltinNode::Start,
        configuration: NodeConfiguration::SubflowEntry {
            outputs: entry_ports,
        },
    }];
    let mut definitions = BTreeMap::from([(entry, entry_definition)]);
    let mut references = BTreeMap::new();
    for node in graph
        .nodes
        .iter()
        .filter(|node| selected.contains(node.id.as_str()))
    {
        let reference = source.reference(&node.id, destination)?;
        if destination == PackageDestination::Library {
            validate_library_configuration(node, reference.as_ref().map(|(_, kind)| *kind))?;
        }
        if let Some((reference, _)) = reference {
            references.insert(node.id.clone(), reference);
        }
        for binding in [
            "schema",
            "role",
            "input_schema",
            "output_schema",
            "reviewer",
        ] {
            let slot = format!("{}:{binding}", node.id);
            if let Some((reference, _)) = source.reference(&slot, destination)? {
                references.insert(slot, reference);
            }
        }
        definitions.insert(node.id.clone(), source.definition(node)?.clone());
        nodes.push(node.clone());
    }
    if graph.nodes.iter().any(|node| node.kind == BuiltinNode::End) {
        let mut completion = outputs.iter_mut().filter(|(name, endpoint)| {
            !matches!(
                name.as_str(),
                "failed" | "rejected" | "expired" | "limit_reached"
            ) && source.effective_port(endpoint, false).ok() == Some(PortType::Control)
        });
        let (_, completed) = completion
            .next()
            .context("Choose the Function's completion port")?;
        ensure!(
            completion.next().is_none(),
            "Choose one completion port for Return"
        );
        let return_id = (1..=nodes.len() + 1)
            .map(|index| {
                if index == 1 {
                    "return".to_owned()
                } else {
                    format!("return-{index}")
                }
            })
            .find(|id| nodes.iter().all(|node| &node.id != id))
            .context("Missing Return ID")?;
        wires.push(Wire {
            from: completed.clone(),
            to: Endpoint {
                node: return_id.clone(),
                port: "ready".into(),
            },
        });
        *completed = Endpoint {
            node: return_id.clone(),
            port: "completed".into(),
        };
        nodes.push(NodeInstance {
            id: return_id.clone(),
            kind: BuiltinNode::End,
            configuration: NodeConfiguration::End,
        });
        let mut definition = crate::workflows::builtin_registry()
            .into_iter()
            .find(|definition| definition.kind == BuiltinNode::End)
            .context("The Return definition is unavailable")?;
        definition.title = "Return".into();
        definitions.insert(return_id, definition);
    }
    Ok((
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: name.into(),
                nodes,
                wires,
            },
            inputs: public_inputs,
            outputs,
        },
        references,
        definitions,
    ))
}

fn select_nodes<'a>(
    graph: &'a WorkflowGraph,
    node_ids: Option<&[String]>,
) -> Result<HashSet<&'a str>> {
    let found = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        found.len() == graph.nodes.len()
            && graph
                .nodes
                .iter()
                .filter(|node| matches!(node.kind, BuiltinNode::Start | BuiltinNode::TypedStart))
                .count()
                == 1,
        "A saved source needs distinct stages and one Start node"
    );
    let mut included = HashSet::new();
    if let Some(ids) = node_ids {
        for id in ids {
            let node = found
                .get(id.as_str())
                .context("The selection contains a node outside the saved graph")?;
            ensure!(
                included.insert(node.id.as_str()),
                "A selected node is repeated"
            );
        }
    } else {
        included.extend(found.keys().copied());
    }
    included.retain(|id| {
        !matches!(
            found[id].kind,
            BuiltinNode::Start | BuiltinNode::TypedStart | BuiltinNode::End
        )
    });
    ensure!(!included.is_empty(), "Select at least one configured stage");
    ensure!(
        included
            .iter()
            .all(|id| !id.contains("::") && !found[id].kind.generated_iteration()),
        "Select authored nodes before expansion to reuse a subflow"
    );
    Ok(included)
}

fn validate_library_configuration(
    node: &NodeInstance,
    reference: Option<PackageArtifactKind>,
) -> Result<()> {
    match &node.configuration {
        NodeConfiguration::Command(command) => {
            ensure!(
                reference == Some(PackageArtifactKind::Command)
                    && command.arguments.is_empty()
                    && command.input.is_empty()
                    && command.registration_revision.len() == 64
                    && command
                        .registration_revision
                        .bytes()
                        .all(|byte| byte == b'0')
                    && !command.registration_id.is_empty()
                    && command
                        .registration_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
                "Library Command {} needs a named binding without local configuration",
                node.id
            );
        }
        NodeConfiguration::McpTool(tool) => {
            ensure!(
                reference == Some(PackageArtifactKind::McpTool)
                    && tool.is_package_placeholder()
                    && tool.arguments.is_empty(),
                "Library MCP stage {} needs a named tool without local configuration",
                node.id
            );
        }
        NodeConfiguration::AgentRole { requirements, .. } => {
            ensure!(
                !requirements.stage.sources.iter().any(|source| matches!(
                    source,
                    RequiredStageSource::Instruction {
                        path: StageSourcePath::Absolute { .. },
                        ..
                    } | RequiredStageSource::Skill {
                        path: StageSourcePath::Absolute { .. },
                        ..
                    }
                ) || matches!(source, RequiredStageSource::Plugin { identity, .. }
                    if identity.install_path.is_some() || identity.source_location.is_some()
                )),
                "Configure local instruction, skill or plugin paths in the consuming project for {}",
                node.id
            );
        }
        _ => {}
    }
    Ok(())
}
