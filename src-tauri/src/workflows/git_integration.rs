use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::{
    conversations::AttemptPlan,
    dispatch::{ContextSnapshot, Delivery},
    jobs::{AdmissionEnvironment, Job},
    providers::{Provider, SessionMode},
    repository::{
        self, AttemptWorktreeEvidence, ManagedWorktree,
        verification::{CandidateSelection, CandidateTarget, SelectedCandidate},
        verified_git::{CommitSource, DeliveryGitPolicy, VerifiedCommit},
    },
    runs::RunStatus,
    store::Store,
    tasks::{TaskDeliveryEvidence, TaskWorkflowState},
};

use super::{
    ArtifactValue, NodeInputs, WorkflowArtifact, WorkflowDeliveryUnit, WorkflowUnitOutcome,
    WorkflowUnitRequest, WorkflowUnitSubmission,
    tests::{ApprovedDeliveryFixture, approved_delivery_fixture, submission},
};

#[tokio::test]
async fn verified_commit_publishes_ordered_units_and_accepts_only_the_manual_working_branch()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("delivery.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    let [first, second] = fixture.units.as_slice() else {
        anyhow::bail!("expected two approved logical units");
    };
    let project = &fixture.project;
    let task = &first.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;

    let accepted_first =
        capture_and_review(&store, &fixture, &worktree, first, "first.txt").await?;
    let first_evidence = verified(
        &store,
        &fixture,
        first,
        commit(&store, &fixture, first, &accepted_first, None).await?,
    )
    .await?;
    assert_eq!(first_evidence.source, CommitSource::Automatic);
    assert_eq!(first_evidence.base_commit, worktree.base_commit);
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        first_evidence.sha
    );
    assert!(!first_evidence.pending_changes_retained);
    assert_ne!(
        store
            .project_task_delivery_state(project.clone(), task.clone())
            .await?
            .context("task delivery state is unavailable")?
            .state,
        TaskWorkflowState::Completed,
    );
    let progress = store
        .workflow_delivery_units(project.clone(), task.clone(), fixture.run.id.clone())
        .await?;
    assert_eq!(
        progress[0].commit_evidence_id.as_ref(),
        Some(&first_evidence.id)
    );
    assert!(progress[1].commit_evidence_id.is_none());

    store
        .set_delivery_git_policy(project.clone(), DeliveryGitPolicy::UserCommitsAndMerges)
        .await?;
    let accepted_second =
        capture_and_review(&store, &fixture, &worktree, second, "second.txt").await?;
    assert_eq!(
        commit(&store, &fixture, second, &accepted_second, None).await?,
        WorkflowUnitOutcome::WaitingForCommit,
    );
    let waiting = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("waiting task delivery state is unavailable")?;
    assert!(waiting.waiting_for_commit);
    assert_eq!(waiting.state, TaskWorkflowState::InReview);
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        first_evidence.sha
    );

    let separate = store
        .create_project_task(project.clone(), "Independent parent work".into())
        .await?;
    let other = store
        .create_project_subtask(project.clone(), separate.id, "Other work scope".into())
        .await?;
    let other_checkout = repository::provision_task_worktree(
        &store,
        temp.path().join("other-worktrees"),
        project.clone(),
        other.id,
        worktree.checkout_id.clone(),
    )
    .await?;
    fs::write(other_checkout.root.join("second.txt"), b"second.txt\n")?;
    let foreign = user_commit(&other_checkout.root, "second.txt", "A different Sub-Task commit")?;
    assert!(
        commit(&store, &fixture, second, &accepted_second, Some(foreign))
            .await
            .is_err()
    );

    let external = user_commit(&worktree.root, "second.txt", "Implement the second unit")?;
    let verified_second = commit(
        &store,
        &fixture,
        second,
        &accepted_second,
        Some(external.clone()),
    )
    .await?;
    let second_evidence = verified(&store, &fixture, second, verified_second.clone()).await?;
    assert_eq!(second_evidence.source, CommitSource::External);
    assert_eq!(second_evidence.sha, external);
    assert_eq!(second_evidence.base_commit, first_evidence.sha);
    assert_eq!(second_evidence.checkout_id, first_evidence.checkout_id);
    assert!(!second_evidence.pending_changes_retained);
    assert_eq!(
        commit(&store, &fixture, second, &accepted_second, Some(external)).await?,
        verified_second
    );

    let WorkflowUnitOutcome::Artifact { artifact: git_artifact } = verified_second else {
        unreachable!()
    };
    store
        .execute_workflow_unit(unit_request(
            &fixture,
            second,
            "result",
            NodeInputs::from([
                ("task".into(), fixture.task.id.clone()),
                    ("git".into(), git_artifact.id),
            ]),
            WorkflowUnitRequest::Result,
        ))
        .await?;
    let result = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("completed task delivery state is unavailable")?;
    assert_eq!(result.state, TaskWorkflowState::Completed);
    let completion = result
        .completion
        .as_ref()
        .context("task delivery receipt is unavailable")?;
    let TaskDeliveryEvidence::VerifiedCommit {
        unit_evidence_ids,
        completion_sha,
        ..
    } = &completion.evidence
    else {
        anyhow::bail!("task delivery must cite verified commits");
    };
    assert_eq!(
        unit_evidence_ids,
        &[first_evidence.id, second_evidence.id.clone()]
    );
    assert_eq!(completion_sha, &second_evidence.sha);

    git(&worktree.root, &["branch", "-m", "renamed-after-delivery"])?;
    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(
        reopened
            .verified_delivery_commit(project.clone(), task.clone(), second_evidence.id.clone())
            .await?,
        Some(second_evidence)
    );
    assert_eq!(
        reopened
            .project_task_delivery_state(project.clone(), task.clone())
            .await?,
        Some(result)
    );
    Ok(())
}

