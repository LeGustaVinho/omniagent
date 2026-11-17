use crate::{
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
    workflows::{ManualWorkflowLaunch, WorkflowLaunchSource},
};

use super::*;

const CHECK_NAME: &str = "Check the materialized Task delivery";

#[tokio::test]
async fn published_task_delivery_preserves_order_and_requires_its_verified_merge() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("task-delivery.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let source = Path::new(&repo.checkout_root);
    let destination = git(source, &["branch", "--show-current"])?;
    let base = git(source, &["rev-parse", "HEAD"])?;
    store
        .set_delivery_milestone_policy(
            project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedMerge,
                destination_branch: Some(destination.clone()),
            },
        )
        .await?;
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![CandidateGate {
                name: CHECK_NAME.into(),
                required: false,
                action: CandidateGateAction::Command {
                    executable: repository::git_path()?,
                    arguments: ["grep", "-q", "^first[.]txt$", "HEAD", "--", "first.txt"]
                        .map(str::to_owned)
                        .to_vec(),
                    timeout_seconds: Some(20),
                },
            }],
        )
        .await?;

    let draft = factory_draft(&store, project, WorkflowFactoryTemplate::TaskDelivery).await?;
    let original_revision = draft.revision.clone();
    let mut package = draft.package;
    let PackageArtifactBody::Pipeline { graph } = &mut package.members[0].body else {
        anyhow::bail!("The Task delivery factory must supply an editable pipeline");
    };
    let gate = NodeConfiguration::RequiredChecks {
        names: vec![CHECK_NAME.into()],
    };
    graph.name = "Task delivery with explicit checks".into();
    graph
        .nodes
        .iter_mut()
        .find(|node| node.kind == BuiltinNode::VerifyCandidate)
        .context("The factory lost its candidate verification node")?
        .configuration = gate.clone();
    let saved = store
        .save_workflow_package(project.clone(), draft.id, draft.revision, package)
        .await?;
    assert_ne!(saved.revision, original_revision);
    let published = publish_factory_member(&store, project, saved, "task-delivery").await?;
    let version = WorkflowRevision {
        id: published.id,
        revision: published.revision,
    };
    let brief = store
        .capture_repository_markdown(repo.checkout_id.clone(), source.join("brief.md"))
        .await?;
    let launch = ManualWorkflowLaunch {
        project_id: project.clone(),
        workflow: version.clone(),
        source: WorkflowLaunchSource {
            checkout_id: repo.checkout_id.clone(),
            revision_id: brief.revision.id,
        },
        request_id: "factory-task-delivery".into(),
        work_id: None,
    };
    let run = store.launch_workflow_manual(launch.clone()).await?;
    assert_eq!(store.launch_workflow_manual(launch).await?, run);
    assert_eq!(run.version, version);
    let definition = store
        .workflow_definition(project.clone(), version.clone())
        .await?
        .context("Published Task delivery definition missing")?;
    assert_eq!(definition.name, "Task delivery with explicit checks");
    assert!(
        definition
            .nodes
            .iter()
            .any(|node| node.instance.kind == BuiltinNode::VerifyCandidate
                && node.instance.configuration == gate)
    );

    let fixture =
        approve_delivery_fixture_in_run(&store, temp.path(), repo.clone(), None, run).await?;
    let [first, second] = fixture.units.as_slice() else {
        anyhow::bail!("The Task delivery factory must use the two approved logical units");
    };
    let task = &first.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("The approved Task delivery has no managed checkout")?
        .worktree;
    assert!(
        queue_implementation(&store, &fixture, &worktree, second)
            .await
            .is_err()
    );
    let accepted = capture_and_review(&store, &fixture, &worktree, first, "first.txt").await?;
    let first_outcome = commit(&store, &fixture, first, &accepted, None).await?;
    let first_commit = verified(&store, &fixture, first, first_outcome.clone()).await?;
    let WorkflowUnitOutcome::Artifact {
        artifact: first_git,
    } = first_outcome
    else {
        anyhow::bail!("The first unit must publish its verified commit");
    };
    let result_for = |unit, git: &WorkflowArtifact| {
        unit_request(
            &fixture,
            unit,
            "result",
            NodeInputs::from([
                ("task".into(), fixture.task.id.clone()),
                ("git".into(), git.id.clone()),
            ]),
            WorkflowUnitRequest::Result,
        )
    };
    assert!(
        store
            .execute_workflow_unit(result_for(first, &first_git))
            .await
            .is_err()
    );
    let partial = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("Missing partially delivered Task")?;
    assert!(!partial.waiting_for_merge && partial.completion.is_none());
    assert_ne!(partial.state, TaskWorkflowState::Completed);
    assert_eq!(first_commit.base_commit, base);

    let accepted = capture_and_review(&store, &fixture, &worktree, second, "second.txt").await?;
    let final_outcome = commit(&store, &fixture, second, &accepted, None).await?;
    let last_commit = verified(&store, &fixture, second, final_outcome.clone()).await?;
    let WorkflowUnitOutcome::Artifact {
        artifact: final_git,
    } = final_outcome
    else {
        anyhow::bail!("The last unit must publish the verified source for Merge");
    };
    assert_eq!(last_commit.base_commit, first_commit.sha);
    assert_ne!(first_commit.verification_id, last_commit.verification_id);
    for receipt in [&first_commit, &last_commit] {
        let check = store
            .candidate_verification(
                project.clone(),
                task.clone(),
                receipt.verification_id.clone(),
            )
            .await?
            .context("The pinned check evidence is missing")?;
        assert_eq!(check.candidate_id, receipt.candidate_revision);
        assert!(check.gates.iter().any(|gate| gate.gate.name == CHECK_NAME
            && gate.gate.required
            && matches!(&gate.outcome, Some(CandidateGateOutcome::Passed { .. }))));
    }
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD^"])?,
        first_commit.sha
    );
    assert_eq!(git(source, &["rev-parse", "HEAD"])?, base);
    assert_eq!(
        store
            .execute_workflow_unit(result_for(second, &final_git))
            .await?,
        WorkflowUnitOutcome::WaitingForMerge
    );
    let pending = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("Missing final integration state")?;
    assert!(pending.waiting_for_merge && pending.completion.is_none());
    let ordered = [first_commit.id, last_commit.id.clone()];
    assert_eq!(
        pending
            .pending_merge
            .context("No pending Merge evidence")?
            .commit_evidence_ids,
        ordered
    );
    let preview = store
        .delivery_merge_preview(
            project.clone(),
            task.clone(),
            fixture.run.id.clone(),
            second.unit.id.clone(),
            last_commit.id.clone(),
        )
        .await?;
    assert_eq!(preview.expected_destination_sha, base);
    assert_eq!(preview.destination_branch, destination);
    git(source, &["merge", "--ff-only", &last_commit.sha])?;
    let merged = store
        .execute_workflow_unit(unit_request(
            &fixture,
            second,
            "merge",
            NodeInputs::from([("git".into(), final_git.id.clone())]),
            WorkflowUnitRequest::VerifyMerge {
                expected_source_branch: preview.source_branch,
                expected_source_sha: preview.source_sha,
                destination_branch: preview.destination_branch,
                expected_destination_sha: preview.expected_destination_sha,
                external_sha: Some(last_commit.sha.clone()),
                drift_acceptance_id: None,
                prepare_drift: false,
                perform: false,
            },
        ))
        .await?;
    assert!(matches!(merged, WorkflowUnitOutcome::Artifact { artifact }
        if artifact.node_instance_id == "merge" && matches!(&artifact.value, ArtifactValue::Merge(evidence)
            if evidence.commit_evidence_id == last_commit.id)));
    assert!(
        matches!(store.execute_workflow_unit(result_for(second, &final_git)).await?,
        WorkflowUnitOutcome::Artifact { artifact } if artifact.node_instance_id == "result")
    );
    let completed = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("Missing completed Task")?;
    assert_eq!(completed.state, TaskWorkflowState::Completed);
    let receipt = completed.completion.context("Missing completion receipt")?;
    assert!(
        matches!(&receipt.evidence, TaskDeliveryEvidence::VerifiedCommit {
        workflow_run_id, unit_evidence_ids, completion_sha, ..
    } if workflow_run_id == &fixture.run.id && unit_evidence_ids == &ordered && completion_sha == &last_commit.sha)
    );
    let merge = receipt.merge.context("Missing pinned merge receipt")?;
    assert_eq!(merge.commit_evidence_ids, ordered);
    assert_eq!(git(source, &["rev-parse", "HEAD"])?, merge.integration_sha);
    Ok(())
}
