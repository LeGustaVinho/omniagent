use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::{
    conversations::AttemptPlan,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, Job, JobOrigin, JobPermission, JobRole, JobRule,
        JobWorkspace, ReleaseBlocker,
    },
    mcp_bridge::{RunBridge, invoke_test_mcp_tool},
    providers::{Provider, ProviderEvent, ProviderEventData, SessionMode},
    repository::{
        self, AttemptWorktreeEvidence, BranchStrategy, ManagedWorktree, ProjectCheckout,
        verification::{
            CandidateGate, CandidateGateAction, CandidateGateOutcome, CandidateSelection,
            CandidateTarget, CandidateVerification, SelectedCandidate,
        },
        verified_git::{CommitSource, DeliveryGitPolicy, VerifiedCommit},
    },
    runs::RunStatus,
    store::Store,
    tasks::{PlannedAssignment, TaskDeliveryEvidence, TaskRunRole, TaskWorkflowState},
};

use super::{
    ArtifactValue, NodeInputs, WorkflowAgentSubmission, WorkflowArtifact, WorkflowDeliveryUnit,
    WorkflowUnitOutcome, WorkflowUnitRequest, WorkflowUnitSubmission,
    tests::{
        ApprovedDeliveryFixture, approve_delivery_fixture_for_work, approved_delivery_fixture,
        approved_delivery_repository, submission,
    },
};

