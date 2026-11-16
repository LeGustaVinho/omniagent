use std::{path::Path, process::Command, sync::Arc, time::Duration};

use crate::{
    jobs::{Job, JobScheduler},
    repository::ProjectCheckout,
    runner::DirectWindowsRunner,
    runs::RunStatus,
    store::Store,
    workflows::{
        ManualWorkflowLaunch, NodeInputs, WorkflowLaunchSource, WorkflowRevision, WorkflowRun,
        authoring::package::{
            PackageArtifactBody, PackageMember, PackageReference, WorkflowPackage,
            WorkflowPackagePublishOutcome, WorkflowSimulationScenario,
        },
        mcp_tools::{McpServer, McpServerRequest, WorkflowMcpToolSubmission},
        tests::approved_delivery_repository,
    },
};

use super::*;

fn package_member(
    key: &str,
    body: PackageArtifactBody,
    references: BTreeMap<String, PackageReference>,
) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Project,
        base: None,
        body,
        references,
    }
}

async fn publish_and_launch(
    store: &Store,
    repo: &ProjectCheckout,
    server: &McpServer,
    values: Vec<Value>,
    cap: u16,
    request: &str,
) -> Result<WorkflowRun> {
    let empty = values.is_empty();
    let mut body = body(false, false)?;
    let apply_stage = body
        .workflow
        .nodes
        .iter_mut()
        .find(|node| node.instance.id == "apply")
        .unwrap();
    let NodeConfiguration::McpTool(configuration) = &mut apply_stage.instance.configuration else {
        unreachable!();
    };
    configuration.server_id.clone_from(&server.id);
    configuration.server_revision.clone_from(&server.revision);
    configuration.tool_name.clone_from(&server.tools[0].name);
    configuration
        .contract_revision
        .clone_from(&server.tools[0].revision);
    configuration.argument_mappings =
        BTreeMap::from([("destination".into(), "/evidence/value".into())]);
    configuration.destination_pointer = Some("/destination".into());
    configuration
        .arguments
        .insert("action".into(), json!("mutate"));
    let child = package_member(
        "child",
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: "Mapped MCP iteration".into(),
                nodes: body
                    .workflow
                    .nodes
                    .into_iter()
                    .map(|pinned| pinned.instance)
                    .collect(),
                wires: body.workflow.wires,
            },
            inputs: body.inputs,
            outputs: body.outputs,
        },
        BTreeMap::new(),
    );
    let pipeline = package_member(
        "root",
        PackageArtifactBody::Pipeline {
            graph: WorkflowGraph {
                name: "List workflow fixture".into(),
                nodes: vec![
                    node("start", BuiltinNode::Start, NodeConfiguration::None),
                    node(
                        "list",
                        BuiltinNode::BuildList,
                        NodeConfiguration::BuildList(BuildListConfiguration::Values { values }),
                    ),
                    node(
                        "each",
                        BuiltinNode::ForEach,
                        NodeConfiguration::ForEach(config(cap)),
                    ),
                    node("success", BuiltinNode::Parallel, NodeConfiguration::None),
                    node("failure", BuiltinNode::Parallel, NodeConfiguration::None),
                ],
                wires: vec![
                    wire("start", "start", "list", "ready"),
                    wire("list", "completed", "each", "ready"),
                    wire("list", "items", "each", "items"),
                    wire("each", "completed", "success", "ready"),
                    wire("each", "failed", "failure", "ready"),
                ],
            },
        },
        BTreeMap::from([(
            "each".into(),
            PackageReference::Draft {
                key: "child".into(),
            },
        )]),
    );
    let draft = store
        .create_workflow_package(
            repo.project_id.clone(),
            None,
            WorkflowPackage {
                name: request.into(),
                members: vec![child, pipeline],
            },
        )
        .await?;
    if empty {
        let scenario: WorkflowSimulationScenario = serde_json::from_value(json!({
            "inputs": {"document": {"relativePath": "sample.md"}}
        }))?;
        let trace = store
            .simulate_workflow_package_revision(
                repo.project_id.clone(),
                draft.id.clone(),
                draft.revision.clone(),
                "root".into(),
                scenario,
            )
            .await?;
        let trace = serde_json::to_value(trace)?;
        let steps = trace["steps"]
            .as_array()
            .context("Missing simulated iteration steps")?;
        let gather = steps
            .iter()
            .find(|step| step["nodeId"] == "each::collect")
            .context("Missing simulated collector")?;
        assert_eq!(
            gather["outputs"]["results"]["value"]["evidence"]["results"],
            json!([])
        );
        assert!(
            steps
                .iter()
                .filter(|step| step["nodeId"]
                    .as_str()
                    .is_some_and(|id| id.ends_with("::apply")))
                .all(|step| step["state"] == "skipped")
        );
    }
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(repo.project_id.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("The test For each package did not publish");
    };
    let pinned = &publication
        .members
        .iter()
        .find(|member| member.key == "root")
        .unwrap()
        .artifact
        .version;
    let captured = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: repo.project_id.clone(),
            workflow: WorkflowRevision {
                id: pinned.id.clone(),
                revision: pinned.revision.clone(),
            },
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: captured.revision.id,
            },
            request_id: request.into(),
            work_id: None,
        })
        .await
}