struct AcceptedUnit {
    candidate: WorkflowArtifact,
    acceptance: WorkflowArtifact,
}

async fn capture_and_review(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
    name: &str,
) -> Result<AcceptedUnit> {
    let unit_id = &unit.unit.id;
    let ArtifactValue::Release(release) = &fixture.release.value else {
        anyhow::bail!("fixture did not issue an implementation release");
    };
    let executable = Path::new(&fixture.checkout.repository_root).join("codex.exe");
    let mut implement = submission(
        &fixture.checkout,
        &fixture.run,
        "implement",
        Some(unit.plan_revision.clone()),
        SessionMode::ApprovedWrite,
    );
    implement.unit_id = Some(unit_id.clone());
    implement.scope.checkout = worktree.root.clone();
    implement.scope.executable = executable.clone();
    implement.request.checkout_id = Some(worktree.checkout_id.clone());
    implement.request.task_id = Some(unit.task_id.clone());
    implement.dispatch.release_id = Some(release.release.id.clone());
    implement.artifacts = NodeInputs::from([
        ("plan".into(), fixture.plan.id.clone()),
        ("worktree".into(), fixture.worktree.id.clone()),
        ("release".into(), fixture.release.id.clone()),
    ]);
    let implement = store.submit_workflow_agent(implement).await?;
    let env = AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    };
    let writer = claim_fixture_job(store, implement, env.clone()).await?;
    let run = writer.run.id;
    store.mark_agent_running(run.clone()).await?;
    ensure!(repository::capture_worktree_start(store, run.clone()).await?);
    fs::write(worktree.root.join(name), format!("{name}\n"))?;
    repository::save_worktree_candidate(store, run.clone()).await?;
    store
        .finish_agent_attempt(run.clone(), RunStatus::Succeeded, None)
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(fixture.project.clone(), run.clone())
        .await?
    else {
        anyhow::bail!("writer candidate was not saved");
    };
    let candidate = store
        .select_worktree_candidate(
            CandidateTarget {
                project_id: fixture.project.clone(),
                task_id: unit.task_id.clone(),
                unit_id: unit_id.clone(),
                capture_revision: artifact.revision,
                run_id: run,
            },
            CandidateSelection {
                tracked_paths: Some(vec![name.into()]),
                untracked_paths: Vec::new(),
            },
        )
        .await?;
    let artifact = store
        .publish_workflow_candidate(
            fixture.project.clone(),
            fixture.run.id.clone(),
            "implement".into(),
            unit.task_id.clone(),
            candidate.id.clone(),
        )
        .await?;

    let mut review = submission(
        &fixture.checkout,
        &fixture.run,
        "review-candidate",
        Some(unit.plan_revision.clone()),
        SessionMode::ReadOnly,
    );
    review.scope.provider = Provider::Claude;
    review.scope.profile_id = "candidate-reviewer".into();
    review.scope.checkout = worktree.root.clone();
    review.scope.executable = executable;
    review.request.checkout_id = Some(worktree.checkout_id.clone());
    review.request.task_id = Some(unit.task_id.clone());
    review.unit_id = Some(unit_id.clone());
    review.artifacts = NodeInputs::from([("candidate".into(), artifact.id.clone())]);
    let review = store.submit_workflow_agent(review).await?;
    let reviewer = claim_fixture_job(store, review, env).await?;
    seal_review(store, &candidate, &reviewer).await?;
    store
        .finish_agent_attempt(reviewer.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    let review = store
        .publish_workflow_artifact(
            fixture.project.clone(),
            fixture.run.id.clone(),
            "review-candidate".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: reviewer.run.id,
            },
        )
        .await?;
    let WorkflowUnitOutcome::Artifact { artifact: checks } = store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "verify-candidate",
            NodeInputs::from([("candidate".into(), artifact.id.clone())]),
            WorkflowUnitRequest::VerifyCandidate,
        ))
        .await?
    else {
        anyhow::bail!("missing candidate checks");
    };
    let WorkflowUnitOutcome::Artifact {
        artifact: acceptance,
    } = store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "accept",
            NodeInputs::from([
                ("candidate".into(), artifact.id.clone()),
                ("review".into(), review.id),
                ("checks".into(), checks.id),
            ]),
            WorkflowUnitRequest::AcceptCandidate {
                completion_criteria: unit.unit.completion_criteria.clone(),
            },
        ))
        .await?
    else {
        anyhow::bail!("missing candidate acceptance");
    };
    Ok(AcceptedUnit {
        candidate: artifact,
        acceptance,
    })
}

