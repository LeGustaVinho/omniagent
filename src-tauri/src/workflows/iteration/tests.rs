use serde_json::json;

use super::*;
use crate::workflows::{
    authoring::package::{PackageArtifactKind, PackageArtifactVersion, PackageDestination},
    mcp_tools::McpToolConfiguration,
};

mod runtime;

fn body(ungated: bool, early_exports: bool) -> Result<BoundSubflow> {
    let mut ports = vec![
        Port {
            name: "ready".into(),
            value: PortType::Control,
        },
        Port {
            name: "item".into(),
            value: PortType::Data(ArtifactKind::JsonItem),
        },
    ];
    if ungated {
        ports.push(Port {
            name: "ungated".into(),
            value: PortType::Control,
        });
    }
    let mut nodes = vec![
        node(
            "start",
            BuiltinNode::Start,
            NodeConfiguration::SubflowEntry {
                outputs: ports.clone(),
            },
        ),
        node(
            "apply",
            BuiltinNode::McpTool,
            NodeConfiguration::McpTool(McpToolConfiguration {
                server_id: "00000000-0000-0000-0000-000000000000".into(),
                server_revision: "0".repeat(64),
                tool_name: "apply_fixture".into(),
                contract_revision: "1".repeat(64),
                arguments: Default::default(),
                argument_mappings: BTreeMap::from([("item".into(), "/evidence/value".into())]),
                destination_pointer: None,
            }),
        ),
    ];
    let mut wires = vec![
        wire("start", "ready", "apply", "ready"),
        wire("start", "item", "apply", "arguments"),
    ];
    if ungated {
        nodes.push(node("escape", BuiltinNode::Join, NodeConfiguration::None));
        wires.extend([
            wire("start", "ready", "escape", "branches"),
            wire("start", "ungated", "escape", "branches"),
        ]);
    }
    let definitions = builtin_registry()
        .into_iter()
        .map(|pin| (pin.kind, pin))
        .collect::<HashMap<_, _>>();
    let pins = nodes
        .iter()
        .map(|node| {
            Ok((
                node.id.clone(),
                subflow::entry_definition(node, &definitions[&node.kind])?,
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let workflow = compile_resolved_graph(
        "iteration-body".into(),
        WorkflowGraph {
            name: "Body".into(),
            nodes,
            wires,
        },
        |node| pins.get(&node.id),
        false,
    )?;
    let inputs = ports
        .into_iter()
        .map(|port| (port.name.clone(), endpoint("start", &port.name)))
        .collect();
    let outputs = if early_exports {
        [
            ("done".into(), endpoint("start", "ready")),
            ("result".into(), endpoint("start", "item")),
        ]
        .into()
    } else {
        [
            ("done".into(), endpoint("apply", "completed")),
            ("result".into(), endpoint("apply", "result")),
        ]
        .into()
    };
    Ok(BoundSubflow {
        name: "Fixture".into(),
        version: PackageArtifactVersion {
            id: "iteration-body".into(),
            kind: PackageArtifactKind::Subflow,
            destination: PackageDestination::Project,
            revision: "body-version-1".into(),
        },
        workflow,
        inputs,
        outputs,
    })
}

fn config(max_items: u16) -> ForEachConfiguration {
    ForEachConfiguration {
        max_items,
        ready_input: "ready".into(),
        item_input: "item".into(),
        completion_output: "done".into(),
        result_output: "result".into(),
    }
}

#[test]
fn expands_exact_child_pins_and_waits_for_preceding_completion_and_result() -> Result<()> {
    let body = body(false, false)?;
    let expansion = expand_for_each(body.clone(), &config(3))?;
    assert_eq!(
        expansion.workflow,
        expand_for_each(body, &config(3))?.workflow
    );
    assert_eq!(
        expansion.inputs,
        BTreeMap::from([
            ("ready".into(), endpoint("start", "ready")),
            ("items".into(), endpoint("start", "items")),
        ])
    );
    assert_eq!(expansion.outputs["results"], endpoint("collect", "results"));
    assert_eq!(expansion.outputs["failed"], endpoint("begin", "failed"));
    assert_eq!(expansion.workflow.subflows.len(), 3);
    for ordinal in 1..=3 {
        let path = format!("iteration-{ordinal}");
        let child = expansion
            .workflow
            .subflows
            .iter()
            .find(|subflow| subflow.path == path)
            .unwrap();
        assert_eq!(child.version.revision, "body-version-1");
        assert_eq!(child.nodes, [format!("{path}::apply")]);
        let step = format!("{path}::apply");
        let gate = format!("gate-{ordinal}");
        assert!(
            expansion
                .workflow
                .wires
                .contains(&wire(&gate, "iterate", &step, "ready"))
        );
        assert!(
            expansion
                .workflow
                .wires
                .contains(&wire(&gate, "item", &step, "arguments"))
        );
        assert!(expansion.workflow.wires.contains(&wire(
            &step,
            "completed",
            "collect",
            "finished"
        )));
        if ordinal > 1 {
            let previous = format!("iteration-{}::apply", ordinal - 1);
            assert!(expansion.workflow.wires.contains(&wire(
                &previous,
                "completed",
                &gate,
                "ready"
            )));
            assert!(
                expansion
                    .workflow
                    .wires
                    .contains(&wire(&previous, "result", &gate, "previous"))
            );
        }
    }
    assert_eq!(
        expansion
            .workflow
            .wires
            .iter()
            .filter(|edge| edge.to == endpoint("collect", "result"))
            .map(|edge| edge.from.node.as_str())
            .collect::<Vec<_>>(),
        [
            "iteration-1::apply",
            "iteration-2::apply",
            "iteration-3::apply"
        ]
    );
    Ok(())
}

#[test]
fn rejects_joins_that_bypass_the_gate_and_exports_that_bypass_unsettled_operations() -> Result<()> {
    let ungated = expand_for_each(body(true, false)?, &config(2))
        .err()
        .unwrap();
    assert!(
        ungated
            .to_string()
            .contains("escape can run outside its item gate")
    );
    let early = expand_for_each(body(false, true)?, &config(2))
        .err()
        .unwrap();
    assert!(
        early
            .to_string()
            .contains("outputs can settle before body operation apply")
    );
    assert!(expand_for_each(body(false, false)?, &config(0)).is_err());
    assert!(expand_for_each(body(false, false)?, &config(MAX_FOR_EACH_ITEMS + 1)).is_err());
    Ok(())
}

fn artifact(id: &str, value: ArtifactValue) -> WorkflowArtifact {
    WorkflowArtifact {
        id: id.into(),
        workflow_run_id: "workflow-run".into(),
        node_instance_id: "fixture".into(),
        port: "result".into(),
        unit_id: None,
        value,
    }
}

#[test]
fn extracts_exact_values_and_collects_owned_results_by_ordinal_even_with_duplicates() -> Result<()>
{
    let input = artifact(
        "list",
        ArtifactValue::JsonList(WorkflowJsonList::new(
            vec![json!(false), json!(false), json!({"count": 3})],
            None,
        )?),
    );
    assert!(bind_list(&input, 2)?.is_none());
    let pinned = artifact(
        "bounded",
        ArtifactValue::JsonList(bind_list(&input, 3)?.unwrap()),
    );
    let first = item_for(&pinned, 1)?.unwrap();
    let second = item_for(&pinned, 2)?.unwrap();
    let third = item_for(&pinned, 3)?.unwrap();
    assert_eq!(first.value, second.value);
    assert!(first != second);
    assert_eq!(third.value, json!({"count":3}));
    assert_eq!(third.list_artifact_id, "bounded");
    assert!(item_for(&pinned, 4)?.is_none());
    assert!(item_for(&pinned, 0).is_err());
    let first = artifact("first", ArtifactValue::JsonItem(first));
    let second = artifact("second", ArtifactValue::JsonItem(second));
    let third = artifact("third", ArtifactValue::JsonItem(third));
    let config = ForEachCollectConfiguration {
        max_items: 3,
        result_kind: ArtifactKind::JsonItem,
    };
    let gathered = collect_results(&pinned, &config, [(3, &third), (1, &first), (2, &second)])?;
    assert_eq!(gathered.results, ["first", "second", "third"]);
    assert_eq!(gathered.result_kind, ArtifactKind::JsonItem);
    assert_eq!(gathered.list_artifact_id, "bounded");
    assert!(collect_results(&pinned, &config, [(1, &first), (1, &second), (3, &third)]).is_err());
    assert!(collect_results(&pinned, &config, [(1, &first), (2, &second)]).is_err());
    assert!(collect_results(&pinned, &config, [(1, &first), (2, &pinned), (3, &third)]).is_err());
    let other = WorkflowArtifact {
        workflow_run_id: "another-run".into(),
        ..second
    };
    assert!(collect_results(&pinned, &config, [(1, &first), (2, &other), (3, &third)]).is_err());
    Ok(())
}

#[test]
fn empty_bound_list_has_no_item_and_has_a_completed_empty_collection() -> Result<()> {
    let source = artifact(
        "empty",
        ArtifactValue::JsonList(WorkflowJsonList::new(Vec::new(), None)?),
    );
    let bound = bind_list(&source, 1)?.unwrap();
    assert_eq!(bound.source_artifact_id.as_deref(), Some("empty"));
    let pinned = artifact("bounded", ArtifactValue::JsonList(bound));
    assert!(item_for(&pinned, 1)?.is_none());
    let config = ForEachCollectConfiguration {
        max_items: 1,
        result_kind: ArtifactKind::JsonItem,
    };
    assert!(collect_results(&pinned, &config, [])?.results.is_empty());
    assert!(
        WorkflowJsonList::new(vec![json!(0); usize::from(MAX_FOR_EACH_ITEMS) + 1], None).is_err()
    );
    assert!(WorkflowJsonList::new(vec![json!("x".repeat(MAX_LIST_BYTES))], None).is_err());
    Ok(())
}
