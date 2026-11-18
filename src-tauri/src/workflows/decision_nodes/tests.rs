use std::path::Path;

use super::*;
use crate::workflows::{
    Endpoint, ManualWorkflowLaunch, NodeInstance, Wire, WorkflowGraph, WorkflowLaunchSource,
    compile, control::ControlStageState, tests::approved_delivery_repository,
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

fn graph(with_evidence: bool) -> WorkflowGraph {
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
    let mut wires = vec![
        wire("start", "start", "ask", "ready"),
        wire("ask", "answered", "review", "ready"),
        wire("review", "approved", "approved", "ready"),
        wire("review", "rejected", "rejected", "ready"),
        wire("start", "document", "approved", "document"),
        wire("start", "document", "rejected", "document"),
    ];
    if with_evidence {
        wires.push(wire("ask", "answers", "review", "evidence"));
    }
    WorkflowGraph {
        name: "Operator decisions".into(),
        nodes: vec![
            node("start", BuiltinNode::Start, NodeConfiguration::None),
            node(
                "ask",
                BuiltinNode::RequestInput,
                NodeConfiguration::RequestInput(question()),
            ),
            node(
                "review",
                BuiltinNode::Approval,
                NodeConfiguration::Approval(ApprovalConfiguration {
                    message: "Review the selected checks.".into(),
                }),
            ),
            node(
                "approved",
                BuiltinNode::LoadContext,
                NodeConfiguration::None,
            ),
            node(
                "rejected",
                BuiltinNode::LoadContext,
                NodeConfiguration::None,
            ),
        ],
        wires,
    }
}

fn answer(ready: Value) -> WorkflowStageDecision {
    WorkflowStageDecision::Answer {
        values: Map::from_iter([
            ("ready".into(), ready),
            ("checks".into(), json!(["lint", "build"])),
        ]),
    }
}

#[test]
fn the_pinned_forms_validate_typed_responses_and_can_omit_approval_evidence() -> Result<()> {
    let definition = compile("decisions".into(), graph(false))?;
    let ask = definition
        .nodes
        .iter()
        .find(|node| node.instance.id == "ask")
        .unwrap();
    let approval = definition
        .nodes
        .iter()
        .find(|node| node.instance.id == "review")
        .unwrap();
    let request = WorkflowDecisionRequest::for_node(ask, None)?;
    let respond = |ready: Value, checks: Value| {
        let reply = WorkflowStageDecision::Answer {
            values: Map::from_iter([("ready".into(), ready), ("checks".into(), checks)]),
        };
        validated_evidence(ask, &request, &reply).map(|_| reply.control_port())
    };
    assert_eq!(respond(json!(false), json!(["lint"]))?, "answered");
    assert!(respond(json!("no"), json!(["lint"])).is_err());
    assert!(respond(json!(false), json!([])).is_err());
    assert!(respond(json!(false), json!(["unknown"])).is_err());

    let request = WorkflowDecisionRequest::for_node(approval, None)?;
    for (approved, port) in [(false, "rejected"), (true, "approved")] {
        let reply = WorkflowStageDecision::Approval { approved };
        validated_evidence(approval, &request, &reply)?;
        assert_eq!(reply.control_port(), port);
    }
    assert!(validated_evidence(approval, &request, &answer(json!(true))).is_err());
    let mut invalid = question();
    invalid.fields[1].id = invalid.fields[0].id.clone();
    assert!(!invalid.is_valid());
    Ok(())
}

#[tokio::test]
async fn a_durable_operator_wait_pins_answers_and_only_settles_the_selected_branch() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("decisions.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let source = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let project = repo.project_id;
    let version = store
        .save_workflow_revision(project.clone(), "decisions".into(), graph(true))
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: version,
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id,
                revision_id: source.revision.id,
            },
            request_id: "start-decisions".into(),
            work_id: None,
        })
        .await?;
    let first = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    assert!(first.jobs.is_empty());
    assert_eq!(first.pending_decisions.len(), 1);
    let ask = &first.pending_decisions[0];
    assert_eq!(ask.node_id, "ask");
    let WorkflowDecisionRequest::RequestInput { form } = &ask.request else {
        bail!("The workflow did not expose its pinned input form");
    };
    assert_eq!(
        form.fields
            .iter()
            .map(|field| field.id.as_str())
            .collect::<Vec<_>>(),
        ["ready", "checks"]
    );
    let submit = |request: &WorkflowPendingDecision, decision| WorkflowDecisionSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        request_id: request.request_id.clone(),
        decision,
    };
    let mut foreign = submit(ask, answer(json!(true)));
    foreign.project_id = "different-project".into();
    assert!(
        store
            .resolve_workflow_stage_decision(foreign)
            .await
            .is_err()
    );
    assert!(
        store
            .resolve_workflow_stage_decision(submit(ask, answer(json!("true"))))
            .await
            .is_err()
    );
    drop(store);

    let store = Store::open(database.clone())?;
    let reopened = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    assert_eq!(reopened.pending_decisions, first.pending_decisions);
    let reply = submit(ask, answer(json!(false)));
    let (recorded, replayed) = tokio::join!(
        store.resolve_workflow_stage_decision(reply.clone()),
        store.resolve_workflow_stage_decision(reply)
    );
    let recorded = recorded?;
    assert_eq!(recorded, replayed?);
    assert!(
        store
            .resolve_workflow_stage_decision(submit(ask, answer(json!(true))))
            .await
            .is_err()
    );

    let after_answer = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    assert_eq!(after_answer.pending_decisions.len(), 1);
    let review = &after_answer.pending_decisions[0];
    assert_eq!(review.inputs["evidence"], recorded.id);
    let refused = submit(review, WorkflowStageDecision::Approval { approved: false });
    let decision = store
        .resolve_workflow_stage_decision(refused.clone())
        .await?;
    assert_eq!(
        decision.value,
        ArtifactValue::JsonObject(Map::from_iter([("approved".into(), json!(false))]))
    );
    assert!(
        store
            .resolve_workflow_artifact(
                project.clone(),
                run.id.clone(),
                decision.id.clone(),
                ArtifactKind::Approval
            )
            .await?
            .is_none()
    );
    drop(store);

    let store = Store::open(database)?;
    let settled = store
        .inspect_workflow_execution(project, run.id)
        .await?
        .unwrap();
    assert!(settled.pending_decisions.is_empty());
    assert!(settled.jobs.is_empty());
    let review_control = settled
        .control
        .iter()
        .find(|receipt| receipt.node_id == "review")
        .unwrap();
    assert_eq!(review_control.selected_ports, ["rejected"]);
    assert_eq!(
        settled
            .control
            .iter()
            .find(|receipt| receipt.node_id == "approved")
            .unwrap()
            .state,
        ControlStageState::Skipped
    );
    assert!(
        settled
            .outputs
            .iter()
            .any(|output| output.current && output.artifact.node_instance_id == "rejected")
    );
    assert_eq!(
        store.resolve_workflow_stage_decision(refused).await?,
        decision
    );
    Ok(())
}

#[tokio::test]
async fn wait_retains_context_across_reopen_and_expired_approval_rejects_a_late_answer()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("waits.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let source = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
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
            node("start", BuiltinNode::Start, NodeConfiguration::None),
            node(
                "ask",
                BuiltinNode::RequestInput,
                NodeConfiguration::RequestInput(question()),
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
            wire("start", "start", "ask", "ready"),
            wire("ask", "answered", "wait", "ready"),
            wire("ask", "answers", "wait", "input"),
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
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: version,
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id,
                revision_id: source.revision.id,
            },
            request_id: "review-after-a-delay".into(),
            work_id: None,
        })
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
    let first = inspect(&store, &run).await?;
    let submit = |request: &WorkflowPendingDecision, decision| WorkflowDecisionSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        request_id: request.request_id.clone(),
        decision,
    };
    let answer_artifact = store
        .resolve_workflow_stage_decision(submit(&first.pending_decisions[0], answer(json!(true))))
        .await?;
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
    assert_eq!(wait_output.artifact.value, answer_artifact.value);
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
