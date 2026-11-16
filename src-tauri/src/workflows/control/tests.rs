use std::path::Path;

use super::*;
use crate::workflows::{
    DeliveryRoles, ManualWorkflowLaunch, NodeInputs, NodeInstance, WorkflowGraph,
    WorkflowHostRequest, WorkflowHostSubmission, WorkflowLaunchSource, builtin_delivery_graph,
    compile, tests::approved_delivery_repository,
};

fn wire(source: &str, out: &str, target: &str, input: &str) -> Wire {
    Wire {
        from: Endpoint {
            node: source.into(),
            port: out.into(),
        },
        to: Endpoint {
            node: target.into(),
            port: input.into(),
        },
    }
}

fn node(id: &str, kind: BuiltinNode) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration: NodeConfiguration::None,
    }
}

#[tokio::test]
async fn selects_one_branch_and_retains_the_join_after_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("control.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let note = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let mut condition = node("select", BuiltinNode::Condition);
    condition.configuration = NodeConfiguration::Condition {
        predicate: ConditionPredicate::Equals {
            pointer: "/evidence/relativePath".into(),
            value: Value::String("brief.md".into()),
        },
    };
    let graph = WorkflowGraph {
        name: "Branch review".into(),
        nodes: vec![
            node("join", BuiltinNode::Join),
            node("unselected", BuiltinNode::LoadContext),
            node("selected", BuiltinNode::LoadContext),
            condition,
            node("start", BuiltinNode::Start),
        ],
        wires: vec![
            wire("start", "start", "select", "ready"),
            wire("start", "document", "select", "value"),
            wire("select", "matched", "selected", "ready"),
            wire("select", "unmatched", "unselected", "ready"),
            wire("start", "document", "selected", "document"),
            wire("start", "document", "unselected", "document"),
            wire("selected", "loaded", "join", "branches"),
            wire("unselected", "loaded", "join", "branches"),
        ],
    };
    let project = repo.project_id.clone();
    let version = store
        .save_workflow_revision(project.clone(), "branches".into(), graph)
        .await?;
    let request = ManualWorkflowLaunch {
        project_id: project.clone(),
        workflow: version,
        request_id: "control-test".into(),
        work_id: None,
        source: WorkflowLaunchSource {
            checkout_id: repo.checkout_id,
            revision_id: note.revision.id,
        },
    };
    let run = store.launch_workflow_manual(request.clone()).await?;
    let initial = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("missing graph")?;
    let source = initial
        .outputs
        .iter()
        .find(|output| output.artifact.node_instance_id == "start")
        .context("missing source")?;
    let selected = initial
        .control
        .iter()
        .find(|receipt| receipt.node_id == "select")
        .context("missing branch decision")?;
    assert_eq!(selected.selected_ports, ["matched"]);
    assert!(
        selected
            .dependencies
            .iter()
            .any(|dependency| dependency.artifact_id.as_ref() == Some(&source.artifact.id))
    );
    assert!(
        initial
            .control
            .iter()
            .any(|receipt| receipt.node_id == "join")
    );
    let call = |node_id: &str| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: node_id.into(),
        inputs: NodeInputs::from([("document".into(), source.artifact.id.clone())]),
        request: WorkflowHostRequest::LoadContext,
    };
    assert!(
        store
            .execute_workflow_host(call("unselected"))
            .await
            .unwrap_err()
            .to_string()
            .contains("skipped")
    );
    let output = store.execute_workflow_host(call("selected")).await?;
    assert!(
        initial
            .outputs
            .iter()
            .any(|entry| entry.artifact.id == output.id)
    );
    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(reopened.launch_workflow_manual(request).await?.id, run.id);
    let snapshot = reopened
        .inspect_workflow_execution(project, run.id)
        .await?
        .context("missing recovered graph")?;
    let join = snapshot
        .control
        .iter()
        .find(|record| record.node_id == "join")
        .context("missing join receipt")?;
    assert_eq!(join.selected_ports, ["joined"]);
    assert_eq!(
        join.dependencies
            .iter()
            .filter(|dependency| dependency.selected)
            .count(),
        1
    );
    assert!(
        join.dependencies
            .iter()
            .any(|dependency| dependency.artifact_id.as_ref() == Some(&output.id))
    );
    assert!(
        snapshot
            .control
            .iter()
            .any(|receipt| receipt.node_id == "unselected"
                && receipt.state == ControlStageState::Skipped)
    );
    Ok(())
}

#[test]
fn compiler_rejects_cycles_and_mixed_write_preparation_sources() {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph
        .nodes
        .push(node("alternate-prepare", BuiltinNode::PrepareWorktree));
    for mut wire in graph
        .wires
        .clone()
        .into_iter()
        .filter(|wire| wire.to.node == "prepare")
    {
        wire.to.node = "alternate-prepare".into();
        graph.wires.push(wire);
    }
    let worktree = graph
        .wires
        .iter_mut()
        .find(|wire| wire.to.node == "implement" && wire.to.port == "worktree")
        .unwrap();
    worktree.from.node = "alternate-prepare".into();
    assert!(
        compile("invalid".into(), graph)
            .unwrap_err()
            .to_string()
            .contains("same preparation")
    );

    let cycle = WorkflowGraph {
        name: "Invalid cycle".into(),
        nodes: vec![
            node("start", BuiltinNode::Start),
            node("parallel", BuiltinNode::Parallel),
            node("join", BuiltinNode::Join),
        ],
        wires: vec![
            wire("join", "joined", "parallel", "ready"),
            wire("parallel", "branches", "join", "branches"),
        ],
    };
    assert!(
        compile("invalid".into(), cycle)
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
}