mod commit_intent;
mod history_integration;
mod live_cli_acceptance;

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
    let foreign = user_commit(
        &other_checkout.root,
        "second.txt",
        "A different Sub-Task commit",
    )?;
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

    let WorkflowUnitOutcome::Artifact {
        artifact: git_artifact,
    } = verified_second
    else {
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

#[tokio::test]
async fn verified_subtask_commits_preserve_writer_ownership_and_hierarchy_history() -> Result<()> {
    exercise_subtask_delivery(BranchStrategy::Independent).await?;
    exercise_subtask_delivery(BranchStrategy::SharedBranchPerTask).await
}

async fn exercise_subtask_delivery(strategy: BranchStrategy) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("delivery.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    store
        .set_project_branch_strategy(project.clone(), strategy)
        .await?;
    let epic = store
        .create_project_epic(project.clone(), "Scoped repository delivery".into())
        .await?;
    let parent = store
        .create_project_task(project.clone(), "Deliver both changes".into())
        .await?;
    store
        .set_project_task_epic(project.clone(), parent.id.clone(), Some(epic.id.clone()))
        .await?;
    let first_task = store
        .create_project_subtask(project.clone(), parent.id.clone(), "First change".into())
        .await?;
    let second_task = store
        .create_project_subtask(project.clone(), parent.id.clone(), "Second change".into())
        .await?;

    let first = approve_delivery_fixture_for_work(
        &store,
        temp.path(),
        repo.clone(),
        Some(first_task.id.clone()),
    )
    .await?;
    let second = approve_delivery_fixture_for_work(
        &store,
        temp.path(),
        repo.clone(),
        Some(second_task.id.clone()),
    )
    .await?;
    let first_unit = &first.units[0];
    let second_unit = &second.units[0];
    let shared = strategy == BranchStrategy::SharedBranchPerTask;
    let first_owner = if shared { &parent.id } else { &first_task.id };
    let second_owner = if shared { &parent.id } else { &second_task.id };
    let first_tree = store
        .live_managed_worktree(project.clone(), first_owner.clone())
        .await?
        .context("first managed worktree is unavailable")?
        .worktree;
    let second_tree = store
        .live_managed_worktree(project.clone(), second_owner.clone())
        .await?
        .context("second managed worktree is unavailable")?
        .worktree;
    assert_eq!(first_tree.checkout_id == second_tree.checkout_id, shared);
    assert_eq!(first_tree.branch == second_tree.branch, shared);
    assert_eq!(&first_tree.owner_task_id, first_owner);
    assert_eq!(&second_tree.owner_task_id, second_owner);
    assert_eq!(first_tree.checkout_id, first_unit.checkout_id);
    assert_eq!(second_tree.checkout_id, second_unit.checkout_id);

    let accepted_first =
        capture_and_review(&store, &first, &first_tree, first_unit, "first.txt").await?;
    let ArtifactValue::Candidate(first_candidate) = &accepted_first.candidate.value else {
        anyhow::bail!("missing first candidate");
    };
    let second_job = queue_implementation(&store, &second, &second_tree, second_unit).await?;
    assert_eq!(
        second_job.worktree_owner_task_id.as_ref(),
        Some(second_owner)
    );
    let env = fixture_admission();
    let blocked = store
        .inspect_job(project.clone(), second_job.id.clone(), env.clone())
        .await?
        .context("second writer disappeared from the queue")?;
    let independent_acceptance = if shared {
        assert!(blocked.blockers.iter().any(|blocker| matches!(
            blocker,
            AdmissionBlocker::PendingWorktreeChanges { checkout_id, scope_task_id, producing_run_id }
                if checkout_id == &first_tree.checkout_id && scope_task_id == &first_task.id
                    && producing_run_id.as_ref() == Some(&first_candidate.target.run_id)
        )));
        assert!(store.claim_next_job(env.clone()).await?.is_none());
        None
    } else {
        assert!(blocked.blockers.is_empty(), "{:?}", blocked.blockers);
        Some(
            capture_queued_implementation_and_review(
                &store,
                &second,
                &second_tree,
                second_unit,
                "second.txt",
                second_job.clone(),
            )
            .await?,
        )
    };

    let first_evidence = verified(
        &store,
        &first,
        first_unit,
        commit(&store, &first, first_unit, &accepted_first, None).await?,
    )
    .await?;
    assert_eq!(first_evidence.task_id, first_task.id);
    assert_eq!(first_evidence.branch, first_tree.branch);
    assert_eq!(first_evidence.capture_run_id, first_candidate.target.run_id);
    assert!(!first_evidence.pending_changes_retained);
    assert_ne!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Completed)
    );
    assert!(
        store
            .verified_delivery_commit(
                project.clone(),
                second_task.id.clone(),
                first_evidence.id.clone()
            )
            .await?
            .is_none()
    );

    let accepted_second = if let Some(accepted) = independent_acceptance {
        accepted
    } else {
        let handed_off = store
            .inspect_job(project.clone(), second_job.id.clone(), env)
            .await?
            .context("second queued writer disappeared after the commit")?;
        assert!(handed_off.blockers.is_empty(), "{:?}", handed_off.blockers);
        capture_queued_implementation_and_review(
            &store,
            &second,
            &second_tree,
            second_unit,
            "second.txt",
            second_job.clone(),
        )
        .await?
    };
    let second_evidence = verified(
        &store,
        &second,
        second_unit,
        commit(&store, &second, second_unit, &accepted_second, None).await?,
    )
    .await?;
    let second_attempt = store
        .workflow_attempt(project.clone(), second_evidence.capture_run_id.clone())
        .await?
        .context("second commit attempt is unavailable")?;
    assert_eq!(second_attempt.job_id, second_job.id);
    assert_eq!(second_evidence.task_id, second_task.id);
    assert_eq!(second_evidence.branch, second_tree.branch);
    assert_eq!(second_evidence.checkout_id, second_tree.checkout_id);
    assert_eq!(
        second_evidence.base_commit,
        if shared {
            first_evidence.sha.as_str()
        } else {
            second_tree.base_commit.as_str()
        }
    );
    assert_eq!(
        git(
            &first_tree.root,
            &["show", &format!("{}:second.txt", first_evidence.sha)]
        )?,
        "original second"
    );
    assert_eq!(
        git(
            &second_tree.root,
            &["show", &format!("{}:first.txt", second_evidence.sha)]
        )?,
        if shared {
            "first.txt"
        } else {
            "original first"
        }
    );
    assert_eq!(
        git(
            &second_tree.root,
            &["show", &format!("{}:second.txt", second_evidence.sha)]
        )?,
        "second.txt"
    );
    assert_eq!(
        store
            .project_task_delivery_state(project.clone(), parent.id.clone())
            .await?
            .map(|state| state.state),
        Some(TaskWorkflowState::Completed)
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Completed)
    );

    store
        .reopen_task_delivery(project.clone(), first_task.id.clone())
        .await?;
    let reopened = store
        .project_task_delivery_state(project.clone(), first_task.id.clone())
        .await?
        .context("reopened Sub-Task is no longer visible")?;
    assert!(!matches!(
        reopened.state,
        TaskWorkflowState::Completed | TaskWorkflowState::Cancelled
    ));
    assert!(reopened.completion.is_none());
    assert_ne!(
        store
            .project_task_delivery_state(project.clone(), parent.id.clone())
            .await?
            .map(|state| state.state),
        Some(TaskWorkflowState::Completed)
    );
    assert_ne!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Completed)
    );

    drop(store);
    let reopened = Store::open(database)?;
    assert_ne!(
        reopened
            .project_task_delivery_state(project.clone(), parent.id)
            .await?
            .map(|state| state.state),
        Some(TaskWorkflowState::Completed)
    );
    assert_ne!(
        reopened
            .project_epic_delivery_state(project.clone(), epic.id)
            .await?,
        Some(TaskWorkflowState::Completed)
    );
    let history = reopened
        .project_task_deliveries(project.clone(), first_task.id.clone())
        .await?;
    assert!(matches!(history.as_slice(), [receipt] if matches!(
        &receipt.evidence, TaskDeliveryEvidence::VerifiedCommit { completion_sha, unit_evidence_ids, .. }
            if completion_sha == &first_evidence.sha && unit_evidence_ids == std::slice::from_ref(&first_evidence.id)
    )));
    assert_eq!(
        reopened
            .verified_delivery_commit(project.clone(), first_task.id, first_evidence.id.clone())
            .await?,
        Some(first_evidence)
    );
    let task = reopened
        .project_task_delivery_state(project.clone(), second_task.id)
        .await?
        .context("second completed Sub-Task is no longer visible")?;
    assert_eq!(task.state, TaskWorkflowState::Completed);
    Ok(())
}

