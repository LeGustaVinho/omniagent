use crate::workflows::{
    ArtifactKind, WorkflowDefinition, builtin_registry, compile_resolved_graph,
};

use super::super::PackageArtifactVersion;
use super::*;

struct Fixture {
    member: PackageMember,
    definitions: BTreeMap<String, NodeDefinition>,
    effective: BTreeMap<String, NodeDefinition>,
}

impl Fixture {
    fn new(nodes: Vec<NodeInstance>, wires: Vec<Wire>) -> Self {
        let registry = builtin_registry();
        let definitions = nodes
            .iter()
            .map(|node| {
                let definition = registry
                    .iter()
                    .find(|definition| definition.kind == node.kind)
                    .unwrap();
                (node.id.clone(), definition.clone())
            })
            .collect::<BTreeMap<_, _>>();
        let mut member = PackageMember {
            key: "source".into(),
            name: "Saved source".into(),
            destination: PackageDestination::Project,
            base: None,
            body: PackageArtifactBody::Pipeline {
                graph: WorkflowGraph {
                    name: "Saved source".into(),
                    nodes,
                    wires,
                },
            },
            references: BTreeMap::new(),
        };
        member.references.insert(
            "discarded".into(),
            PackageReference::Draft {
                key: "unrelated".into(),
            },
        );
        Self {
            member,
            effective: definitions.clone(),
            definitions,
        }
    }

    fn source(&self) -> DerivationSource<'_> {
        DerivationSource {
            member: &self.member,
            members: std::slice::from_ref(&self.member),
            definitions: &self.definitions,
            effective_definitions: &self.effective,
        }
    }
}

#[test]
fn derives_selection_from_exact_pins_and_resolved_subflow_ports() -> Result<()> {
    let mut fixture = Fixture::new(
        vec![
            node("start", BuiltinNode::Start),
            node("load", BuiltinNode::LoadContext),
            node("nested", BuiltinNode::Subflow),
            node("outside", BuiltinNode::Join),
        ],
        vec![
            link("start", "start", "load", "ready"),
            link("start", "document", "load", "document"),
            link("load", "loaded", "nested", "go"),
            link("load", "context", "nested", "document"),
            link("nested", "success", "outside", "branches"),
        ],
    );
    let pinned_child = PackageReference::Published {
        version: PackageArtifactVersion {
            destination: PackageDestination::Library,
            kind: PackageArtifactKind::Subflow,
            id: "selected-child".into(),
            revision: "existing-child-revision".into(),
        },
    };
    fixture
        .member
        .references
        .insert("nested".into(), pinned_child.clone());
    fixture.definitions.get_mut("load").unwrap().revision = 4;
    let nested = fixture.effective.get_mut("nested").unwrap();
    nested.inputs = vec![
        port("go", PortType::Control),
        port("document", PortType::Data(ArtifactKind::Markdown)),
    ];
    nested.outputs = vec![
        port("success", PortType::Control),
        port("report", PortType::Data(ArtifactKind::Review)),
    ];
    let selected = ["load".into(), "nested".into()];
    let source = fixture.source();
    let boundary = describe_subflow_boundary(&source, Some(&selected))?;
    assert_eq!(boundary.inputs.len(), 2);
    assert!(
        boundary
            .outputs
            .iter()
            .any(|output| output.endpoint == endpoint("nested", "success")
                && output.consumed_outside_selection)
    );
    assert!(
        boundary
            .outputs
            .iter()
            .any(|output| output.endpoint == endpoint("nested", "report")
                && output.value == PortType::Data(ArtifactKind::Review))
    );
    let request = |inputs, outputs| ReusableDerivation {
        key: "reusable".into(),
        name: "Reusable selection".into(),
        destination: PackageDestination::Library,
        selection: ReusableSelection::Subflow {
            node_ids: Some(selected.to_vec()),
            inputs,
            outputs,
        },
    };
    let inputs = BTreeMap::from([
        (
            "ready".into(),
            vec![link("start", "start", "load", "ready")],
        ),
        (
            "brief".into(),
            vec![link("start", "document", "load", "document")],
        ),
    ]);
    let outputs = BTreeMap::from([
        ("done".into(), endpoint("nested", "success")),
        ("review".into(), endpoint("nested", "report")),
    ]);
    let derived = derive_member(&source, request(inputs.clone(), outputs.clone()))?;
    assert_eq!(derived.definitions["load"].revision, 4);
    assert!(derived.definitions["nested"].outputs.is_empty());
    assert_eq!(
        derived.member.references,
        BTreeMap::from([("nested".into(), pinned_child)])
    );
    let PackageArtifactBody::Subflow {
        graph,
        inputs: public_inputs,
        outputs: public_outputs,
    } = derived.member.body
    else {
        unreachable!()
    };
    assert_eq!(
        graph
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["entry", "load", "nested"]
    );
    assert_eq!(public_inputs["brief"], endpoint("entry", "brief"));
    assert_eq!(public_outputs, outputs);
    assert!(
        graph
            .wires
            .contains(&link("entry", "brief", "load", "document"))
    );
    assert!(
        graph
            .wires
            .contains(&link("load", "context", "nested", "document"))
    );
    assert!(
        derive_member(
            &source,
            request(
                BTreeMap::from([("ready".into(), inputs["ready"].clone())]),
                outputs.clone()
            )
        )
        .is_err()
    );
    assert!(
        derive_member(
            &source,
            request(
                inputs.clone(),
                BTreeMap::from([("bad".into(), endpoint("outside", "joined"))])
            )
        )
        .is_err()
    );
    let mut forged = inputs;
    forged.insert(
        "forged".into(),
        vec![link("start", "document", "nested", "document")],
    );
    assert!(derive_member(&source, request(forged, outputs)).is_err());
    Ok(())
}

