use std::{fs, path::Path};

use serde_json::json;

use crate::{
    conversations::AttemptPlan,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::AdmissionEnvironment,
    providers::{ProviderEvent, ProviderEventData, ProviderLocation, TurnOutcome},
    repository::{
        self, ManagedWorktree,
        verification::{CandidateGate, CandidateGateAction},
    },
    workflows::{
        DeliveryRoles, Endpoint, NodeConfiguration, NodeInstance, Wire, WorkflowAgentSubmission,
        builtin_delivery_graph,
        tests::{
            ApprovedDeliveryFixture, approve_delivery_fixture_in_run, approved_delivery_repository,
            submission,
        },
    },
};

use super::{super::PendingWorkflowAgent, *};

async fn fixture_admission(
    store: &Store,
    request: &PendingWorkflowAgent,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
) -> Result<WorkflowAgentSubmission> {
    let target = store
        .workflow_operator_agent_target(
            &fixture.project,
            &fixture.run.id,
            &request.node,
            request.request.unit.as_deref(),
            &request.request.inputs,
        )
        .await?;
    let mut scope = submission(
        &fixture.checkout,
        &fixture.run,
        &request.node,
        target.plan_revision.clone(),
        target.mode,
    )
    .scope;
    scope.checkout = worktree.root.clone();
    scope.executable = Path::new(&fixture.checkout.repository_root).join("codex.exe");
    Ok(request.submission(target, scope))
}

async fn claim(store: &Store) -> Result<AttemptPlan> {
    store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-runner".into(),
            bridge_available: false,
        })
        .await?
        .context("The delivery fixture did not admit the stage")
}