#[tokio::test]
async fn changed_candidate_needs_fresh_required_checks_and_preserves_review_participation()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("delivery.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = approved_delivery_repository(&store, temp.path()).await?;
    let project = checkout.project_id.clone();
    let task = store
        .create_project_task(project.clone(), "Checked and reviewed change".into())
        .await?
        .id;
    let fixture =
        approve_delivery_fixture_for_work(&store, temp.path(), checkout, Some(task.clone()))
            .await?;
    let [unit] = fixture.units.as_slice() else {
        anyhow::bail!("expected one logical unit");
    };
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("managed worktree is unavailable")?
        .worktree;
    let base = git(&worktree.root, &["rev-parse", "HEAD"])?;
    let executable = PathBuf::from(std::env::var_os("SystemRoot").context("missing SystemRoot")?)
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let policy_revision = store.set_candidate_gate_policy(project.clone(), vec![CandidateGate {
        name: "required build".into(),
        required: true,
        action: CandidateGateAction::Command {
            executable,
            arguments: vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
                "if ((Get-Content -LiteralPath 'second.txt' -Raw).Trim() -eq 'second.txt') { exit 0 } else { exit 17 }".into()],
            timeout_seconds: Some(30),
        },
    }]).await?;

    let first_assignment = PlannedAssignment {
        model: Some("original-model".into()),
        ..first_reviewer()
    };
    let mut details = store
        .project_task_details(project.clone(), task.clone())
        .await?
        .context("task details are unavailable")?;
    details.planned_assignments = vec![first_assignment.clone()];
    store
        .set_project_task_details(project.clone(), task.clone(), details.clone())
        .await?;
    let job = queue_implementation(&store, &fixture, &worktree, unit).await?;
    let capture = capture_implementation(
        &store,
        &fixture,
        &worktree,
        unit,
        &["first.txt", "second.txt"],
        job,
    )
    .await?;

    let incomplete = select_candidate(&store, &fixture, capture.clone(), &["first.txt"]).await?;
    let reviewed_incomplete = review_candidate(
        &store,
        &fixture,
        &worktree,
        unit,
        &incomplete,
        &first_assignment,
    )
    .await?;
    let failed = verify_candidate(&store, &fixture, unit, &incomplete).await?;
    let failed_check = checked_evidence(&store, &project, &task, &failed).await?;
    assert_eq!(failed_check.policy_revision, policy_revision);
    assert!(
        matches!(failed_check.gates.as_slice(), [result] if result.gate.required &&
        matches!(&result.outcome, Some(CandidateGateOutcome::Failed { exit_code: 17, .. })))
    );
    assert!(!failed_check.allows_acceptance());
    let error = accept_candidate(
        &store,
        &fixture,
        unit,
        &incomplete,
        &reviewed_incomplete,
        &failed,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("required checks"), "{error:#}");
    let blocked = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("task state is unavailable after failed checks")?;
    assert_ne!(blocked.state, TaskWorkflowState::Completed);
    assert!(blocked.completion.is_none());
    assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, base);

    let approved_candidate =
        select_candidate(&store, &fixture, capture.clone(), &["second.txt"]).await?;
    let reviewed_approved = review_candidate(
        &store,
        &fixture,
        &worktree,
        unit,
        &approved_candidate,
        &first_assignment,
    )
    .await?;
    let passed = verify_candidate(&store, &fixture, unit, &approved_candidate).await?;
    let approved_check = checked_evidence(&store, &project, &task, &passed).await?;
    assert_eq!(approved_check.policy_revision, policy_revision);
    assert!(approved_check.allows_acceptance());
    let acceptance = accept_candidate(
        &store,
        &fixture,
        unit,
        &approved_candidate,
        &reviewed_approved,
        &passed,
    )
    .await?;
    let ArtifactValue::Acceptance(reference) = &acceptance.value else {
        anyhow::bail!("expected acceptance");
    };
    let prior_acceptance = store
        .current_candidate_acceptance(project.clone(), task.clone(), reference.evidence.id.clone())
        .await?;
    let prior_reviewers =
        review_participants(&store, &project, &task, &first_assignment.role).await?;
    assert_eq!(prior_reviewers.len(), 2);
    for participant in &prior_reviewers {
        assert_eq!(participant.provider, first_assignment.provider);
        assert_eq!(participant.profile_id, first_assignment.profile_id);
        assert_eq!(
            participant.requested_model.as_deref(),
            Some("original-model")
        );
        assert_eq!(
            participant.reported_model.as_deref(),
            Some("observed-original-model")
        );
    }

    let reassigned = PlannedAssignment {
        role: first_assignment.role.clone(),
        provider: Provider::Codex,
        profile_id: "reassigned-reviewer".into(),
        model: Some("replacement-model".into()),
    };
    details.planned_assignments = vec![reassigned.clone()];
    store
        .set_project_task_details(project.clone(), task.clone(), details.clone())
        .await?;
    let changed = select_candidate(&store, &fixture, capture, &["first.txt", "second.txt"]).await?;
    let ArtifactValue::Candidate(new_candidate) = &changed.value else {
        anyhow::bail!("expected new candidate");
    };
    assert_ne!(new_candidate.id, prior_acceptance.candidate.id);
    assert_ne!(new_candidate.tree, prior_acceptance.candidate.tree);
    let pending = store
        .current_candidate_verification(project.clone(), task.clone(), new_candidate.id.clone())
        .await
        .unwrap_err();
    assert!(
        pending.to_string().contains("current project gate policy"),
        "{pending:#}"
    );
    let old = AcceptedUnit {
        candidate: approved_candidate,
        acceptance,
    };
    let rejected = commit(&store, &fixture, unit, &old, None)
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("superseded"), "{rejected:#}");
    assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, base);
    let reviewed_changed =
        review_candidate(&store, &fixture, &worktree, unit, &changed, &reassigned).await?;
    assert!(
        accept_candidate(&store, &fixture, unit, &changed, &reviewed_changed, &passed)
            .await
            .is_err()
    );
    assert!(
        store
            .project_task_deliveries(project.clone(), task.clone())
            .await?
            .is_empty()
    );

    let verified_change = verify_candidate(&store, &fixture, unit, &changed).await?;
    let new_check = checked_evidence(&store, &project, &task, &verified_change).await?;
    assert_eq!(new_check.candidate_id, new_candidate.id);
    assert_eq!(new_check.policy_revision, policy_revision);
    assert_ne!(new_check.id, approved_check.id);
    assert!(new_check.allows_acceptance());
    let accepted_change = AcceptedUnit {
        acceptance: accept_candidate(
            &store,
            &fixture,
            unit,
            &changed,
            &reviewed_changed,
            &verified_change,
        )
        .await?,
        candidate: changed,
    };
    let result = commit(&store, &fixture, unit, &accepted_change, None).await?;
    let evidence = verified(&store, &fixture, unit, result).await?;
    assert_eq!(evidence.verification_id, new_check.id);
    assert_eq!(
        store
            .project_task_delivery_state(project.clone(), task.clone())
            .await?
            .context("completed task state is unavailable")?
            .state,
        TaskWorkflowState::Completed
    );

    drop(store);
    let reopened = Store::open(database)?;
    let updated = reopened
        .project_task_details(project.clone(), task.clone())
        .await?
        .context("reopened task details are unavailable")?;
    assert_eq!(updated.planned_assignments, details.planned_assignments);
    assert_eq!(updated.workflow_state, TaskWorkflowState::Completed);
    let saved = review_participants(&reopened, &project, &task, &first_assignment.role).await?;
    assert_eq!(saved.len(), 3);
    assert_eq!(&saved[..2], prior_reviewers.as_slice());
    let latest = &saved[2];
    let ArtifactValue::Review { run_id } = reviewed_changed.value else {
        anyhow::bail!("expected review");
    };
    assert_eq!(latest.run_id, run_id);
    assert_eq!(latest.provider, reassigned.provider);
    assert_eq!(latest.profile_id, reassigned.profile_id);
    assert_eq!(latest.requested_model.as_deref(), Some("replacement-model"));
    assert_eq!(
        latest.reported_model.as_deref(),
        Some("observed-replacement-model")
    );
    assert_eq!(
        reopened
            .candidate_acceptance(project, task, prior_acceptance.id.clone())
            .await?,
        Some(prior_acceptance)
    );
    Ok(())
}

