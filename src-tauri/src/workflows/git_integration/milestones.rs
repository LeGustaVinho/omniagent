use super::*;
use crate::{
    repository::verified_merge::{DeliveryMergeOutcome, DeliveryMergeRequest, MergeSource},
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
};

#[tokio::test]
async fn automatic_merge_completes_its_subtask_and_rolls_up_to_ancestors() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = Store::open(temporary.path().join("delivery.sqlite"))?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let project = &repo.project_id;
    let destination = "integration";
    git(Path::new(&repo.checkout_root), &["branch", destination])?;
    store
        .set_delivery_milestone_policy(
            project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedMerge,
                destination_branch: Some(destination.into()),
            },
        )
        .await?;
    let epic = store
        .create_project_epic(project.clone(), "Verify a Sub-Task".into())
        .await?;
    let parent = store
        .create_project_task_in_epic(
            project.clone(),
            epic.id.clone(),
            "Integrate scoped work".into(),
        )
        .await?;
    let child = store
        .create_project_subtask(
            project.clone(),
            parent.id.clone(),
            "Complete the implementation".into(),
        )
        .await?;
    let fixture = approve_delivery_fixture_for_work(
        &store,
        temporary.path(),
        repo.clone(),
        Some(child.id.clone()),
    )
    .await?;
    let [unit] = fixture.units.as_slice() else {
        anyhow::bail!("expected the approved Sub-Task unit")
    };
    let worktree = store
        .live_managed_worktree(project.clone(), child.id.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;
    let accepted = capture_and_review(&store, &fixture, &worktree, unit, "first.txt").await?;
    let receipt = verified(
        &store,
        &fixture,
        unit,
        commit(&store, &fixture, unit, &accepted, None).await?,
    )
    .await?;
    let pending = store
        .project_task_delivery_state(project.clone(), child.id.clone())
        .await?
        .context("missing Sub-Task")?;
    assert!(pending.waiting_for_merge);
    assert!(
        store
            .project_task_delivery_state(project.clone(), parent.id.clone())
            .await?
            .context("missing parent")?
            .completion
            .is_none()
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Backlog)
    );

    store
        .set_delivery_git_policy(
            project.clone(),
            DeliveryGitPolicy::AutomaticLocalCommitAndMerge,
        )
        .await?;
    let preview = store
        .delivery_merge_preview(
            project.clone(),
            child.id.clone(),
            fixture.run.id.clone(),
            unit.unit.id.clone(),
            receipt.id.clone(),
        )
        .await?;
    let outcome = store
        .verify_delivery_merge(DeliveryMergeRequest {
            project_id: project.clone(),
            task_id: child.id.clone(),
            workflow_run_id: fixture.run.id.clone(),
            unit_id: unit.unit.id.clone(),
            commit_evidence_id: receipt.id.clone(),
            expected_source_branch: preview.source_branch,
            expected_source_sha: preview.source_sha,
            destination_branch: preview.destination_branch,
            expected_destination_sha: preview.expected_destination_sha,
            external_sha: None,
            drift_acceptance_id: None,
            prepare_drift: false,
            perform: true,
        })
        .await?;
    let DeliveryMergeOutcome::Verified { evidence } = &outcome else {
        anyhow::bail!("automatic merge did not settle: {outcome:?}")
    };
    assert_eq!(evidence.source, MergeSource::Automatic);
    assert_eq!(evidence.source_sha, receipt.sha);
    assert_eq!(evidence.destination_branch, destination);
    assert_eq!(
        git(Path::new(&repo.checkout_root), &["rev-parse", destination])?,
        evidence.integration_sha
    );
    let child_state = store
        .project_task_delivery_state(project.clone(), child.id)
        .await?
        .context("missing Sub-Task")?;
    assert_eq!(child_state.state, TaskWorkflowState::Completed);
    assert_eq!(
        child_state
            .completion
            .and_then(|receipt| receipt.merge)
            .as_ref(),
        Some(evidence)
    );
    let parent_state = store
        .project_task_delivery_state(project.clone(), parent.id)
        .await?
        .context("missing parent")?;
    assert_eq!(parent_state.state, TaskWorkflowState::Completed);
    assert!(parent_state.completion.is_none());
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id)
            .await?,
        Some(TaskWorkflowState::Completed)
    );
    Ok(())
}