#[test]
fn whole_pipeline_and_join_boundary_produce_valid_typed_subflows() -> Result<()> {
    let fixture = Fixture::new(
        vec![
            node("start", BuiltinNode::Start),
            node("entry", BuiltinNode::Parallel),
            node("right", BuiltinNode::Parallel),
            node("join", BuiltinNode::Join),
        ],
        vec![
            link("start", "start", "entry", "ready"),
            link("start", "start", "right", "ready"),
            link("entry", "branches", "join", "branches"),
            link("right", "branches", "join", "branches"),
        ],
    );
    let source = fixture.source();
    let derive = |nodes, inputs| {
        derive_member(
            &source,
            ReusableDerivation {
                key: "derived".into(),
                name: "Join reviewed branches".into(),
                destination: PackageDestination::Project,
                selection: ReusableSelection::Subflow {
                    node_ids: nodes,
                    inputs,
                    outputs: BTreeMap::from([("done".into(), endpoint("join", "joined"))]),
                },
            },
        )
    };
    let whole = derive(
        None,
        BTreeMap::from([(
            "ready".into(),
            vec![
                link("start", "start", "entry", "ready"),
                link("start", "start", "right", "ready"),
            ],
        )]),
    )?;
    let compiled = validate(&whole)?;
    assert_eq!(compiled.nodes[0].instance.id, "entry-2");
    assert_eq!(compiled.wires.len(), 4);
    let join_inputs = BTreeMap::from([
        (
            "a".into(),
            vec![link("entry", "branches", "join", "branches")],
        ),
        (
            "b".into(),
            vec![link("right", "branches", "join", "branches")],
        ),
    ]);
    let partial = derive(Some(vec!["join".into()]), join_inputs.clone())?;
    assert_eq!(validate(&partial)?.wires.len(), 2);
    let errors = derive(
        Some(vec!["join".into()]),
        BTreeMap::from([(
            "combined".into(),
            join_inputs.into_values().flatten().collect(),
        )]),
    );
    assert!(errors.is_err());
    Ok(())
}