async fn checked_evidence(
    store: &Store,
    project: &str,
    task: &str,
    artifact: &WorkflowArtifact,
) -> Result<CandidateVerification> {
    let ArtifactValue::Checks(evidence) = &artifact.value else {
        anyhow::bail!("expected check evidence");
    };
    store
        .candidate_verification(project.into(), task.into(), evidence.id.clone())
        .await?
        .context("candidate check receipt is unavailable")
}

#[tokio::test]
async fn builtin_mcp_nested_writer_inherits_release_and_blocks_wrong_or_missing_scope() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("delivery.sqlite"))?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    let project = &fixture.project;
    let unit = &fixture.units[0];
    let task = &unit.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("released worktree is unavailable")?
        .worktree;
    let ArtifactValue::Release(reference) = &fixture.release.value else {
        anyhow::bail!("built-in approval did not issue release evidence");
    };
    let other = store
        .create_project_task(project.clone(), "Other managed scope".into())
        .await?;
    let other_worktree = repository::provision_task_worktree(
        &store,
        temp.path().join("other-worktrees"),
        project.clone(),
        other.id,
        worktree.checkout_id.clone(),
    )
    .await?;
    // An allowed Agent job still needs the originating workflow release.
    store
        .set_project_job_rules(
            project.clone(),
            Vec::new(),
            vec![JobRule {
                role: JobRole::Agent,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    let env = AdmissionEnvironment {
        bridge_available: true,
        ..fixture_admission()
    };
    let mut implementing = implementation_submission(&fixture, &worktree, unit)?;
    implementing.request.requires_bridge = true;
    let parent_job = store.submit_workflow_agent(implementing).await?;
    let parent = claim_fixture_job(&store, parent_job, env.clone()).await?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    assert!(repository::capture_worktree_start(&store, parent.run.id.clone()).await?);
    let origin = store
        .attempt_pipeline_origin(project.clone(), parent.run.id.clone())
        .await?
        .context("built-in parent did not keep pipeline provenance")?;
    assert_eq!(origin.pipeline_run_id, fixture.run.id);
    assert_eq!(origin.plan_revision.as_ref(), Some(&unit.plan_revision));
    assert_eq!(origin.release.as_ref(), Some(&reference.release));
    let parent_bridge = RunBridge::start(&store, parent.run.id.clone()).await?;
    let child_id = submit_mcp_writer(&parent_bridge, "first-level", json!({
        "mode": "approved_write", "instructions": "Delegate the released file change", "requiresBridge": true,
    })).await?;
    let wrong_scope = submit_mcp_writer(
        &parent_bridge,
        "other-scope",
        json!({
            "mode": "approved_write", "instructions": "Try another registered work scope",
            "checkoutId": other_worktree.checkout_id, "taskId": other_worktree.owner_task_id,
        }),
    )
    .await?;
    let missing = submit_mcp_writer(
        &parent_bridge,
        "missing-release-guard",
        json!({
            "mode": "approved_write", "instructions": "Exercise admission with the release removed",
        }),
    )
    .await?;
    let child_job = store
        .job(project.clone(), child_id)
        .await?
        .context("MCP child job is unavailable")?;
    assert_eq!(child_job.pipeline.as_ref(), Some(&origin));
    assert_eq!(child_job.task_id.as_ref(), Some(task));
    assert_eq!(child_job.checkout_id, worktree.checkout_id);
    let changed = store
        .inspect_job(project.clone(), wrong_scope, env.clone())
        .await?
        .context("wrong-scope job is unavailable")?;
    assert_eq!(changed.job.pipeline.as_ref(), Some(&origin));
    assert!(
        changed
            .blockers
            .contains(&AdmissionBlocker::PipelineRelease {
                reason: ReleaseBlocker::WrongScope
            })
    );

    // Normal typed submissions preserve an inherited release. Simulate a
    // missing persisted association separately to reach the admission guard.
    let release = reference.release.id.clone();
    store.execute({
        let missing = missing.clone();
        move |db| {
            let removed = db.execute(
                "UPDATE job_pipeline_origins SET release_id = NULL WHERE job_id = ?1 AND release_id = ?2",
                (&missing, release),
            )?;
            ensure!(removed == 1, "fixture job did not have the original release");
            Ok(())
        }
    }).await?;
    let inspected = store
        .inspect_job(project.clone(), missing, env.clone())
        .await?
        .context("missing-release job is unavailable")?;
    assert!(
        inspected
            .blockers
            .contains(&AdmissionBlocker::PipelineRelease {
                reason: ReleaseBlocker::Missing
            })
    );
    assert!(
        store.claim_next_job(env.clone()).await?.is_none(),
        "parent still owns the worktree"
    );
    repository::save_worktree_candidate(&store, parent.run.id.clone()).await?;
    store
        .finish_agent_attempt(parent.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    drop(parent_bridge);

    let child = claim_fixture_job(&store, child_job, env.clone()).await?;
    assert_eq!(
        store
            .attempt_pipeline_origin(project.clone(), child.run.id.clone())
            .await?,
        Some(origin.clone())
    );
    store.mark_agent_running(child.run.id.clone()).await?;
    assert!(repository::capture_worktree_start(&store, child.run.id.clone()).await?);
    let nested_bridge = RunBridge::start(&store, child.run.id.clone()).await?;
    let nested_id = submit_mcp_writer(
        &nested_bridge,
        "second-level",
        json!({
            "mode": "approved_write", "instructions": "Write the released worktree",
        }),
    )
    .await?;
    let nested_job = store
        .job(project.clone(), nested_id.clone())
        .await?
        .context("nested MCP writer is unavailable")?;
    assert!(
        matches!(&nested_job.origin, JobOrigin::Agent { run_id, .. } if run_id == &child.run.id)
    );
    assert_eq!(nested_job.pipeline.as_ref(), Some(&origin));
    assert_eq!(nested_job.task_id.as_ref(), Some(task));
    assert_eq!(nested_job.checkout_id, worktree.checkout_id);
    assert!(
        store.claim_next_job(env.clone()).await?.is_none(),
        "child still owns the worktree"
    );
    repository::save_worktree_candidate(&store, child.run.id.clone()).await?;
    store
        .finish_agent_attempt(child.run.id, RunStatus::Succeeded, None)
        .await?;
    drop(nested_bridge);

    let nested = claim_fixture_job(&store, nested_job, env.clone()).await?;
    assert_eq!(nested.scope.checkout, worktree.root);
    assert_eq!(nested.scope.mode, SessionMode::ApprovedWrite);
    assert_eq!(
        store
            .attempt_pipeline_origin(project.clone(), nested.run.id.clone())
            .await?,
        Some(origin)
    );
    store.mark_agent_running(nested.run.id.clone()).await?;
    assert!(repository::capture_worktree_start(&store, nested.run.id.clone()).await?);
    let id = nested.run.id.clone();
    store
        .execute(move |db| {
            ensure!(
                crate::jobs::record_launch_intent(db, &id)? == Some(true),
                "nested writer lost its release before launch"
            );
            Ok(())
        })
        .await?;
    let changed_name = "nested-writer.txt";
    fs::write(
        nested.scope.checkout.join(changed_name),
        b"written by the released nested attempt\n",
    )?;
    repository::save_worktree_candidate(&store, nested.run.id.clone()).await?;
    store
        .finish_agent_attempt(nested.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.clone(), nested.run.id.clone())
        .await?
    else {
        anyhow::bail!("nested writer did not persist its Git capture");
    };
    assert_eq!(artifact.start.worktree.checkout_id, worktree.checkout_id);
    assert!(artifact.candidate.changes.as_ref().is_some_and(|changes| {
        changes
            .untracked
            .iter()
            .any(|file| file.path == changed_name)
    }));
    assert_eq!(
        fs::read(worktree.root.join(changed_name))?,
        b"written by the released nested attempt\n"
    );
    let settled = store
        .job(project.clone(), nested_id)
        .await?
        .and_then(|job| job.latest_attempt)
        .context("nested writer did not settle")?;
    assert_eq!(settled.run_id, nested.run.id);
    assert_eq!(settled.status, RunStatus::Succeeded);

    // A real built-in read-only review has no implementation release even
    // though it shares the same plan. Its tool call cannot create a writer.
    let ArtifactValue::Approval(decision) = &fixture.decision.value else {
        anyhow::bail!("workflow approval decision is unavailable");
    };
    let source = ProjectCheckout {
        project_id: project.clone(),
        checkout_id: decision.source_checkout_id.clone(),
        checkout_root: fixture.checkout.repository_root.clone(),
        repository_root: fixture.checkout.repository_root.clone(),
    };
    let mut review = submission(
        &source,
        &fixture.run,
        "review-plan",
        Some(unit.plan_revision.clone()),
        SessionMode::ReadOnly,
    );
    review.scope.provider = Provider::Claude;
    review.scope.profile_id = "read-only-release-proof".into();
    review.request.requires_bridge = true;
    review.artifacts = NodeInputs::from([("plan".into(), fixture.plan.id.clone())]);
    let review = store.submit_workflow_agent(review).await?;
    let unreleased = claim_fixture_job(&store, review, env.clone()).await?;
    store.mark_agent_running(unreleased.run.id.clone()).await?;
    let read_origin = store
        .attempt_pipeline_origin(project.clone(), unreleased.run.id.clone())
        .await?
        .context("read-only origin lost its workflow")?;
    assert!(read_origin.release.is_none());
    let read_bridge = RunBridge::start(&store, unreleased.run.id.clone()).await?;
    let refused = invoke_test_mcp_tool(&read_bridge, "submit_job", json!({
        "requestId": "write-without-release",
        "request": { "mode": "approved_write", "instructions": "Try the prepared worktree without a release",
            "checkoutId": worktree.checkout_id, "taskId": task },
    })).await?;
    assert!(refused["error"].is_object(), "{refused}");
    let run = unreleased.run.id.clone();
    let created = store.execute(move |db| Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE origin_run_id = ?1 AND request_id = 'write-without-release')",
        [run], |row| row.get::<_, bool>(0),
    )?)).await?;
    assert!(!created);
    store
        .finish_agent_attempt(unreleased.run.id, RunStatus::Succeeded, None)
        .await?;
    assert!(store.claim_next_job(env).await?.is_none());
    Ok(())
}

