use crate::workflows::{
    ArtifactKind, CommandConfiguration, CommandOutputFormat,
    authoring::package::{PackageArtifactKind, PackageArtifactVersion, PackageDestination},
};

use super::*;

#[test]
fn repeat_gates_writers_and_waits_for_their_outcomes() -> Result<()> {
    let configuration = RepeatUntilConfiguration {
        max_iterations: 2,
        ready_input: "go".into(),
        completion_output: "done".into(),
        evidence_output: "evidence".into(),
        until: ConditionPredicate::Exists {
            pointer: String::new(),
        },
        feedback: None,
        exports: Vec::new(),
    };

    let outside_gate = expand_repeat(body("always", false)?, &configuration)
        .err()
        .context("The writer was accepted on the independent Start port")?;
    assert!(
        outside_gate
            .to_string()
            .contains("write can run outside its ready gate")
    );
    let early = expand_repeat(body("ready", false)?, &configuration)
        .err()
        .context("The repeat was allowed to complete before the writer")?;
    assert!(
        early
            .to_string()
            .contains("outputs can settle before body operation write")
    );

    let expanded = expand_repeat(body("ready", true)?, &configuration)?;
    assert!(expanded.workflow.wires.contains(&Wire {
        from: endpoint("condition-1", "unmatched"),
        to: endpoint("iteration-2::write", "ready"),
    }));
    assert!(expanded.workflow.wires.contains(&Wire {
        from: endpoint("start", "always"),
        to: endpoint("iteration-2::load", "ready"),
    }));
    Ok(())
}

fn body(writer_gate: &str, wait_for_writer: bool) -> Result<BoundSubflow> {
    let start_ports = [
        ("ready", PortType::Control),
        ("always", PortType::Control),
        ("document", PortType::Data(ArtifactKind::Markdown)),
        ("release", PortType::Data(ArtifactKind::Release)),
        ("worktree", PortType::Data(ArtifactKind::Worktree)),
    ];
    let instance = |id: &str, kind, configuration| NodeInstance {
        id: id.into(),
        kind,
        configuration,
    };
    let graph = WorkflowGraph {
        name: "Repeatable command".into(),
        nodes: vec![
            instance(
                "start",
                BuiltinNode::Start,
                NodeConfiguration::SubflowEntry {
                    outputs: start_ports
                        .iter()
                        .map(|(name, value)| Port {
                            name: (*name).into(),
                            value: *value,
                        })
                        .collect(),
                },
            ),
            instance("load", BuiltinNode::LoadContext, NodeConfiguration::None),
            instance(
                "write",
                BuiltinNode::Command,
                NodeConfiguration::Command(CommandConfiguration {
                    registration_id: "exact-command".into(),
                    registration_revision: "f".repeat(64),
                    arguments: vec![],
                    input: Default::default(),
                    output: CommandOutputFormat::Text,
                }),
            ),
        ],
        wires: [
            (
                if wait_for_writer { "always" } else { "ready" },
                "load",
                "ready",
            ),
            ("document", "load", "document"),
            (writer_gate, "write", "ready"),
            ("release", "write", "release"),
            ("worktree", "write", "worktree"),
        ]
        .into_iter()
        .map(|(source, target, port)| Wire {
            from: endpoint("start", source),
            to: endpoint(target, port),
        })
        .collect(),
    };
    let registry: HashMap<_, _> = builtin_registry()
        .into_iter()
        .map(|entry| (entry.kind, entry))
        .collect();
    let definitions: HashMap<_, _> = graph
        .nodes
        .iter()
        .map(|node| {
            Ok((
                node.id.clone(),
                subflow::entry_definition(node, &registry[&node.kind])?,
            ))
        })
        .collect::<Result<_>>()?;
    let workflow = compile_resolved_graph(
        "repeatable-command".into(),
        graph,
        |node| definitions.get(&node.id),
        false,
    )?;
    let inputs = start_ports
        .into_iter()
        .map(|(name, _)| {
            (
                if name == "ready" { "go" } else { name }.into(),
                endpoint("start", name),
            )
        })
        .collect();
    let outputs = if wait_for_writer {
        [
            ("done".into(), endpoint("write", "completed")),
            ("evidence".into(), endpoint("write", "result")),
        ]
    } else {
        [
            ("done".into(), endpoint("load", "loaded")),
            ("evidence".into(), endpoint("load", "context")),
        ]
    }
    .into();
    Ok(BoundSubflow {
        name: "Repeatable command".into(),
        version: PackageArtifactVersion {
            destination: PackageDestination::Project,
            kind: PackageArtifactKind::Subflow,
            id: workflow.version.id.clone(),
            revision: workflow.version.revision.clone(),
        },
        workflow,
        inputs,
        outputs,
    })
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}
