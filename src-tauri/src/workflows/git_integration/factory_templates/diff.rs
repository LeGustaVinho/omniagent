use std::sync::Arc;

use crate::{
    providers::runtime::start_managed_turn,
    runner::Runner,
    workflows::{
        ManualWorkflowLaunch, WorkflowHostRequest, WorkflowHostSubmission, WorkflowLaunchSource,
        WorkflowRun, templates::diff::GitSelection,
    },
};

use super::*;

mod review_runner;

#[tokio::test]
async fn factory_review_diff_freezes_the_selected_tree_forks_read_only_work_and_reports_both_branches()
-> Result<()> {
    run_review_diff_factory(false).await
}

#[tokio::test]
async fn adaptive_review_diff_dispatches_focused_specialists_on_one_frozen_git_tree() -> Result<()>
{
    run_review_diff_factory(true).await
}

async fn run_review_diff_factory(adaptive: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("review-diff.sqlite"))?;
    let base_checkout = approved_delivery_repository(&store, temp.path()).await?;
    let delivery =
        approve_delivery_fixture_for_work(&store, temp.path(), base_checkout.clone(), None).await?;
    let project = delivery.project.clone();
    let unit = &delivery.units[0];
    let task = &unit.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("Missing managed candidate checkout")?
        .worktree;
    let writer = queue_implementation(&store, &delivery, &worktree, unit).await?;
    let capture =
        capture_implementation(&store, &delivery, &worktree, unit, &["first.txt"], writer).await?;
    let selection = CandidateSelection {
        tracked_paths: Some(vec!["first.txt".into()]),
        untracked_paths: vec![],
    };
    let candidate = store
        .select_worktree_candidate(capture, selection.clone())
        .await?;
    let template = if adaptive {
        WorkflowFactoryTemplate::ReviewDiffAdaptive
    } else {
        WorkflowFactoryTemplate::ReviewDiff
    };
    let draft = factory_draft(&store, &project, template).await?;
    let version = publish_factory_member(&store, &project, draft, "review-diff").await?;
    let version = WorkflowRevision {
        id: version.id,
        revision: version.revision,
    };
    let document = store
        .capture_repository_markdown(worktree.checkout_id.clone(), worktree.root.join("brief.md"))
        .await?;
    let run = if adaptive {
        store
            .launch_workflow_request(serde_json::from_value(json!({
                "projectId": project, "workflow": version, "checkoutId": worktree.checkout_id,
                "documentRevisionId": document.revision.id, "requestId": "selected-saved-diff",
                "automatic": true, "inputs": {},
            }))?)
            .await?
    } else {
        store
            .launch_workflow_manual(ManualWorkflowLaunch {
                project_id: project.clone(),
                workflow: version.clone(),
                source: WorkflowLaunchSource {
                    checkout_id: worktree.checkout_id.clone(),
                    revision_id: document.revision.id,
                },
                request_id: "selected-saved-diff".into(),
                work_id: None,
            })
            .await?
    };
    let current = store
        .current_workflow_host_outputs(project.clone(), run.id.clone())
        .await?;
    let inputs = NodeInputs::from([("document".into(), current["start"].clone())]);
    let invoke = |node: &str, inputs: NodeInputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    assert_eq!(
        store
            .workflow_diff_candidates(project.clone(), run.id.clone(), "read-git".into())
            .await?,
        [candidate.clone()]
    );
    assert!(
        store
            .workflow_diff_candidates(project.clone(), run.id.clone(), "report".into())
            .await
            .is_err()
    );
    let selected = GitSelection {
        task_id: task.clone(),
        candidate_id: candidate.id.clone(),
        expected_base: candidate.base_commit.clone(),
        expected_tree: candidate.tree.clone(),
    };
    let mut changed_tree = selected.clone();
    changed_tree.expected_tree = "different tree".into();
    assert!(
        store
            .execute_workflow_host(invoke(
                "read-git",
                inputs.clone(),
                WorkflowHostRequest::ReadGit(changed_tree)
            ))
            .await
            .is_err()
    );
    let read_request: WorkflowHostRequest = serde_json::from_value(json!({
        "action":"read_git", "taskId":selected.task_id, "candidateId":selected.candidate_id,
        "expectedBase":selected.expected_base, "expectedTree":selected.expected_tree
    }))?;
    let base_note = store
        .capture_repository_markdown(
            base_checkout.checkout_id.clone(),
            Path::new(&base_checkout.checkout_root).join("brief.md"),
        )
        .await?;
    let other = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: version,
            source: WorkflowLaunchSource {
                checkout_id: base_checkout.checkout_id.clone(),
                revision_id: base_note.revision.id,
            },
            request_id: "different-checkout".into(),
            work_id: None,
        })
        .await?;
    assert!(
        store
            .workflow_diff_candidates(project.clone(), other.id.clone(), "read-git".into())
            .await?
            .is_empty()
    );
    let other_outputs = store
        .current_workflow_host_outputs(project.clone(), other.id.clone())
        .await?;
    assert!(
        store
            .execute_workflow_host(WorkflowHostSubmission {
                project_id: project.clone(),
                run_id: other.id,
                node_id: "read-git".into(),
                inputs: NodeInputs::from([("document".into(), other_outputs["start"].clone())]),
                request: read_request.clone(),
            })
            .await
            .is_err()
    );
    let snapshot = store
        .execute_workflow_host(invoke("read-git", inputs.clone(), read_request.clone()))
        .await?;
    assert_eq!(
        store
            .execute_workflow_host(invoke("read-git", inputs, read_request))
            .await?,
        snapshot
    );
    let ArtifactValue::GitInspection(frozen) = &snapshot.value else {
        anyhow::bail!("Read Git must produce a separate evidence type");
    };
    assert_eq!(&frozen.candidate, &candidate);
    assert!(
        frozen
            .status
            .iter()
            .any(|entry| entry.ends_with("first.txt"))
    );
    assert_eq!(frozen.captured_head, candidate.base_commit);

    let writer = queue_implementation(&store, &delivery, &worktree, unit).await?;
    let changed =
        capture_implementation_changes(&store, &delivery, &worktree, unit, writer, |root| {
            fs::write(
                root.join("first.txt"),
                "Different live worktree after freezing the review\n",
            )?;
            Ok(())
        })
        .await?;
    let latest = store.select_worktree_candidate(changed, selection).await?;
    assert_ne!(latest.id, candidate.id);
    assert_ne!(latest.tree, candidate.tree);
    let git_program = std::env::split_paths(&std::env::var_os("PATH").context("Git needs PATH")?)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("git.exe"))
        .find(|path| path.is_file())
        .context("Git for Windows unavailable")?;
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![CandidateGate {
                name: "Prove the frozen Git snapshot".into(),
                required: true,
                action: CandidateGateAction::Command {
                    executable: git_program,
                    arguments: ["grep", "-q", "first.txt", "HEAD", "--", "first.txt"]
                        .map(str::to_owned)
                        .to_vec(),
                    timeout_seconds: Some(20),
                },
            }],
        )
        .await?;
    if adaptive {
        return verify_adaptive_diff(
            &store,
            &project,
            &run,
            &worktree.root,
            &snapshot,
            &candidate,
        )
        .await;
    }
    let inputs = NodeInputs::from([("git".into(), snapshot.id.clone())]);
    let target = store
        .workflow_operator_agent_target(&project, &run.id, "review", None, &inputs)
        .await?;
    assert_eq!(target.mode, SessionMode::ReadOnly);
    assert_eq!(target.task_id.as_ref(), Some(task));
    assert!(target.release_id.is_none());
    let runner = Arc::new(review_runner::DiffReviewRunner {
        live_checkout: worktree.root.clone(),
        tree: candidate.tree.clone(),
    });
    let mut review = submission(
        &delivery.checkout,
        &run,
        "review",
        None,
        SessionMode::ReadOnly,
    );
    review.scope.cli_version = "0.169.0-alpha.11.1".into();
    review.scope.account_binding = None;
    review.scope.executable = std::env::current_exe()?;
    review.scope.runner = runner.identity().into();
    review.scope.checkout = worktree.root.clone();
    review.request.checkout_id = Some(target.checkout_id);
    review.request.task_id = target.task_id;
    review.artifacts = inputs.clone();
    let job = store.submit_workflow_agent(review).await?;
    let attempt = claim_fixture_job(
        &store,
        job,
        AdmissionEnvironment {
            runner: runner.identity().into(),
            bridge_available: false,
        },
    )
    .await?;
    let attempt_id = attempt.run.id.clone();
    let conversation = attempt.conversation_id.clone();
    let reviewer = start_managed_turn(store.clone(), runner, attempt, None);
    let checks = store
        .execute_workflow_host(invoke(
            "check",
            inputs,
            WorkflowHostRequest::VerifyReviewDiff,
        ))
        .await?;
    let ArtifactValue::Checks(reference) = &checks.value else {
        unreachable!()
    };
    let evidence = store
        .candidate_verification(project.clone(), task.clone(), reference.id.clone())
        .await?
        .context("Missing saved Git check result")?;
    assert_eq!(evidence.candidate_id, candidate.id);
    assert!(evidence.allows_acceptance());
    assert!(
        store
            .execute_workflow_host(invoke(
                "report",
                NodeInputs::from([
                    ("git".into(), snapshot.id.clone()),
                    ("checks".into(), checks.id.clone()),
                ]),
                WorkflowHostRequest::PublishDiffReport
            ))
            .await
            .is_err()
    );
    let status =
        tokio::time::timeout(std::time::Duration::from_secs(30), reviewer.wait()).await??;
    assert_eq!(
        status,
        RunStatus::Succeeded,
        "{:?}",
        store
            .job_failure(project.clone(), attempt_id.clone())
            .await?
    );
    let sent = store
        .agent_dispatch(conversation, attempt_id.clone())
        .await?
        .context("Missing saved read-only diff dispatch")?;
    assert_eq!(sent.delivery, Delivery::Submitted);
    let dispatch = serde_json::to_value(&sent.snapshot)?;
    assert_eq!(
        dispatch["scopedLaunch"]["candidateCheckout"]["tree"],
        candidate.tree
    );
    let review = store
        .publish_workflow_artifact(
            project.clone(),
            run.id.clone(),
            "review".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: attempt_id.clone(),
            },
        )
        .await?;
    let report = store
        .execute_workflow_host(invoke(
            "report",
            NodeInputs::from([
                ("git".into(), snapshot.id.clone()),
                ("checks".into(), checks.id.clone()),
                ("review".into(), review.id),
            ]),
            WorkflowHostRequest::PublishDiffReport,
        ))
        .await?;
    let ArtifactValue::DiffReport(report) = report.value else {
        anyhow::bail!("Expected typed diff report");
    };
    assert_eq!(report.git_inspection_artifact_id, snapshot.id);
    assert_eq!(report.candidate_id, candidate.id);
    assert_eq!(report.review_run_id, attempt_id);
    assert_eq!(report.checks_id, reference.id);
    assert!(report.required_checks_passed);
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        worktree.base_commit
    );
    assert!(
        fs::read_to_string(worktree.root.join("first.txt"))?.contains("Different live worktree")
    );
    Ok(())
}