async fn submit_mcp_writer(
    bridge: &RunBridge,
    request_id: &str,
    request: serde_json::Value,
) -> Result<String> {
    let reply = invoke_test_mcp_tool(
        bridge,
        "submit_job",
        json!({"requestId": request_id, "request": request}),
    )
    .await?;
    reply["result"]["structuredContent"]["jobId"]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("MCP fixture writer request {request_id} was rejected: {reply}"))
}

async fn review_participants(
    store: &Store,
    project: &str,
    task: &str,
    role: &str,
) -> Result<Vec<crate::tasks::TaskRun>> {
    let role = TaskRunRole::Workflow(role.into());
    Ok(store
        .project_task_runs(project.into(), task.into())
        .await?
        .into_iter()
        .filter(|run| run.role == role)
        .collect())
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
    let implement = queue_implementation(store, fixture, worktree, unit).await?;
    capture_queued_implementation_and_review(store, fixture, worktree, unit, name, implement).await
}

async fn queue_implementation(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
) -> Result<Job> {
    store
        .submit_workflow_agent(implementation_submission(fixture, worktree, unit)?)
        .await
}

fn implementation_submission(
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
) -> Result<WorkflowAgentSubmission> {
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
    Ok(implement)
}

async fn capture_queued_implementation_and_review(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
    name: &str,
    implement: Job,
) -> Result<AcceptedUnit> {
    let target = capture_implementation(store, fixture, worktree, unit, &[name], implement).await?;
    let candidate = select_candidate(store, fixture, target, &[name]).await?;
    let review = review_candidate(
        store,
        fixture,
        worktree,
        unit,
        &candidate,
        &first_reviewer(),
    )
    .await?;
    let checks = verify_candidate(store, fixture, unit, &candidate).await?;
    let acceptance = accept_candidate(store, fixture, unit, &candidate, &review, &checks).await?;
    Ok(AcceptedUnit {
        candidate,
        acceptance,
    })
}

