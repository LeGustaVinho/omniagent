use anyhow::{Context, Result};
use serde_json::json;

use crate::{
    store::Store,
    workflows::{
        BuiltinNode, Endpoint, NodeInstance, WorkflowGraph, WorkflowRevision, WorkflowRun,
        launch::WorkflowLaunchRequest, tests::approved_delivery_repository,
    },
};

use super::*;

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn wire(from: &str, output: &str, to: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(from, output),
        to: endpoint(to, input),
    }
}

fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

fn typed_start() -> Result<NodeInstance> {
    Ok(node(
        "start",
        BuiltinNode::TypedStart,
        serde_json::from_value(json!({
            "kind":"typed_start", "fields":[{"name":"topic", "valueType":"text", "required":true}],
        }))?,
    ))
}

fn fork_graph(strategy: JoinStrategy, cancel: bool, limit: u16) -> Result<WorkflowGraph> {
    let branches = vec!["alpha".to_owned(), "beta".to_owned()];
    let mut nodes = vec![
        typed_start()?,
        node("end", BuiltinNode::End, NodeConfiguration::End),
        node(
            "fork",
            BuiltinNode::Fork,
            NodeConfiguration::Fork(flow::ForkConfiguration {
                branches: branches.clone(),
                concurrency: Some(limit),
            }),
        ),
        node(
            "join",
            BuiltinNode::JoinResults,
            NodeConfiguration::JoinResults(JoinResultsConfiguration {
                branches: branches.clone(),
                strategy,
                quorum: None,
                count_failures: false,
                cancel_remaining: cancel,
                fork: Some("fork".into()),
            }),
        ),
    ];
    let mut wires = vec![
        wire("start", "start", "fork", "ready"),
        wire("start", "inputs", "fork", "input"),
        wire("join", "joined", "end", "ready"),
        wire("alpha", "result", "end", "result"),
    ];
    for branch in branches {
        nodes.push(node(
            &branch,
            BuiltinNode::Wait,
            serde_json::from_value(json!({
                "kind":"wait", "until":{"kind":"delay", "seconds":3600},
            }))?,
        ));
        wires.extend([
            wire("fork", &flow::branch_port(&branch), &branch, "ready"),
            wire("fork", &flow::context_port(&branch), &branch, "input"),
            wire(&branch, "completed", "join", &flow::branch_port(&branch)),
            wire(&branch, "result", "join", &flow::result_port(&branch)),
        ]);
    }
    Ok(WorkflowGraph {
        name: "Branch fixture".into(),
        nodes,
        wires,
    })
}

async fn launch(
    store: &Store,
    project: &str,
    checkout: &str,
    version: WorkflowRevision,
    id: &str,
) -> Result<WorkflowRun> {
    let request: WorkflowLaunchRequest = serde_json::from_value(json!({
        "projectId":project, "checkoutId":checkout, "workflow":version, "requestId":id, "inputs":{"topic":"hello"},
    }))?;
    store.launch_workflow_request(request).await
}

async fn advance_deadline(store: &Store, run: &WorkflowRun, branch: &str) -> Result<()> {
    let pipeline = run.id.clone();
    let branch = branch.to_owned();
    store.execute(move |db| {
        let count = db.execute("UPDATE workflow_wait_deadlines SET deadline_at = 0 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2", params![pipeline, branch])?;
        assert_eq!(count, 1);
        Ok(())
    }).await
}