async fn verify_adaptive_diff(
    store: &Store,
    project: &str,
    run: &WorkflowRun,
    live_checkout: &Path,
    git_snapshot: &WorkflowArtifact,
    candidate: &SelectedCandidate,
) -> Result<()> {
    const FOCUS: &str = "Inspect latency in the selected file diff.";
    let submit_prompt = |stage: &'static str, response: serde_json::Value| async move {
        let pending = store
            .next_workflow_agent()
            .await?
            .context("Missing adaptive Prompt")?;
        assert_eq!(pending.fixture_identity(), (run.id.as_str(), stage));
        pending
            .enqueue(store, pending.fixture_submission(store).await?)
            .await?;
        let attempt = store
            .claim_next_job(AdmissionEnvironment {
                runner: "fixture-runner".into(),
                bridge_available: false,
            })
            .await?
            .context("Missing queued Prompt")?;
        crate::workflows::generic_agent::tests::complete_response(
            store,
            &attempt,
            &response.to_string(),
            None,
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    };
    submit_prompt(
        "review::execute",
        json!({
            "route":"concluido", "reason":"Latency needs an additional reviewer.",
            "outputs":{}, "delegations":[{"role":"performance", "focus":FOCUS}],
        }),
    )
    .await?;

    let runner = Arc::new(review_runner::DiffReviewRunner {
        live_checkout: live_checkout.to_path_buf(),
        tree: candidate.tree.clone(),
    });
    let mut correctness = None;
    for alias in ["correctness", "performance"] {
        let pending = store
            .next_workflow_agent()
            .await?
            .context("Missing selected diff reviewer")?;
        let stage = format!("review::delegate-{alias}");
        assert_eq!(
            pending.fixture_identity(),
            (run.id.as_str(), stage.as_str())
        );
        let mut submission = pending.fixture_submission(store).await?;
        assert_eq!(submission.scope.mode, SessionMode::ReadOnly);
        assert_eq!(submission.artifacts["git"], git_snapshot.id);
        assert!(submission.artifacts.contains_key("brief"));
        submission.scope.cli_version = "0.169.0-alpha.11.1".into();
        submission.scope.runner = runner.identity().into();
        pending.enqueue(store, submission).await?;
        let attempt = store
            .claim_next_job(AdmissionEnvironment {
                runner: runner.identity().into(),
                bridge_available: false,
            })
            .await?
            .context("Missing queued diff reviewer")?;
        assert_eq!(attempt.scope.mode, SessionMode::ReadOnly);
        if alias == "performance" {
            assert!(
                attempt.input[0].text.contains(FOCUS),
                "The invited reviewer needs the selected focus"
            );
        }
        let attempt_id = attempt.run.id.clone();
        let conversation = attempt.conversation_id.clone();
        let handle = start_managed_turn(store.clone(), runner.clone(), attempt, None);
        let status =
            tokio::time::timeout(std::time::Duration::from_secs(30), handle.wait()).await??;
        assert_eq!(
            status,
            RunStatus::Succeeded,
            "{:?}",
            store
                .job_failure(project.into(), attempt_id.clone())
                .await?
        );
        let dispatched = store
            .agent_dispatch(conversation, attempt_id.clone())
            .await?
            .context("Missing optional review dispatch")?;
        let saved = serde_json::to_value(&dispatched.snapshot)?;
        assert_eq!(
            saved["scopedLaunch"]["candidateCheckout"]["tree"],
            candidate.tree
        );
        assert_eq!(
            saved["scopedLaunch"]["candidateCheckout"]["candidateId"],
            candidate.id
        );
        let outputs = store
            .workflow_run_artifacts(project.into(), run.id.clone())
            .await?;
        let report = outputs
            .iter()
            .find(|artifact| artifact.node_instance_id == stage && artifact.port == "report")
            .context("Automatic diff specialist lost its report")?;
        let ArtifactValue::JsonObject(report) = &report.value else {
            unreachable!()
        };
        assert_eq!(report["provenance"]["runId"], attempt_id);
        assert_eq!(report["provenance"]["gitArtifactId"], git_snapshot.id);
        assert_eq!(report["provenance"]["tree"], candidate.tree);
        if alias == "correctness" {
            let review = outputs
                .into_iter()
                .find(|artifact| artifact.node_instance_id == stage && artifact.port == "review")
                .context("Automatic correctness review lost its native output")?;
            correctness = Some((attempt_id, review.id));
        }
    }
    assert!(store.next_workflow_agent().await?.is_none());
    let (attempt_id, review) = correctness.context("Missing mandatory review")?;
    let submit_host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.into(),
        run_id: run.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let checks = store
        .execute_workflow_host(submit_host(
            "check",
            NodeInputs::from([("git".into(), git_snapshot.id.clone())]),
            WorkflowHostRequest::VerifyReviewDiff,
        ))
        .await?;
    let report = store
        .execute_workflow_host(submit_host(
            "report",
            NodeInputs::from([
                ("git".into(), git_snapshot.id.clone()),
                ("checks".into(), checks.id),
                ("review".into(), review),
            ]),
            WorkflowHostRequest::PublishDiffReport,
        ))
        .await?;
    let ArtifactValue::DiffReport(report) = report.value else {
        unreachable!()
    };
    assert_eq!(report.candidate_id, candidate.id);
    assert_eq!(report.review_run_id, attempt_id);
    assert!(report.required_checks_passed);
    submit_prompt("consolidate::execute", json!({
        "route":"concluido", "reason":"Consolidate retained evidence.",
        "outputs":{"summary":{"summary":"Checks passed for the frozen diff."}}, "delegations":[],
    })).await?;
    let outputs = store
        .workflow_run_artifacts(project.into(), run.id.clone())
        .await?;
    let outcome = outputs
        .iter()
        .find(|artifact| {
            artifact.node_instance_id == "review::outcome" && artifact.port == "outcome"
        })
        .context("Missing saved adaptive review receipts")?;
    let ArtifactValue::JsonObject(outcome) = &outcome.value else {
        unreachable!()
    };
    assert_eq!(outcome["route"], "concluido");
    let calls = outcome["calls"]
        .as_array()
        .context("Missing review call statuses")?;
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["status"] == "completed")
            .count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["status"] == "skipped")
            .count(),
        3
    );
    assert!(
        outputs
            .iter()
            .any(|artifact| artifact.node_instance_id == "result")
    );
    assert!(
        fs::read_to_string(live_checkout.join("first.txt"))?.contains("Different live worktree")
    );
    Ok(())
}