async fn capture_implementation(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
    files: &[&str],
    implement: Job,
) -> Result<CandidateTarget> {
    let env = fixture_admission();
    let writer = claim_fixture_job(store, implement, env).await?;
    let run = writer.run.id;
    store.mark_agent_running(run.clone()).await?;
    ensure!(repository::capture_worktree_start(store, run.clone()).await?);
    for name in files {
        fs::write(worktree.root.join(name), format!("{name}\n"))?;
    }
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
    Ok(CandidateTarget {
        project_id: fixture.project.clone(),
        task_id: unit.task_id.clone(),
        unit_id: unit.unit.id.clone(),
        capture_revision: artifact.revision,
        run_id: run,
    })
}

async fn select_candidate(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    target: CandidateTarget,
    paths: &[&str],
) -> Result<WorkflowArtifact> {
    let task = target.task_id.clone();
    let candidate = store
        .select_worktree_candidate(
            target,
            CandidateSelection {
                tracked_paths: Some(paths.iter().map(|path| (*path).to_owned()).collect()),
                untracked_paths: Vec::new(),
            },
        )
        .await?;
    store
        .publish_workflow_candidate(
            fixture.project.clone(),
            fixture.run.id.clone(),
            "implement".into(),
            task,
            candidate.id,
        )
        .await
}

