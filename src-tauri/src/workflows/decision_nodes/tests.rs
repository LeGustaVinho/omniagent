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
fn the_pinned_forms_validate_typed_samples_and_can_omit_approval_evidence() -> Result<()> {
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
    let sample = |port: &str, value| BTreeMap::from([(port.to_owned(), value)]);
    assert_eq!(
        simulate_stage_decision(
            ask,
            &sample("answers", json!({"ready":false,"checks":["lint"]}))
        )?,
        "answered"
    );
    assert!(
        simulate_stage_decision(
            ask,
            &sample("answers", json!({"ready":"no","checks":["lint"]}))
        )
        .is_err()
    );
    assert!(
        simulate_stage_decision(ask, &sample("answers", json!({"ready":false,"checks":[]})))
            .is_err()
    );
    assert!(
        simulate_stage_decision(
            ask,
            &sample("answers", json!({"ready":false,"checks":["unknown"]}))
        )
        .is_err()
    );
    assert_eq!(
        simulate_stage_decision(approval, &sample("decision", json!({"approved":false})))?,
        "rejected"
    );
    assert_eq!(
        simulate_stage_decision(approval, &sample("decision", json!({"approved":true})))?,
        "approved"
    );
    assert!(
        simulate_stage_decision(
            approval,
            &sample("decision", json!({"approved":true,"release":"fixture"}))
        )
        .is_err()
    );
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
