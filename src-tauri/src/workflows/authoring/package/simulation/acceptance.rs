use std::path::Path;

use crate::workflows::{
    ArtifactKind, CommandConfiguration, ConditionPredicate, DeliveryRoles, Port, WorkflowGraph,
    builtin_delivery_graph, mcp_tools::McpToolConfiguration, tests::approved_delivery_repository,
};

use super::{
    tests::{endpoint, member, node, production_counts, wire},
    *,
};

fn port(name: &str, value: PortType) -> Port {
    Port {
        name: name.into(),
        value,
    }
}

fn fixture() -> WorkflowPackage {
    let mut outer = builtin_delivery_graph(DeliveryRoles::default());
    outer.nodes.retain(|node| {
        ["start", "context", "plan", "review-plan", "work"].contains(&node.id.as_str())
    });
    outer
        .wires
        .retain(|wire| outer.nodes.iter().any(|node| node.id == wire.to.node));
    outer.nodes.push(node(
        "delivery",
        BuiltinNode::Subflow,
        NodeConfiguration::None,
    ));
    outer.wires.extend([
        wire("work", "linked", "delivery", "ready"),
        wire("work", "task", "delivery", "task"),
        wire("plan", "plan", "delivery", "plan"),
    ]);

    let mut pipeline = member("pipeline", PackageArtifactBody::Pipeline { graph: outer });
    pipeline.references.insert(
        "delivery".into(),
        PackageReference::Draft {
            key: "reviewed-delivery".into(),
        },
    );
    let graph = WorkflowGraph {
        name: "Conditional delivery".into(),
        nodes: vec![
            node(
                "entry",
                BuiltinNode::Start,
                NodeConfiguration::SubflowEntry {
                    outputs: vec![
                        port("ready", PortType::Control),
                        port("plan", PortType::Data(ArtifactKind::Plan)),
                        port("task", PortType::Data(ArtifactKind::Task)),
                    ],
                },
            ),
            node(
                "select",
                BuiltinNode::Condition,
                NodeConfiguration::Condition {
                    predicate: ConditionPredicate::Equals {
                        pointer: "/evidence/taskId".into(),
                        value: json!("sample-task"),
                    },
                },
            ),
            node(
                "approval",
                BuiltinNode::ApproveImplementation,
                NodeConfiguration::None,
            ),
            node(
                "prepare",
                BuiltinNode::PrepareWorktree,
                NodeConfiguration::None,
            ),
            node(
                "check",
                BuiltinNode::Command,
                NodeConfiguration::Command(CommandConfiguration {
                    registration_id: "sample-only".into(),
                    registration_revision: "0".repeat(64),
                    arguments: Vec::new(),
                    input: Default::default(),
                    output: CommandOutputFormat::Json,
                }),
            ),
            node(
                "inspect",
                BuiltinNode::McpTool,
                NodeConfiguration::McpTool(McpToolConfiguration {
                    server_id: "00000000-0000-0000-0000-000000000000".into(),
                    server_revision: "0".repeat(64),
                    tool_name: "inspect_file".into(),
                    contract_revision: "1".repeat(64),
                    arguments: Default::default(),
                    argument_mappings: Default::default(),
                    destination_pointer: None,
                }),
            ),
            node("alternate", BuiltinNode::Parallel, NodeConfiguration::None),
            node("diagnostic", BuiltinNode::Parallel, NodeConfiguration::None),
        ],
        wires: vec![
            wire("entry", "ready", "select", "ready"),
            wire("entry", "task", "select", "value"),
            wire("select", "matched", "approval", "ready"),
            wire("entry", "task", "approval", "task"),
            wire("entry", "plan", "approval", "plan"),
            wire("approval", "approved", "prepare", "ready"),
            wire("entry", "task", "prepare", "task"),
            wire("approval", "decision", "prepare", "decision"),
            wire("prepare", "prepared", "check", "ready"),
            wire("prepare", "release", "check", "release"),
            wire("prepare", "worktree", "check", "worktree"),
            wire("check", "completed", "inspect", "ready"),
            wire("check", "failed", "diagnostic", "ready"),
            wire("select", "unmatched", "alternate", "ready"),
        ],
    };
    let child = member(
        "reviewed-delivery",
        PackageArtifactBody::Subflow {
            graph,
            inputs: ["ready", "plan", "task"]
                .into_iter()
                .map(|name| (name.into(), endpoint("entry", name)))
                .collect(),
            outputs: BTreeMap::from([
                ("test-result".into(), endpoint("check", "result")),
                ("inspection".into(), endpoint("inspect", "result")),
            ]),
        },
    );
    let tool = member(
        "local-mcp",
        PackageArtifactBody::McpTool {
            binding: "fixture-server".into(),
            tool: "inspect_file".into(),
            input_schema: json!({"type":"object", "properties":{"path":{"type":"string"}}})
                .as_object()
                .unwrap()
                .clone(),
            output_schema: None,
        },
    );
    WorkflowPackage {
        name: "Review before publication".into(),
        members: vec![pipeline, child, tool],
    }
}

