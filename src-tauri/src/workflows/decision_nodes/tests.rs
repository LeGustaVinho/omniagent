use super::*;
use crate::workflows::{
    Endpoint, NodeInstance, Wire, WorkflowGraph, tests::approved_delivery_repository,
};

fn question() -> RequestInputConfiguration {
    RequestInputConfiguration {
        message: "Which checks should the reviewer use?".into(),
        fields: vec![
            RequestInputField {
                id: "ready".into(),
                label: "Ready".into(),
                required: true,
                input: RequestInputKind::Boolean,
            },
            RequestInputField {
                id: "checks".into(),
                label: "Checks".into(),
                required: true,
                input: RequestInputKind::Choice {
                    multiple: true,
                    options: ["build", "lint"]
                        .map(|name| RequestInputOption {
                            id: name.into(),
                            label: name.into(),
                        })
                        .to_vec(),
                },
            },
        ],
    }
}

#[tokio::test]
async fn wait_retains_context_across_reopen_and_expired_approval_rejects_a_late_answer()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("waits.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let node = |id: &str, kind, configuration| NodeInstance {
        id: id.into(),
        kind,
        configuration,
    };
    let wire = |from: &str, output: &str, to: &str, input: &str| Wire {
        from: Endpoint {
            node: from.into(),
            port: output.into(),
        },
        to: Endpoint {
            node: to.into(),
            port: input.into(),
        },
    };
    let workflow = WorkflowGraph {
        name: "Review after a delay".into(),
        nodes: vec![
            node(
                "start",
                BuiltinNode::TypedStart,
                serde_json::from_value(json!({
                    "kind": "typed_start", "fields": [
                        {"name":"ready","valueType":"boolean","required":true},
                        {"name":"checks","valueType":"array","required":true}
                    ]
                }))?,
            ),
            node(
                "wait",
                BuiltinNode::Wait,
                NodeConfiguration::Wait(timed::WaitConfiguration {
                    until: timed::WaitUntil::Delay { seconds: 3600 },
                }),
            ),
            node(
                "review",
                BuiltinNode::Human,
                NodeConfiguration::Human(HumanConfiguration {
                    interaction: HumanInteraction::Review,
                    message: "Review the readiness flag".into(),
                    fields: vec![question().fields.remove(0)],
                    timeout_seconds: None,
                    on_expiration: HumanExpiration::Failed,
                }),
            ),
            node(
                "approve",
                BuiltinNode::Human,
                NodeConfiguration::Human(HumanConfiguration {
                    interaction: HumanInteraction::Approval,
                    message: "Approve the reviewed data".into(),
                    fields: Vec::new(),
                    timeout_seconds: Some(3600),
                    on_expiration: HumanExpiration::Reject,
                }),
            ),
        ],
        wires: vec![
            wire("start", "start", "wait", "ready"),
            wire("start", "inputs", "wait", "input"),
            wire("wait", "completed", "review", "ready"),
            wire("wait", "result", "review", "input"),
            wire("review", "answered", "approve", "ready"),
            wire("review", "response", "approve", "input"),
        ],
    };
    let version = store
        .save_workflow_revision(project.clone(), "waits".into(), workflow)
        .await?;
    let run = store
        .launch_workflow_request(serde_json::from_value(json!({
            "projectId": project, "workflow": version, "checkoutId": repo.checkout_id,
            "requestId":"review-after-a-delay", "inputs":{"ready":true,"checks":["lint","build"]}
        }))?)
        .await?;
    async fn inspect(
        store: &Store,
        run: &WorkflowRun,
    ) -> Result<inspection::WorkflowExecutionInspection> {
        store
            .inspect_workflow_execution(run.project_id.clone(), run.id.clone())
            .await?
            .context("The workflow was not started")
    }
    let submit = |request: &WorkflowPendingDecision, decision| WorkflowDecisionSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        request_id: request.request_id.clone(),
        decision,
    };
    let waiting = inspect(&store, &run).await?;
    let deadlines = serde_json::to_value(&waiting.timed_waits)?;
    assert_eq!(waiting.timed_waits.len(), 1);
    assert!(waiting.pending_decisions.is_empty());
    drop(store);

    let store = Store::open(database)?;
    let reopened = inspect(&store, &run).await?;
    assert_eq!(serde_json::to_value(&reopened.timed_waits)?, deadlines);
    async fn make_due(store: &Store, run: &WorkflowRun, node: &'static str) -> Result<()> {
        let run = run.id.clone();
        store.execute(move |db| {
            let changed = db.execute(
                "UPDATE workflow_wait_deadlines SET deadline_at = 0 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2",
                params![run, node],
            )?;
            ensure!(changed == 1, "The workflow deadline was not persisted");
            Ok(())
        }).await
    }
    make_due(&store, &run, "wait").await?;
    assert_eq!(store.refresh_due_workflow_waits().await?, 1);
    let reviewing = inspect(&store, &run).await?;
    assert!(reviewing.timed_waits.is_empty());
    let wait_output = reviewing
        .outputs
        .iter()
        .find(|output| output.artifact.node_instance_id == "wait")
        .unwrap();
    assert_eq!(
        serde_json::to_value(&wait_output.artifact.value)?["evidence"],
        json!({"ready":true,"checks":["lint","build"]})
    );
    let review = &reviewing.pending_decisions[0];
    let WorkflowDecisionRequest::Human {
        form,
        original: Some(original),
        ..
    } = &review.request
    else {
        bail!("Expected the original data for Review");
    };
    assert_eq!(form.fields[0].default, Some(json!(true)));
    assert_eq!(original["checks"], json!(["lint", "build"]));
    let edit = |ready| WorkflowStageDecision::Answer {
        values: Map::from_iter([("ready".into(), ready)]),
    };
    assert!(
        store
            .resolve_workflow_stage_decision(submit(review, edit(json!("false"))))
            .await
            .is_err()
    );
    let reviewed = store
        .resolve_workflow_stage_decision(submit(review, edit(json!(false))))
        .await?;
    assert_eq!(
        reviewed.value,
        ArtifactValue::JsonObject(
            json!({
                "interaction":"review", "status":"answered", "original":original,
                "values":{"ready":false,"checks":["lint","build"]}
            })
            .as_object()
            .unwrap()
            .clone()
        )
    );

    let approving = inspect(&store, &run).await?;
    let approval = &approving.pending_decisions[0];
    assert!(approval.deadline_at.is_some());
    make_due(&store, &run, "approve").await?;
    let (due, late) = tokio::join!(
        store.refresh_due_workflow_waits(),
        store.resolve_workflow_stage_decision(submit(
            approval,
            WorkflowStageDecision::HumanApproval {
                approved: true,
                values: Map::new()
            }
        ))
    );
    due?;
    assert!(late.is_err());
    assert_eq!(store.refresh_due_workflow_waits().await?, 0);
    let settled = inspect(&store, &run).await?;
    assert!(settled.pending_decisions.is_empty());
    assert_eq!(
        settled
            .control
            .iter()
            .find(|receipt| receipt.node_id == "approve")
            .unwrap()
            .selected_ports,
        ["rejected"]
    );
    let outputs = settled
        .outputs
        .iter()
        .filter(|output| output.artifact.node_instance_id == "approve")
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    let ArtifactValue::JsonObject(response) = &outputs[0].artifact.value else {
        bail!("Expected the expiration response");
    };
    assert_eq!(response["expired"], json!(true));
    assert_eq!(response["approved"], json!(false));
    Ok(())
}