async fn seal_review(
    store: &Store,
    candidate: &SelectedCandidate,
    reviewer: &AttemptPlan,
) -> Result<()> {
    let materialized = store
        .materialize_selected_candidate(
            candidate.target.project_id.clone(),
            candidate.target.task_id.clone(),
            candidate.id.clone(),
        )
        .await?;
    store.mark_agent_running(reviewer.run.id.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 13,
        "contextId": reviewer.context_id,
        "scope": reviewer.scope,
        "submittedMessages": reviewer.input,
        "baseCommit": candidate.base_commit,
        "nativeHistory": { "mode": "fresh" },
        "scopedLaunch": {
            "provider": reviewer.scope.provider,
            "strategy": "candidate-review-fixture",
            "approved": {}, "excluded": {}, "instructionSources": [], "inputs": [],
            "candidateCheckout": {
                "candidateId": candidate.id,
                "sourceRunId": candidate.target.run_id,
                "baseCommit": candidate.base_commit,
                "tree": candidate.tree,
                "workingDirectory": materialized.path(),
            },
        },
    }))?;
    let dispatch = store
        .prepare_dispatch(
            reviewer.run.id.clone(),
            snapshot,
            vec![b"review fixture".to_vec()],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    Ok(())
}

async fn claim_fixture_job(store: &Store, job: Job, environment: AdmissionEnvironment) -> Result<AttemptPlan> {
    if let Some(attempt) = store.claim_next_job(environment.clone()).await? {
        return Ok(attempt);
    }
    let blocked = store
        .inspect_job(job.project_id, job.id, environment)
        .await?
        .context("fixture job disappeared")?;
    anyhow::bail!("fixture job was blocked by {:?}", blocked.blockers)
}

fn unit_request(
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    node: &str,
    inputs: NodeInputs,
    request: WorkflowUnitRequest,
) -> WorkflowUnitSubmission {
    WorkflowUnitSubmission {
        project_id: fixture.project.clone(),
        run_id: fixture.run.id.clone(),
        node_id: node.to_owned(),
        unit_id: unit.unit.id.clone(),
        task_id: unit.task_id.clone(),
        inputs,
        request,
    }
}

async fn commit(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    accepted: &AcceptedUnit,
    external_sha: Option<String>,
) -> Result<WorkflowUnitOutcome> {
    store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "commit",
            NodeInputs::from([
                ("candidate".into(), accepted.candidate.id.clone()),
                ("acceptance".into(), accepted.acceptance.id.clone()),
            ]),
            WorkflowUnitRequest::VerifyCommit { external_sha },
        ))
        .await
}

async fn verified(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    outcome: WorkflowUnitOutcome,
) -> Result<VerifiedCommit> {
    let WorkflowUnitOutcome::Artifact { artifact } = outcome else {
        anyhow::bail!("commit was not verified");
    };
    let ArtifactValue::Git(evidence) = artifact.value else {
        anyhow::bail!("missing Git evidence");
    };
    store
        .verified_delivery_commit(fixture.project.clone(), unit.task_id.clone(), evidence.id)
        .await?
        .context("verified commit receipt is unavailable")
}

fn user_commit(root: &Path, file: &str, message: &str) -> Result<String> {
    git(root, &["add", "--", file])?;
    git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@omniagent.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    )?;
    git(root, &["rev-parse", "HEAD"])
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git.exe")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