async fn submit(
    store: &Store,
    repo: &ProjectCheckout,
    run: &WorkflowRun,
    ordinal: u16,
    item: &str,
) -> Result<Job> {
    store
        .submit_workflow_mcp_tool(WorkflowMcpToolSubmission {
            project_id: repo.project_id.clone(),
            checkout_id: repo.checkout_id.clone(),
            run_id: run.id.clone(),
            node_id: format!("each::iteration-{ordinal}::apply"),
            inputs: NodeInputs::from([("arguments".into(), item.into())]),
        })
        .await
}

async fn approve_and_settle(store: &Store, job: &Job, expected_destination: &str) -> Result<()> {
    let review = store
        .workflow_mcp_job_review(job.project_id.clone(), job.id.clone())
        .await?;
    assert_eq!(review.decision, "pending");
    assert_eq!(review.destination.as_deref(), Some(expected_destination));
    store
        .decide_workflow_mcp_job(job.project_id.clone(), job.id.clone(), true)
        .await?;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let current = store
                .job(job.project_id.clone(), job.id.clone())
                .await?
                .context("Iteration job disappeared")?;
            if let Some(attempt) = current
                .latest_attempt
                .filter(|attempt| attempt.status.is_terminal())
            {
                assert_eq!(attempt.status, RunStatus::Succeeded);
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn bounded_iteration_runs_no_empty_work_and_recovers_exact_order_after_a_real_attempt()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let database = fixture.path().join("iteration.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, fixture.path()).await?;
    let executable = fixture.path().join("mcp-server.exe");
    let compiled = Command::new("rustc")
        .args(["--edition=2024", "-C", "opt-level=1"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local_mcp_tool_server.rs"))
        .arg("-o")
        .arg(&executable)
        .output()?;
    ensure!(
        compiled.status.success(),
        "Could not compile local MCP fixture: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let server = store
        .register_workflow_mcp_server(
            repo.project_id.clone(),
            McpServerRequest {
                name: "Local iteration fixture".into(),
                executable,
                outbound_network: false,
                arguments: vec![
                    Path::new(&repo.checkout_root)
                        .join("brief.md")
                        .to_str()
                        .context("Fixture checkout path is unavailable")?
                        .into(),
                ],
            },
        )
        .await?;

    let empty = publish_and_launch(&store, &repo, &server, Vec::new(), 3, "empty").await?;
    let inspection = store
        .inspect_workflow_execution(repo.project_id.clone(), empty.id)
        .await?
        .unwrap();
    assert!(inspection.jobs.is_empty());
    assert!(
        inspection
            .outputs
            .iter()
            .all(|entry| !matches!(entry.artifact.value, ArtifactValue::JsonItem(_)))
    );
    assert!(inspection.outputs.iter().any(|entry| matches!(&entry.artifact.value, ArtifactValue::IterationResults(results) if results.results.is_empty())));
    assert!(
        inspection
            .control
            .iter()
            .any(|receipt| receipt.node_id == "success" && receipt.selected_ports == ["branches"])
    );

    let overflow = publish_and_launch(
        &store,
        &repo,
        &server,
        vec![json!("a"), json!("b")],
        1,
        "overflow",
    )
    .await?;
    let inspection = store
        .inspect_workflow_execution(repo.project_id.clone(), overflow.id)
        .await?
        .unwrap();
    assert!(inspection.jobs.is_empty());
    assert!(inspection.outputs.iter().all(|entry| !matches!(
        entry.artifact.value,
        ArtifactValue::JsonItem(_) | ArtifactValue::IterationResults(_)
    )));
    assert!(
        inspection
            .control
            .iter()
            .any(|receipt| receipt.node_id == "failure" && receipt.selected_ports == ["branches"])
    );

    let run = publish_and_launch(
        &store,
        &repo,
        &server,
        vec![json!("first"), json!("second")],
        3,
        "ordered",
    )
    .await?;
    let inspection = store
        .inspect_workflow_execution(repo.project_id.clone(), run.id.clone())
        .await?
        .unwrap();
    let first_item = inspection
        .outputs
        .iter()
        .find(|entry| entry.artifact.node_instance_id == "each::gate-1")
        .context("First item not ready")?
        .artifact
        .id
        .clone();
    assert!(
        inspection
            .outputs
            .iter()
            .all(|entry| entry.artifact.node_instance_id != "each::gate-2")
    );
    assert!(submit(&store, &repo, &run, 2, &first_item).await.is_err());
    let first_job = submit(&store, &repo, &run, 1, &first_item).await?;
    let scheduler = JobScheduler::start(store.clone(), Arc::new(DirectWindowsRunner), None);
    let verdict = approve_and_settle(&store, &first_job, "first").await;
    scheduler.shutdown().await?;
    verdict?;
    drop(store);

    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let inspection = store
        .inspect_workflow_execution(repo.project_id.clone(), run.id.clone())
        .await?
        .unwrap();
    assert_eq!(inspection.jobs.len(), 1);
    assert_eq!(inspection.jobs[0].job.id, first_job.id);
    assert_eq!(
        inspection.jobs[0]
            .job
            .latest_attempt
            .as_ref()
            .unwrap()
            .ordinal,
        1
    );
    let first_result = inspection
        .outputs
        .iter()
        .find(|entry| entry.artifact.node_instance_id == "each::iteration-1::apply")
        .unwrap()
        .artifact
        .id
        .clone();
    let second = inspection
        .outputs
        .iter()
        .find(|entry| entry.artifact.node_instance_id == "each::gate-2")
        .context("Second item was not released")?;
    assert_eq!(second.input_receipts[0].inputs["previous"], first_result);
    let ArtifactValue::JsonItem(item) = &second.artifact.value else {
        anyhow::bail!("Missing pinned second value");
    };
    assert_eq!(item.value, json!("second"));
    assert!(submit(&store, &repo, &run, 2, &first_item).await.is_err());
    let second_job = submit(&store, &repo, &run, 2, &second.artifact.id).await?;
    let scheduler = JobScheduler::start(store.clone(), Arc::new(DirectWindowsRunner), None);
    let verdict = approve_and_settle(&store, &second_job, "second").await;
    scheduler.shutdown().await?;
    verdict?;

    let inspection = store
        .inspect_workflow_execution(repo.project_id.clone(), run.id.clone())
        .await?
        .unwrap();
    assert_eq!(inspection.jobs.len(), 2);
    let second_result = inspection
        .outputs
        .iter()
        .find(|entry| entry.artifact.node_instance_id == "each::iteration-2::apply")
        .unwrap()
        .artifact
        .id
        .clone();
    let gathered = inspection
        .outputs
        .iter()
        .find(|entry| entry.artifact.node_instance_id == "each::collect")
        .context("Summary not published")?;
    let ArtifactValue::IterationResults(results) = &gathered.artifact.value else {
        anyhow::bail!("Missing ordered iteration result");
    };
    assert_eq!(results.result_kind, ArtifactKind::McpToolResult);
    assert_eq!(results.results, [first_result, second_result]);
    let saved = store
        .resolve_workflow_artifact(
            repo.project_id.clone(),
            run.id,
            gathered.artifact.id.clone(),
            ArtifactKind::IterationResults,
        )
        .await?;
    assert_eq!(saved.as_ref(), Some(&gathered.artifact));
    Ok(())
}