fn first_reviewer() -> PlannedAssignment {
    PlannedAssignment {
        role: "candidate reviewer".into(),
        provider: Provider::Claude,
        profile_id: "candidate-reviewer".into(),
        model: None,
    }
}

async fn review_candidate(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
    artifact: &WorkflowArtifact,
    assignment: &PlannedAssignment,
) -> Result<WorkflowArtifact> {
    let ArtifactValue::Candidate(candidate) = &artifact.value else {
        anyhow::bail!("selection artifact has no candidate");
    };
    let mut review = submission(
        &fixture.checkout,
        &fixture.run,
        "review-candidate",
        Some(unit.plan_revision.clone()),
        SessionMode::ReadOnly,
    );
    review.scope.provider = assignment.provider;
    review.scope.profile_id = assignment.profile_id.clone();
    review.scope.model = assignment.model.clone();
    review.scope.checkout = worktree.root.clone();
    review.scope.executable = Path::new(&fixture.checkout.repository_root).join("codex.exe");
    review.request.checkout_id = Some(worktree.checkout_id.clone());
    review.request.task_id = Some(unit.task_id.clone());
    review.unit_id = Some(unit.unit.id.clone());
    review.artifacts = NodeInputs::from([("candidate".into(), artifact.id.clone())]);
    let review = store.submit_workflow_agent(review).await?;
    let reviewer = claim_fixture_job(store, review, fixture_admission()).await?;
    seal_review(store, candidate, &reviewer).await?;
    if let Some(model) = &assignment.model {
        store
            .record_agent_observation(
                reviewer.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: Default::default(),
                    data: ProviderEventData::ModelObserved {
                        model: Some(format!("observed-{model}")),
                        effort: None,
                    },
                },
            )
            .await?;
    }
    store
        .finish_agent_attempt(reviewer.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    store
        .publish_workflow_artifact(
            fixture.project.clone(),
            fixture.run.id.clone(),
            "review-candidate".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: reviewer.run.id,
            },
        )
        .await
}

