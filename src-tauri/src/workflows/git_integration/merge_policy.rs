use crate::{
    repository::verified_merge::{DeliveryMergePreview, MergeSource},
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
    workflows::{ArtifactKind, DeliveryRoles, builtin_delivery_graph},
};

use super::*;

#[test]
fn persisted_git_stage_titles_maintain_exact_execution_contracts() {
    use crate::workflows::registry::supports_pinned_execution;
    use crate::workflows::{BuiltinNode, builtin_registry};

    let registry = builtin_registry();
    for (kind, original, published) in [
        (BuiltinNode::VerifyCommit, "Verify commit", "Commit"),
        (BuiltinNode::VerifyMerge, "Verify merge", "Merge"),
    ] {
        let mut pinned = registry
            .iter()
            .find(|definition| definition.kind == kind)
            .unwrap()
            .clone();
        assert_eq!(pinned.title, original);
        assert!(supports_pinned_execution(&pinned));
        pinned.title = published.into();
        assert!(supports_pinned_execution(&pinned));
        pinned.inputs.clear();
        assert!(!supports_pinned_execution(&pinned));
    }
}

#[tokio::test]
async fn automatic_delivery_merges_only_the_complete_chain_and_reuses_its_pinned_result()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("automatic-delivery.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    let (root, worktree, destination) = prepare_destination(&store, &fixture).await?;
    assert_eq!(
        store.delivery_git_policy(fixture.project.clone()).await?,
        DeliveryGitPolicy::AutomaticCommitUserMerge
    );
    store
        .set_delivery_git_policy(
            fixture.project.clone(),
            DeliveryGitPolicy::AutomaticLocalCommitAndMerge,
        )
        .await?;

    let [first, last] = fixture.units.as_slice() else {
        anyhow::bail!("expected two delivery units");
    };
    let accepted_first =
        capture_and_review(&store, &fixture, &worktree, first, "first.txt").await?;
    let WorkflowUnitOutcome::Artifact {
        artifact: first_commit,
    } = commit(&store, &fixture, first, &accepted_first, None).await?
    else {
        anyhow::bail!("the first logical unit must commit without merging the delivery");
    };
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/integration"])?,
        destination
    );
    let partial_preview = DeliveryMergePreview {
        source_branch: worktree.branch.clone(),
        source_sha: git(&worktree.root, &["rev-parse", "HEAD"])?,
        destination_branch: "integration".into(),
        expected_destination_sha: destination.clone(),
    };
    assert!(
        merge(
            &store,
            &fixture,
            first,
            &first_commit,
            &partial_preview,
            true,
            None
        )
        .await
        .is_err()
    );

    let accepted_last = capture_and_review(&store, &fixture, &worktree, last, "second.txt").await?;
    let outcome = commit(&store, &fixture, last, &accepted_last, None).await?;
    let WorkflowUnitOutcome::CommitWithMerge {
        artifact: final_commit,
        merge: integrated,
    } = &outcome
    else {
        anyhow::bail!("the final logical unit did not trigger automatic integration: {outcome:?}");
    };
    let WorkflowUnitOutcome::Artifact {
        artifact: integration,
    } = integrated.as_ref()
    else {
        anyhow::bail!("automatic integration is unavailable: {integrated:?}");
    };
    let ArtifactValue::Merge(reference) = &integration.value else {
        anyhow::bail!("expected typed integration evidence");
    };
    let ArtifactValue::Git(source) = &final_commit.value else {
        anyhow::bail!("expected typed source commit evidence");
    };
    let receipt = store
        .verified_delivery_merge(
            fixture.project.clone(),
            last.task_id.clone(),
            source.id.clone(),
        )
        .await?
        .context("verified integration receipt is unavailable")?;
    assert_eq!(receipt.id, reference.id);
    assert_eq!(receipt.source, MergeSource::Automatic);
    assert_eq!(receipt.commit_evidence_id, source.id);
    assert_eq!(receipt.previous_destination_sha, destination);
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/integration"])?,
        receipt.integration_sha
    );
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        receipt.source_sha
    );
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, destination);
    assert_eq!(receipt.commit_evidence_ids.len(), 2);

    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(
        reopened
            .resolve_workflow_artifact(
                fixture.project.clone(),
                fixture.run.id.clone(),
                integration.id.clone(),
                ArtifactKind::Merge
            )
            .await?,
        Some(integration.clone())
    );
    assert!(
        reopened
            .resolve_workflow_artifact(
                fixture.project.clone(),
                fixture.run.id.clone(),
                integration.id.clone(),
                ArtifactKind::Git
            )
            .await?
            .is_none()
    );
    assert_eq!(
        commit(&reopened, &fixture, last, &accepted_last, None).await?,
        outcome
    );
    Ok(())
}