#[test]
fn preset_preserves_fields_and_exact_role_reference_and_checks_destination() -> Result<()> {
    let mut plan = node("plan", BuiltinNode::Plan);
    plan.configuration = NodeConfiguration::AgentRole {
        role: "REVIEW-RISK".into(),
        model: None,
        requirements: Default::default(),
    };
    let mut fixture = Fixture::new(
        vec![node("start", BuiltinNode::Start), plan.clone()],
        vec![],
    );
    let project_role = PackageReference::Published {
        version: PackageArtifactVersion {
            destination: PackageDestination::Project,
            kind: PackageArtifactKind::Role,
            id: "project-role".into(),
            revision: "pinned".into(),
        },
    };
    fixture
        .member
        .references
        .insert("plan".into(), project_role.clone());
    let derive = |destination, editable_fields| {
        derive_member(
            &fixture.source(),
            ReusableDerivation {
                key: "review-plan".into(),
                name: "Review risk".into(),
                destination,
                selection: ReusableSelection::Preset {
                    node_id: "plan".into(),
                    editable_fields,
                },
            },
        )
    };
    let preset = derive(PackageDestination::Project, vec!["role".into()])?;
    assert_eq!(preset.member.references["$preset"], project_role);
    assert_eq!(preset.definitions["$preset"], fixture.definitions["plan"]);
    assert_eq!(
        preset.member.body,
        PackageArtifactBody::Preset {
            node_kind: BuiltinNode::Plan,
            configuration: plan.configuration,
            editable_fields: vec!["role".into()],
        }
    );
    assert!(derive(PackageDestination::Library, vec!["role".into()]).is_err());
    assert!(derive(PackageDestination::Project, vec!["kind".into()]).is_err());
    assert!(
        derive(
            PackageDestination::Project,
            vec!["role".into(), "role".into()]
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn library_preset_keeps_the_named_mcp_slot_and_refuses_local_registration_or_values() -> Result<()>
{
    let mut configuration = crate::workflows::mcp_tools::McpToolConfiguration {
        server_id: uuid::Uuid::nil().to_string(),
        server_revision: "0".repeat(64),
        contract_revision: "1".repeat(64),
        tool_name: "check".into(),
        arguments: Default::default(),
        argument_mappings: Default::default(),
        destination_pointer: None,
    };
    let parameter = PackageReference::Parameter {
        name: "Review tool".into(),
        kind: PackageArtifactKind::McpTool,
    };
    let fixture = |config| {
        let mut tool = node("tool", BuiltinNode::McpTool);
        tool.configuration = NodeConfiguration::McpTool(config);
        let mut fixture = Fixture::new(vec![node("start", BuiltinNode::Start), tool], vec![]);
        fixture
            .member
            .references
            .insert("tool".into(), parameter.clone());
        fixture
    };
    let derive = |fixture: &Fixture| {
        derive_member(
            &fixture.source(),
            ReusableDerivation {
                key: "portable".into(),
                name: "Portable tool".into(),
                destination: PackageDestination::Library,
                selection: ReusableSelection::Preset {
                    node_id: "tool".into(),
                    editable_fields: vec![],
                },
            },
        )
    };
    let named = derive(&fixture(configuration.clone()))?;
    assert_eq!(named.member.references["$preset"], parameter);
    configuration
        .arguments
        .insert("destination".into(), serde_json::json!("local-only"));
    assert!(derive(&fixture(configuration.clone())).is_err());
    configuration.arguments.clear();
    configuration.server_id = uuid::Uuid::new_v4().to_string();
    assert!(derive(&fixture(configuration)).is_err());
    Ok(())
}

fn validate(derived: &DerivedReusable) -> Result<WorkflowDefinition> {
    let PackageArtifactBody::Subflow {
        graph,
        inputs,
        outputs,
    } = &derived.member.body
    else {
        unreachable!()
    };
    let entry = &graph.nodes[0];
    let mut definitions = derived.definitions.clone();
    definitions.insert(
        entry.id.clone(),
        crate::workflows::subflow::entry_definition(entry, &definitions[&entry.id])?,
    );
    let compiled = compile_resolved_graph(
        "derived".into(),
        graph.clone(),
        |node| definitions.get(&node.id),
        false,
    )?;
    crate::workflows::subflow::validate_interface(&compiled, inputs, outputs)?;
    Ok(compiled)
}

fn node(id: &str, kind: BuiltinNode) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration: NodeConfiguration::None,
    }
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn link(node: &str, output: &str, target: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(node, output),
        to: endpoint(target, input),
    }
}

fn port(name: &str, value: PortType) -> Port {
    Port {
        name: name.into(),
        value,
    }
}