#[tokio::test]
async fn automatic_delivery_seals_frozen_candidate_and_retained_review_with_pinned_provenance()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("automatic-delivery.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = approved_delivery_repository(&store, dir.path()).await?;
    let powershell = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?,
    )
    .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    store
        .set_candidate_gate_policy(
            checkout.project_id.clone(),
            vec![CandidateGate {
                name: "fixture-validation".into(),
                required: true,
                action: CandidateGateAction::Command {
                    executable: powershell,
                    arguments: vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                        "exit 7".into(),
                    ],
                    timeout_seconds: Some(30),
                },
            }],
        )
        .await?;
    let task = store
        .create_project_task(
            checkout.project_id.clone(),
            "Check automatic delivery".into(),
        )
        .await?;
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.nodes.push(NodeInstance {
        id: "dispatch".into(),
        kind: BuiltinNode::Parallel,
        configuration: NodeConfiguration::None,
    });
    let ready = graph
        .wires
        .iter_mut()
        .find(|wire| wire.to.node == "implement" && wire.to.port == "ready")
        .unwrap();
    ready.from = Endpoint {
        node: "dispatch".into(),
        port: "branches".into(),
    };
    graph.wires.push(Wire {
        from: Endpoint {
            node: "prepare".into(),
            port: "prepared".into(),
        },
        to: Endpoint {
            node: "dispatch".into(),
            port: "ready".into(),
        },
    });
    let definition = store
        .save_workflow_revision(
            checkout.project_id.clone(),
            "automatic-delivery-fixture".into(),
            graph,
        )
        .await?;
    let run = store
        .start_workflow_run(checkout.project_id.clone(), definition)
        .await?;
    let fixture =
        approve_delivery_fixture_in_run(&store, dir.path(), checkout, Some(task.id.clone()), run)
            .await?;
    let unit = &fixture.units[0].unit.id;
    let worktree = store
        .live_managed_worktree(fixture.project.clone(), task.id.clone())
        .await?
        .context("The approved task has no managed worktree")?
        .worktree;
    store
        .execute({
            let workflow = fixture.run.id.clone();
            let project = fixture.project.clone();
            move |db| {
                db.execute(
                    "INSERT INTO workflow_automatic_runs VALUES (?1)",
                    [&workflow],
                )?;
                control::refresh(db, &project, &workflow)
            }
        })
        .await?;

    let pending = store
        .next_workflow_agent()
        .await?
        .context("The implementation was not selected")?;
    assert_eq!(pending.node, "implement");
    assert_eq!(pending.request.unit.as_ref(), Some(unit));
    assert!(pending.request.inputs.contains_key("release"));
    let request = fixture_admission(&store, &pending, &fixture, &worktree).await?;
    pending.enqueue(&store, request).await?;
    let duplicate = fixture_admission(&store, &pending, &fixture, &worktree).await?;
    assert!(
        store
            .submit_workflow_agent(duplicate)
            .await
            .unwrap_err()
            .to_string()
            .contains("already has a job")
    );
    let writer = claim(&store).await?;
    let source_run = writer.run.id;
    store.mark_agent_running(source_run.clone()).await?;
    ensure!(repository::capture_worktree_start(&store, source_run.clone()).await?);
    fs::write(worktree.root.join("first.txt"), "frozen tracked text\n")?;
    fs::write(worktree.root.join("added.txt"), "frozen new text\n")?;
    repository::save_worktree_candidate(&store, source_run.clone()).await?;
    store
        .finish_agent_attempt(source_run.clone(), RunStatus::Succeeded, None)
        .await?;
    fs::write(worktree.root.join("first.txt"), "later mutable text\n")?;
    store.set_job_queue_paused(true).await?;
    assert!(store.next_workflow_delivery_result().await?.is_none());
    store
        .execute({
            let attempt = source_run.clone();
            move |db| {
                // Reproduce a stop immediately after the job committed its terminal state.
                assert_eq!(
                    db.execute(
                        "DELETE FROM workflow_automatic_delivery_results WHERE attempt_id = ?1",
                        [attempt]
                    )?,
                    1
                );
                Ok(())
            }
        })
        .await?;
    drop(store);

    let store = Store::open(database)?;
    store.set_job_queue_paused(false).await?;
    store
        .next_workflow_delivery_result()
        .await?
        .context("The capture publication was lost after restart")?
        .publish(&store)
        .await?;
    assert!(store.next_workflow_delivery_result().await?.is_none());
    let run = fixture.run.id.clone();
    let candidate_artifact = store
        .workflow_run_artifacts(fixture.project.clone(), run.clone())
        .await?
        .into_iter()
        .find(|artifact| artifact.node_instance_id == "implement" && artifact.port == "candidate")
        .context("The published candidate is missing")?;
    let ArtifactValue::Candidate(candidate) = &candidate_artifact.value else {
        unreachable!()
    };
    assert_eq!(candidate.target.run_id, source_run);
    assert_eq!(candidate.selection.untracked_paths, ["added.txt"]);
    // A worker that saw an earlier selection must preserve the simultaneous selection from this capture.
    let preserved = store
        .select_workflow_candidate(
            candidate.target.clone(),
            CandidateSelection {
                tracked_paths: Some(vec!["first.txt".into()]),
                untracked_paths: Vec::new(),
            },
            None,
        )
        .await?;
    assert_eq!(&preserved, candidate);
    let snapshot = store
        .materialize_selected_candidate(fixture.project.clone(), task.id, candidate.id.clone())
        .await?;
    assert_eq!(
        fs::read_to_string(snapshot.path().join("first.txt"))?.trim_end(),
        "frozen tracked text"
    );
    assert_eq!(
        fs::read_to_string(snapshot.path().join("added.txt"))?.trim_end(),
        "frozen new text"
    );

    let queued_check = store
        .next_workflow_unit_action()
        .await?
        .context("Candidate checks were not selected")?;
    store.set_job_queue_paused(true).await?;
    queued_check.execute(&store).await?;
    assert!(store.next_workflow_unit_action().await?.is_none());
    store.set_job_queue_paused(false).await?;
    store
        .next_workflow_unit_action()
        .await?
        .context("Candidate checks were not scheduled")?
        .execute(&store)
        .await?;
    assert!(store.next_workflow_unit_action().await?.is_none());
    let checked = store
        .inspect_workflow_execution(fixture.project.clone(), run.clone())
        .await?
        .context("The workflow has no check result")?;
    let check_receipt = checked
        .control
        .iter()
        .find(|receipt| receipt.node_id == "verify-candidate")
        .context("The check verdict is unavailable")?;
    assert_eq!(check_receipt.selected_ports, ["failed"]);
    assert!(
        checked.outputs.iter().any(
            |output| output.artifact.node_instance_id == "verify-candidate"
                && matches!(output.artifact.value, ArtifactValue::Checks(_))
        )
    );

    let pending = store
        .next_workflow_agent()
        .await?
        .context("The reviewer did not receive the sealed candidate")?;
    assert_eq!(pending.node, "review-candidate");
    assert_eq!(
        pending.request.inputs.get("candidate"),
        Some(&candidate_artifact.id)
    );
    let request = fixture_admission(&store, &pending, &fixture, &worktree).await?;
    pending.enqueue(&store, request).await?;
    let reviewer = claim(&store).await?;
    let review_run = reviewer.run.id.clone();
    assert!(reviewer.input[0].text.contains("omniagent-review"));
    store.mark_agent_running(review_run.clone()).await?;
    let review_dispatch: ContextSnapshot = serde_json::from_value(json!({
        "version": 13, "contextId": reviewer.context_id, "scope": reviewer.scope,
        "submittedMessages": reviewer.input, "baseCommit": candidate.base_commit,
        "nativeHistory": {"mode":"fresh"}, "questionRoute": reviewer.question_route,
        "scopedLaunch": {
            "provider": reviewer.scope.provider, "strategy": "candidate-review-fixture", "approved": {},
            "excluded": {}, "instructionSources": [], "inputs": [],
            "candidateCheckout": {
                "candidateId": candidate.id, "sourceRunId": candidate.target.run_id, "baseCommit": candidate.base_commit,
                "tree": candidate.tree, "workingDirectory": snapshot.path(),
            },
        },
    }))?;
    let dispatch_id = store
        .prepare_dispatch(
            review_run.clone(),
            review_dispatch,
            vec![b"fixture".to_vec()],
        )
        .await?;
    store
        .settle_dispatch(dispatch_id.clone(), Delivery::Submitted)
        .await?;
    store
        .record_agent_observation(
            review_run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some("review-session".into()),
                    turn_id: Some("review-turn".into()),
                    ..Default::default()
                },
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store.finish_observed_agent_attempt(review_run.clone(), RunStatus::Succeeded,
        Some(r#"{"verdict":"changes_requested","criteria":[{"index":1,"status":"unmet"}],"findings":[{"severity":"high","summary":"Missing test","evidence":"first.txt:1"}]}"#.into()), None, true).await?;
    store
        .next_workflow_delivery_result()
        .await?
        .context("The review publication was not queued")?
        .publish(&store)
        .await?;
    let view = store
        .inspect_workflow_execution(fixture.project.clone(), run.clone())
        .await?
        .context("The workflow was lost")?;
    let receipt = view
        .control
        .iter()
        .find(|receipt| receipt.node_id == "review-candidate")
        .context("The review did not settle")?;
    assert_eq!(receipt.selected_ports, ["reviewed"], "{:?}", receipt);
    assert_eq!(receipt.source_run_id.as_ref(), Some(&review_run));
    let output = view
        .outputs
        .iter()
        .find(|output| {
            output.artifact.node_instance_id == "review-candidate"
                && output.artifact.port == "report"
        })
        .context("The sealed review report is missing")?;
    let ArtifactValue::JsonObject(report) = &output.artifact.value else {
        unreachable!()
    };
    assert_eq!(report["verdict"], "changes_requested");
    assert_eq!(report["criteria"][0]["criterion"], "Scoped file updated");
    assert_eq!(report["provenance"]["tree"], candidate.tree);
    assert_eq!(report["provenance"]["dispatchId"], dispatch_id);
    assert_eq!(
        output
            .input_receipts
            .first()
            .and_then(|receipt| receipt.inputs.get("candidate")),
        Some(&candidate_artifact.id)
    );
    Ok(())
}