#[test]
fn quorum_uses_completion_order_and_only_counts_failed_branches_when_configured() -> Result<()> {
    let mut config = JoinResultsConfiguration {
        branches: vec!["a".into(), "b".into(), "c".into()],
        strategy: JoinStrategy::Quorum,
        quorum: Some(2),
        count_failures: false,
        cancel_remaining: false,
        fork: None,
    };
    let mut branches = [
        ObservedBranch {
            name: "a",
            state: BranchState::Completed,
            order: Some(3),
            result: None,
        },
        ObservedBranch {
            name: "b",
            state: BranchState::Failed,
            order: Some(1),
            result: None,
        },
        ObservedBranch {
            name: "c",
            state: BranchState::Completed,
            order: Some(2),
            result: None,
        },
    ];
    let (success, selected) = choose_join(&config, &mut branches)?;
    assert_eq!(
        success.context("missing quorum decision")?.ports,
        ["joined"]
    );
    assert_eq!(selected, BTreeSet::from(["a", "c"]));
    config.count_failures = true;
    let (_, selected) = choose_join(&config, &mut branches)?;
    assert_eq!(selected, BTreeSet::from(["b", "c"]));

    config.count_failures = false;
    branches[2].state = BranchState::Skipped;
    let (failure, _) = choose_join(&config, &mut branches)?;
    assert_eq!(
        failure.context("impossible quorum stayed pending")?.ports,
        ["failed"]
    );
    Ok(())
}

#[tokio::test]
async fn fork_waits_keep_slots_across_restart_and_join_records_branch_context() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let db = folder.path().join("fork.sqlite");
    let store = Store::open(db.clone())?;
    let repo = approved_delivery_repository(&store, folder.path()).await?;
    let project = repo.project_id;
    let version = store
        .save_workflow_revision(
            project.clone(),
            "branches".into(),
            fork_graph(JoinStrategy::All, false, 1)?,
        )
        .await?;
    let run = launch(&store, &project, &repo.checkout_id, version, "fork").await?;
    let snapshot = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("missing fork run")?;
    assert_eq!(
        snapshot
            .fork_branches
            .iter()
            .map(|branch| branch.state)
            .collect::<Vec<_>>(),
        [BranchState::Running, BranchState::Queued]
    );
    assert_eq!(snapshot.timed_waits.len(), 1);
    drop(store);
    let store = Store::open(db)?;
    advance_deadline(&store, &run, "alpha").await?;
    let snapshot = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("missing fork run")?;
    assert_eq!(
        snapshot
            .fork_branches
            .iter()
            .map(|branch| branch.state)
            .collect::<Vec<_>>(),
        [BranchState::Completed, BranchState::Running]
    );
    assert_eq!(snapshot.timed_waits.len(), 1);
    advance_deadline(&store, &run, "beta").await?;
    let snapshot = store
        .inspect_workflow_execution(project, run.id)
        .await?
        .context("missing fork run")?;
    let join = snapshot
        .outputs
        .iter()
        .find(|entry| entry.artifact.node_instance_id == "join" && entry.current)
        .context("missing Join result")?;
    assert_eq!(
        join.artifact.value,
        ArtifactValue::JsonObject(Map::from_iter([(
            "branches".into(),
            json!({
                "alpha":{"status":"completed", "result":{"topic":"hello"}},
                "beta":{"status":"completed", "result":{"topic":"hello"}},
            })
        )]))
    );
    Ok(())
}

#[tokio::test]
async fn early_join_cancels_queued_and_waiting_branches() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let store = Store::open(folder.path().join("early.sqlite"))?;
    let repo = approved_delivery_repository(&store, folder.path()).await?;
    let project = repo.project_id;
    for limit in [1, 2] {
        let key = format!("early-{limit}");
        let version = store
            .save_workflow_revision(
                project.clone(),
                key.clone(),
                fork_graph(JoinStrategy::Any, true, limit)?,
            )
            .await?;
        let run = launch(&store, &project, &repo.checkout_id, version, &key).await?;
        let before = store
            .inspect_workflow_execution(project.clone(), run.id.clone())
            .await?
            .context("missing fork run")?;
        assert_eq!(before.timed_waits.len(), usize::from(limit));
        advance_deadline(&store, &run, "alpha").await?;
        let after = store
            .inspect_workflow_execution(project.clone(), run.id.clone())
            .await?
            .context("missing fork run")?;
        assert_eq!(
            after
                .fork_branches
                .iter()
                .map(|branch| branch.state)
                .collect::<Vec<_>>(),
            [BranchState::Completed, BranchState::Cancelled]
        );
        assert!(after.timed_waits.is_empty());
        let winner = after
            .outputs
            .iter()
            .find(|entry| entry.artifact.node_instance_id == "join" && entry.current)
            .context("missing early result")?;
        let value = &winner.artifact.value;
        assert!(
            matches!(value, ArtifactValue::JsonObject(fields) if fields["branches"].as_object().is_some_and(|branches| branches.len() == 1 && branches.contains_key("alpha")))
        );
    }
    Ok(())
}