#[tokio::test]
async fn default_commit_waits_for_an_explicit_local_merge_of_the_expected_destination() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("operator-delivery.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    let (root, worktree, destination) = prepare_destination(&store, &fixture).await?;
    let [first, last] = fixture.units.as_slice() else {
        anyhow::bail!("expected two delivery units");
    };
    let first_request =
        production_acceptance(&store, &fixture, &worktree, first, "first.txt").await?;
    store
        .set_delivery_git_policy(
            fixture.project.clone(),
            DeliveryGitPolicy::UserCommitsAndMerges,
        )
        .await?;
    let original = git(&worktree.root, &["rev-parse", "HEAD"])?;
    let manual = store
        .execute_workflow_unit(unit_request(
            &fixture,
            first,
            "accept",
            first_request.clone(),
            WorkflowUnitRequest::AcceptDiff {
                completion_criteria: first.unit.completion_criteria.clone(),
            },
        ))
        .await?;
    let WorkflowUnitOutcome::AcceptanceWithCommit {
        artifact: accepted,
        commit: waiting,
    } = manual
    else {
        anyhow::bail!("the manual policy must preserve acceptance and wait");
    };
    assert_eq!(*waiting, WorkflowUnitOutcome::WaitingForCommit);
    assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, original);
    store
        .set_delivery_git_policy(
            fixture.project.clone(),
            DeliveryGitPolicy::AutomaticCommitUserMerge,
        )
        .await?;
    let (first_acceptance, first_commit) =
        execute_production_acceptance(&store, &fixture, first, first_request).await?;
    assert_eq!(accepted.id, first_acceptance.id);
    assert_eq!(first_commit.node_instance_id, "commit");
    let last_request =
        production_acceptance(&store, &fixture, &worktree, last, "second.txt").await?;
    let (_, commit) = execute_production_acceptance(&store, &fixture, last, last_request).await?;
    assert_eq!(commit.node_instance_id, "commit");
    let ArtifactValue::Git(source) = &commit.value else {
        anyhow::bail!("expected verified commit");
    };
    let preview = store
        .delivery_merge_preview(
            fixture.project.clone(),
            last.task_id.clone(),
            fixture.run.id.clone(),
            last.unit.id.clone(),
            source.id.clone(),
        )
        .await?;
    assert_eq!(preview.expected_destination_sha, destination);
    let mut wrong_source = preview.clone();
    wrong_source.source_sha = destination.clone();
    assert!(
        merge(&store, &fixture, last, &commit, &wrong_source, true, None)
            .await
            .is_err()
    );
    assert_eq!(
        merge(&store, &fixture, last, &commit, &preview, false, None).await?,
        WorkflowUnitOutcome::WaitingForMerge
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/integration"])?,
        destination
    );
    let completed = merge(&store, &fixture, last, &commit, &preview, true, None).await?;
    let WorkflowUnitOutcome::Artifact {
        artifact: integration,
    } = completed
    else {
        anyhow::bail!("the operator merge must publish its integration artifact");
    };
    assert_eq!(integration.node_instance_id, "merge");
    let ArtifactValue::Merge(reference) = &integration.value else {
        anyhow::bail!("the integration port must publish a typed merge reference");
    };
    assert_eq!(reference.commit_evidence_id, source.id);
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/integration"])?,
        preview.source_sha
    );
    drop(store);
    let reopened = Store::open(database)?;
    let definition = reopened
        .workflow_definition(fixture.project.clone(), fixture.run.version.clone())
        .await?
        .context("the delivery definition must remain pinned")?;
    assert!(
        definition
            .nodes
            .iter()
            .any(|node| node.instance.id == commit.node_instance_id
                && node.definition.title == "Verify commit")
    );
    assert!(
        definition
            .nodes
            .iter()
            .any(|node| node.instance.id == integration.node_instance_id
                && node.definition.title == "Verify merge")
    );
    let history = reopened
        .project_task_deliveries(fixture.project.clone(), last.task_id.clone())
        .await?;
    let merged = reopened
        .verified_delivery_merge(
            fixture.project.clone(),
            last.task_id.clone(),
            source.id.clone(),
        )
        .await?
        .context("the saved merge receipt is unavailable")?;
    assert_eq!(merged.id, reference.id);
    assert_eq!(merged.source_sha, preview.source_sha);
    assert_eq!(merged.source_branch, preview.source_branch);
    assert_eq!(merged.destination_branch, preview.destination_branch);
    assert!(history.iter().any(|delivery| {
        delivery
            .merge
            .as_ref()
            .is_some_and(|receipt| receipt.id == merged.id)
    }));
    Ok(())
}

async fn production_acceptance(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
    path: &str,
) -> Result<NodeInputs> {
    let job = queue_implementation(store, fixture, worktree, unit).await?;
    let target = capture_implementation(store, fixture, worktree, unit, &[path], job).await?;
    let candidate = select_candidate(store, fixture, target, &[path]).await?;
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
    Ok(NodeInputs::from([
        ("candidate".into(), candidate.id),
        ("review".into(), review.id),
        ("checks".into(), checks.id),
    ]))
}

async fn execute_production_acceptance(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    inputs: NodeInputs,
) -> Result<(WorkflowArtifact, WorkflowArtifact)> {
    let outcome = store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "accept",
            inputs,
            WorkflowUnitRequest::AcceptDiff {
                completion_criteria: unit.unit.completion_criteria.clone(),
            },
        ))
        .await?;
    let WorkflowUnitOutcome::AcceptanceWithCommit {
        artifact: acceptance,
        commit,
    } = outcome
    else {
        anyhow::bail!("acceptance did not invoke the pinned Commit node");
    };
    let WorkflowUnitOutcome::Artifact { artifact } = *commit else {
        anyhow::bail!("the default policy did not publish the commit: {commit:?}");
    };
    Ok((acceptance, artifact))
}