async fn verify_candidate(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    candidate: &WorkflowArtifact,
) -> Result<WorkflowArtifact> {
    let WorkflowUnitOutcome::Artifact { artifact: checks } = store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "verify-candidate",
            NodeInputs::from([("candidate".into(), candidate.id.clone())]),
            WorkflowUnitRequest::VerifyCandidate,
        ))
        .await?
    else {
        anyhow::bail!("missing candidate checks");
    };
    Ok(checks)
}

async fn accept_candidate(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    candidate: &WorkflowArtifact,
    review: &WorkflowArtifact,
    checks: &WorkflowArtifact,
) -> Result<WorkflowArtifact> {
    let WorkflowUnitOutcome::Artifact {
        artifact: acceptance,
    } = store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "accept",
            NodeInputs::from([
                ("candidate".into(), candidate.id.clone()),
                ("review".into(), review.id.clone()),
                ("checks".into(), checks.id.clone()),
            ]),
            WorkflowUnitRequest::AcceptCandidate {
                completion_criteria: unit.unit.completion_criteria.clone(),
            },
        ))
        .await?
    else {
        anyhow::bail!("missing candidate acceptance");
    };
    Ok(acceptance)
}

fn fixture_admission() -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    }
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

async fn claim_fixture_job(
    store: &Store,
    job: Job,
    environment: AdmissionEnvironment,
) -> Result<AttemptPlan> {
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