#[tokio::test]
async fn loop_preserves_results_and_exits_using_previous_context() -> Result<()> {
    use crate::workflows::authoring::package::WorkflowPackagePublishOutcome;

    let folder = tempfile::tempdir()?;
    let store = Store::open(folder.path().join("loop.sqlite"))?;
    let repo = approved_delivery_repository(&store, folder.path()).await?;
    let project = repo.project_id;
    let body = json!({
        "key":"body", "name":"Iteration", "body":{"kind":"subflow", "graph":{
            "name":"Iteration", "nodes":[
                {"id":"start", "kind":"start", "configuration":{"kind":"subflow_entry","outputs":[
                    {"name":"ready", "value":{"kind":"control"}},
                    {"name":"context", "value":{"kind":"data", "artifact":"json_object"}},
                ]}},
                {"id":"copy", "kind":"set_fields", "configuration":{"kind":"set_fields", "mode":"build", "operations":[
                    {"op":"set", "path":"/index", "value":{"source":"input", "pointer":"/index"}},
                ]}},
            ], "wires":[wire("start", "ready", "copy", "ready"), wire("start", "context", "copy", "source")],
        }, "inputs":{"ready": endpoint("start", "ready"), "context": endpoint("start", "context")},
           "outputs":{"completed":endpoint("copy", "completed"), "failed":endpoint("copy", "failed"), "result":endpoint("copy", "object")}},
    });
    let pipeline = json!({
        "key":"pipeline", "name":"Loop fixture", "body":{"kind":"pipeline", "graph":{
            "name":"Loop fixture", "nodes":[typed_start()?, {"id":"loop", "kind":"loop", "configuration":{
                "kind":"loop", "maxIterations":3, "limit":"fail",
                "readyInput":"ready", "contextInput":"context", "completionOutput":"completed", "resultOutput":"result",
                "exitWhen":{"op":"equals", "pointer":"/previous/index", "value":1},
            }}], "wires":[wire("start", "start", "loop", "ready"), wire("start", "inputs", "loop", "input")],
        }}, "references":{"loop":{"source":"draft", "key":"body"}},
    });
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            serde_json::from_value(json!({"name":"Loop demo","members":[body,pipeline]}))?,
        )
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("the fixture did not publish");
    };
    let artifact = publication
        .members
        .iter()
        .find(|member| member.key == "pipeline")
        .context("missing pipeline")?;
    let version = WorkflowRevision {
        id: artifact.artifact.version.id.clone(),
        revision: artifact.artifact.version.revision.clone(),
    };
    let run = launch(&store, &project, &repo.checkout_id, version, "loop").await?;
    let snapshot = store
        .inspect_workflow_execution(project, run.id)
        .await?
        .context("missing Loop run")?;
    let result = snapshot
        .outputs
        .iter()
        .find(|entry| entry.artifact.node_instance_id == "loop::collect" && entry.current)
        .context("missing Loop results")?;
    assert!(
        matches!(&result.artifact.value, ArtifactValue::JsonObject(value) if value["reason"] == "exit" && value["iterations"] == json!([{"index":1},{"index":2}]))
    );
    assert!(snapshot.control.iter().any(
        |record| record.node_id == "loop::gate-3" && record.state == ControlStageState::Skipped
    ));
    Ok(())
}