fn scenario(approve: bool) -> Result<WorkflowSimulationScenario> {
    let mut sample = json!({ "inputs": {"document": {"relativePath":"brief.md"}}, "outcomes": {
        "plan": {"state":"completed", "outputs":{"plan":{"milestones":["Check repository"]}}},
        "review-plan": {"state":"completed", "outputs":{"review":{"passed":true}}},
        "work": {"state":"completed", "outputs":{"task":{"taskId":"sample-task"}}},
    } });
    if approve {
        sample["outcomes"]["delivery::approval"] =
            json!({ "state":"completed", "outputs":{"decision":{"sample":true}} });
        sample["outcomes"]["delivery::prepare"] = json!({ "state":"completed", "outputs":{"worktree":{"id":"sample-tree"}, "release":{"id":"sample-release"}} });
        sample["outcomes"]["delivery::check"] =
            json!({ "state":"completed", "outputs":{"result":{"passed":true}} });
        sample["outcomes"]["delivery::inspect"] =
            json!({ "state":"completed", "outputs":{"result":{"size":42}} });
    }
    Ok(serde_json::from_value(sample)?)
}

#[tokio::test]
async fn simulates_conditional_subflow_and_retains_the_saved_revision_without_production_writes()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("simulation.sqlite");
    let store = Store::open(db.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id;
    let original = store
        .create_workflow_package(project.clone(), None, fixture())
        .await?;
    let draft = original.id;
    let revision = original.revision;
    let before = production_counts(&store).await?;
    let file = Path::new(&repo.checkout_root).join("brief.md");
    let content = std::fs::read(&file)?;
    let preview = store
        .preview_workflow_package_revision(project.clone(), draft.clone(), revision.clone())
        .await?;
    let preview = serde_json::to_value(preview)?;
    assert_eq!(preview["members"][0]["simulatable"], true, "{preview}");
    let authored = &preview["members"][0]["graphDefinitions"]["delivery"];
    assert!(authored["inputs"].as_array().is_some_and(|ports|
        ports.iter().any(|port| port["name"] == "plan" && port["value"]["artifact"] == "plan")));
    assert!(authored["outputs"].as_array().is_some_and(|ports|
        ports.iter().any(|port| port["name"] == "test-result" && port["value"]["artifact"] == "command_result")));
    assert!(
        preview["members"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["path"] == "/body/graph/nodes/delivery")
    );
    assert_eq!(
        preview["members"][0]["dependencies"]["delivery"]["key"],
        "reviewed-delivery"
    );
    let simulate = |sample| {
        store.simulate_workflow_package_revision(
            project.clone(),
            draft.clone(),
            revision.clone(),
            "pipeline".into(),
            sample,
        )
    };
    let waiting = simulate(scenario(false)?).await?;
    let by_node = |trace: &WorkflowSimulation, id: &str| {
        trace
            .steps
            .iter()
            .find(|step| step.node_id == id)
            .map(|step| step.state)
    };
    assert_eq!(
        by_node(&waiting, "delivery::approval"),
        Some(TraceState::Waiting)
    );
    assert_eq!(
        by_node(&waiting, "delivery::check"),
        Some(TraceState::Waiting)
    );
    assert_eq!(
        by_node(&waiting, "delivery::alternate"),
        Some(TraceState::Skipped)
    );
    let ready = simulate(scenario(true)?).await?;
    assert_eq!(
        by_node(&ready, "delivery::select"),
        Some(TraceState::Completed)
    );
    assert_eq!(
        by_node(&ready, "delivery::approval"),
        Some(TraceState::Completed)
    );
    assert_eq!(
        by_node(&ready, "delivery::check"),
        Some(TraceState::Completed)
    );
    assert_eq!(
        by_node(&ready, "delivery::inspect"),
        Some(TraceState::Completed)
    );
    let mut failure = scenario(true)?;
    failure.outcomes.insert(
        "delivery::check".into(),
        SampleOutcome {
            state: TraceState::Failed,
            reason: Some("Sample exit code 1".into()),
            outputs: BTreeMap::new(),
        },
    );
    let failure = simulate(failure).await?;
    assert_eq!(
        by_node(&failure, "delivery::check"),
        Some(TraceState::Failed)
    );
    assert_eq!(
        by_node(&failure, "delivery::diagnostic"),
        Some(TraceState::Completed)
    );
    assert_eq!(
        by_node(&failure, "delivery::inspect"),
        Some(TraceState::Skipped)
    );
    assert_eq!(ready.subflows[0].path, "delivery");
    assert_eq!(
        ready.subflows[0].outputs["test-result"].from,
        endpoint("delivery::check", "result")
    );
    assert!(ready.subflows[0].outputs["test-result"].state == Signal::Selected);
    let tool = store.simulate_workflow_package_revision(project.clone(), draft.clone(), revision.clone(), "local-mcp".into(),
        serde_json::from_value(json!({"inputs":{"path":"brief.md"}, "outcomes":{"local-mcp":{"state":"completed", "outputs":{"result":{"size":42}}}}}))?).await?;
    assert_eq!(
        tool.steps[0].outputs["result"].value,
        Some(json!({"size":42}))
    );
    assert!(
        store
            .simulate_workflow_package_revision(
                "unrelated".into(),
                draft.clone(),
                revision.clone(),
                "pipeline".into(),
                scenario(true)?
            )
            .await
            .is_err()
    );
    assert_eq!(production_counts(&store).await?, before);
    assert_eq!(std::fs::read(file)?, content);

    let mut changed = fixture();
    changed.members[0].references.insert(
        "delivery".into(),
        PackageReference::Draft {
            key: "unavailable".into(),
        },
    );
    let mut concurrent = changed.clone();
    concurrent.name = "Concurrent package edit".into();
    let (first, second) = tokio::join!(
        store.save_workflow_package(project.clone(), draft.clone(), revision.clone(), changed),
        store.save_workflow_package(project.clone(), draft.clone(), revision.clone(), concurrent),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let next = first.or(second)?;
    let next_preview = serde_json::to_value(
        store
            .preview_workflow_package_revision(
                project.clone(),
                draft.clone(),
                next.revision.clone(),
            )
            .await?,
    )?;
    assert_eq!(next_preview["members"][0]["simulatable"], false);
    assert_eq!(next_preview["revisions"], json!([next.revision, revision]));
    let original_trace = serde_json::to_value(ready)?;
    drop(store);
    let reopened = Store::open(db)?;
    let trace = reopened
        .simulate_workflow_package_revision(
            project,
            draft,
            revision,
            "pipeline".into(),
            scenario(true)?,
        )
        .await?;
    assert_eq!(serde_json::to_value(trace)?, original_trace);
    Ok(())
}