#[tokio::test]
async fn verified_merge_waits_for_a_complete_chain_and_reopens_with_historical_evidence()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("delivery.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let root = Path::new(&repo.checkout_root);
    let project = &repo.project_id;
    let destination = git(root, &["branch", "--show-current"])?;
    let base = git(root, &["rev-parse", "HEAD"])?;
    assert_eq!(
        store.delivery_milestone_policy(project.clone()).await?,
        DeliveryMilestonePolicy::default()
    );
    assert!(
        store
            .set_delivery_milestone_policy(
                project.clone(),
                DeliveryMilestonePolicy {
                    milestone: DeliveryCompletionMilestone::VerifiedMerge,
                    destination_branch: None,
                }
            )
            .await
            .is_err()
    );
    let policy = DeliveryMilestonePolicy {
        milestone: DeliveryCompletionMilestone::VerifiedMerge,
        destination_branch: Some(destination.clone()),
    };
    assert_eq!(
        store
            .set_delivery_milestone_policy(
                project.clone(),
                DeliveryMilestonePolicy {
                    destination_branch: Some(format!("refs/heads/{destination}")),
                    ..policy.clone()
                }
            )
            .await?,
        policy
    );
    let fixture =
        approve_delivery_fixture_for_work(&store, temporary.path(), repo.clone(), None).await?;
    let [first, second] = fixture.units.as_slice() else {
        anyhow::bail!("expected two planned units")
    };
    let task = &first.task_id;
    let epic = store
        .create_project_epic(project.clone(), "Deliver the milestone".into())
        .await?;
    store
        .set_project_task_epic(project.clone(), task.clone(), Some(epic.id.clone()))
        .await?;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;

    let accepted = capture_and_review(&store, &fixture, &worktree, first, "first.txt").await?;
    let first_receipt = verified(
        &store,
        &fixture,
        first,
        commit(&store, &fixture, first, &accepted, None).await?,
    )
    .await?;
    let partial = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("missing task")?;
    assert_ne!(partial.state, TaskWorkflowState::Completed);
    assert!(!partial.waiting_for_merge);
    assert!(partial.pending_merge.is_none());

    store
        .set_delivery_git_policy(project.clone(), DeliveryGitPolicy::UserCommitsAndMerges)
        .await?;
    let accepted = capture_and_review(&store, &fixture, &worktree, second, "second.txt").await?;
    assert_eq!(
        commit(&store, &fixture, second, &accepted, None).await?,
        WorkflowUnitOutcome::WaitingForCommit
    );
    let waiting = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("missing task")?;
    assert!(waiting.waiting_for_commit);
    assert!(!waiting.waiting_for_merge);
    let sha = user_commit(&worktree.root, "second.txt", "Commit the complete delivery")?;
    let commit_outcome = commit(&store, &fixture, second, &accepted, Some(sha.clone())).await?;
    let final_receipt = verified(&store, &fixture, second, commit_outcome.clone()).await?;
    let WorkflowUnitOutcome::Artifact { artifact } = commit_outcome else {
        anyhow::bail!("commit node did not publish its evidence")
    };
    let result = || {
        unit_request(
            &fixture,
            second,
            "result",
            NodeInputs::from([
                ("task".into(), fixture.task.id.clone()),
                ("git".into(), artifact.id.clone()),
            ]),
            WorkflowUnitRequest::Result,
        )
    };
    let waiting = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("missing task")?;
    assert_eq!(waiting.state, TaskWorkflowState::InReview);
    assert!(waiting.waiting_for_merge);
    assert!(!waiting.waiting_for_commit);
    let candidate = waiting
        .pending_merge
        .as_ref()
        .context("missing final merge candidate")?;
    assert_eq!(
        candidate.commit_evidence_ids,
        [first_receipt.id.clone(), final_receipt.id.clone()]
    );
    assert_eq!(candidate.source_sha, sha);
    assert_eq!(candidate.destination_branch.as_ref(), Some(&destination));
    assert!(waiting.completion.is_none());
    assert!(
        store
            .project_task_deliveries(project.clone(), task.clone())
            .await?
            .is_empty()
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Backlog)
    );

    drop(store);
    let store = Store::open(database.clone())?;
    assert_eq!(
        store.delivery_milestone_policy(project.clone()).await?,
        policy
    );
    assert_eq!(
        store
            .project_task_delivery_state(project.clone(), task.clone())
            .await?,
        Some(waiting)
    );
    store
        .set_delivery_milestone_policy(
            project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedCommit,
                destination_branch: Some("next-integration".into()),
            },
        )
        .await?;
    assert_eq!(
        store.execute_workflow_unit(result()).await?,
        WorkflowUnitOutcome::WaitingForMerge
    );
    let preview = store
        .delivery_merge_preview(
            project.clone(),
            task.clone(),
            fixture.run.id.clone(),
            second.unit.id.clone(),
            final_receipt.id.clone(),
        )
        .await?;
    assert_eq!(preview.destination_branch, destination);
    assert_eq!(preview.expected_destination_sha, base);
    let mut request = DeliveryMergeRequest {
        project_id: project.clone(),
        task_id: task.clone(),
        workflow_run_id: fixture.run.id.clone(),
        unit_id: second.unit.id.clone(),
        commit_evidence_id: final_receipt.id.clone(),
        expected_source_branch: preview.source_branch,
        expected_source_sha: preview.source_sha,
        destination_branch: preview.destination_branch,
        expected_destination_sha: preview.expected_destination_sha,
        external_sha: None,
        drift_acceptance_id: None,
        prepare_drift: false,
        perform: false,
    };
    assert_eq!(
        store.verify_delivery_merge(request.clone()).await?,
        DeliveryMergeOutcome::WaitingForMerge
    );
    git(root, &["merge", "--ff-only", &sha])?;
    request.external_sha = Some(sha.clone());
    let saved = store.verify_delivery_merge(request.clone()).await?;
    let DeliveryMergeOutcome::Verified { evidence } = &saved else {
        anyhow::bail!("external merge did not settle: {saved:?}")
    };
    assert_eq!(evidence.source, MergeSource::External);
    assert_eq!(evidence.source_sha, sha);
    assert_eq!(evidence.destination_branch, destination);
    assert_eq!(evidence.previous_destination_sha, base);
    assert_eq!(evidence.integration_sha, sha);
    assert_eq!(store.verify_delivery_merge(request.clone()).await?, saved);
    assert!(matches!(
        store.execute_workflow_unit(result()).await?,
        WorkflowUnitOutcome::Artifact { .. }
    ));
    let complete = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("missing task")?;
    assert_eq!(complete.state, TaskWorkflowState::Completed);
    assert!(!complete.waiting_for_merge);
    let receipt = complete.completion.context("missing delivery receipt")?;
    assert_eq!(receipt.merge.as_ref(), Some(evidence));
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Completed)
    );

    store
        .reopen_task_delivery(project.clone(), task.clone())
        .await?;
    git(
        &worktree.root,
        &["branch", "-m", "renamed-after-integration"],
    )?;
    git(&worktree.root, &["switch", "--detach", &sha])?;
    git(root, &["branch", "-D", "renamed-after-integration"])?;
    git(root, &["branch", "-m", "renamed-destination"])?;
    drop(store);
    let store = Store::open(database)?;
    let reopened = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("missing task")?;
    assert_ne!(reopened.state, TaskWorkflowState::Completed);
    assert!(reopened.pending_merge.is_none());
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id)
            .await?,
        Some(TaskWorkflowState::Backlog)
    );
    assert_eq!(
        store
            .project_task_deliveries(project.clone(), task.clone())
            .await?,
        [receipt]
    );
    assert_eq!(store.verify_delivery_merge(request).await?, saved);
    Ok(())
}