#[tokio::test]
async fn manual_policy_pins_the_destination_before_an_external_merge_or_preview_open() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("external-delivery.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.nodes.retain(|node| node.id != "merge");
    graph
        .wires
        .retain(|wire| wire.from.node != "merge" && wire.to.node != "merge");
    let revision = store
        .save_workflow_revision(
            repo.project_id.clone(),
            super::super::builtin::BUILTIN_DELIVERY_ID.into(),
            graph,
        )
        .await?;
    let run = store
        .start_workflow_run(repo.project_id.clone(), revision)
        .await?;
    let fixture =
        super::super::tests::approve_delivery_fixture_in_run(&store, temp.path(), repo, None, run)
            .await?;
    let (root, worktree, destination) = prepare_destination(&store, &fixture).await?;
    store
        .set_delivery_git_policy(
            fixture.project.clone(),
            DeliveryGitPolicy::UserCommitsAndMerges,
        )
        .await?;

    let mut final_result = None;
    for (unit, path) in fixture.units.iter().zip(["first.txt", "second.txt"]) {
        let accepted = capture_and_review(&store, &fixture, &worktree, unit, path).await?;
        assert_eq!(
            commit(&store, &fixture, unit, &accepted, None).await?,
            WorkflowUnitOutcome::WaitingForCommit
        );
        let sha = user_commit(&worktree.root, path, &unit.unit.purpose)?;
        let WorkflowUnitOutcome::Artifact { artifact } =
            commit(&store, &fixture, unit, &accepted, Some(sha.clone())).await?
        else {
            anyhow::bail!("external commit was not verified");
        };
        final_result = Some((unit, artifact, sha));
    }
    let (unit, artifact, source_sha) =
        final_result.context("final verified commit is unavailable")?;
    let ArtifactValue::Git(commit) = &artifact.value else {
        anyhow::bail!("missing commit evidence");
    };

    // The operator can integrate immediately after receiving the last commit.
    // The first later preview must still refer to the pre-integration tip.
    git(
        root,
        &[
            "update-ref",
            "refs/heads/integration",
            &source_sha,
            &destination,
        ],
    )?;
    let preview = store
        .delivery_merge_preview(
            fixture.project.clone(),
            unit.task_id.clone(),
            fixture.run.id.clone(),
            unit.unit.id.clone(),
            commit.id.clone(),
        )
        .await?;
    assert_eq!(preview.expected_destination_sha, destination);
    let outcome = merge(
        &store,
        &fixture,
        unit,
        &artifact,
        &preview,
        false,
        Some(source_sha.clone()),
    )
    .await?;
    let WorkflowUnitOutcome::MergeVerified { evidence } = outcome else {
        anyhow::bail!("legacy delivery must return standalone integration evidence");
    };
    let receipt = store
        .verified_delivery_merge(
            fixture.project.clone(),
            unit.task_id.clone(),
            commit.id.clone(),
        )
        .await?
        .context("external merge receipt is unavailable")?;
    assert_eq!(receipt.previous_destination_sha, destination);
    assert_eq!(receipt.integration_sha, source_sha);
    assert_eq!(receipt.source, MergeSource::External);
    assert_eq!(receipt, evidence);
    assert!(
        !store
            .workflow_run_artifacts(fixture.project.clone(), fixture.run.id.clone())
            .await?
            .iter()
            .any(|artifact| artifact.node_instance_id == "merge")
    );
    Ok(())
}

async fn prepare_destination<'a>(
    store: &Store,
    fixture: &'a ApprovedDeliveryFixture,
) -> Result<(&'a Path, ManagedWorktree, String)> {
    let root = Path::new(&fixture.checkout.repository_root);
    let destination = git(root, &["rev-parse", "HEAD"])?;
    git(root, &["branch", "integration", &destination])?;
    store
        .set_delivery_milestone_policy(
            fixture.project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedCommit,
                destination_branch: Some("integration".into()),
            },
        )
        .await?;
    let worktree = store
        .live_managed_worktree(fixture.project.clone(), fixture.units[0].task_id.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;
    Ok((root, worktree, destination))
}

async fn merge(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    commit: &WorkflowArtifact,
    preview: &DeliveryMergePreview,
    perform: bool,
    external_sha: Option<String>,
) -> Result<WorkflowUnitOutcome> {
    store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "merge",
            NodeInputs::from([("git".into(), commit.id.clone())]),
            WorkflowUnitRequest::VerifyMerge {
                expected_source_branch: preview.source_branch.clone(),
                expected_source_sha: preview.source_sha.clone(),
                destination_branch: preview.destination_branch.clone(),
                expected_destination_sha: preview.expected_destination_sha.clone(),
                external_sha,
                drift_acceptance_id: None,
                prepare_drift: false,
                perform,
            },
        ))
        .await
}
