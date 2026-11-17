use crate::workflows::{
    BuiltinNode, Endpoint, NodeConfiguration, NodeInstance, Wire, WorkflowRevision,
    authoring::package::{
        PackageArtifactBody, PackageArtifactVersion, PackageReference, WorkflowPackageDraft,
        WorkflowPackagePublishOutcome,
    },
    templates::{WorkflowFactoryTemplate, workflow_factory_templates},
    tests::approve_delivery_fixture_in_run,
};

use super::*;

mod diff;
mod repair;
mod task;

const REVIEW: &str = "review-test::review";
const CHECK: &str = "review-test::check";
const JOIN: &str = "review-test::join";

#[tokio::test]
async fn factory_review_and_test_runs_parallel_on_one_candidate_and_forwards_its_evidence()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("factory.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id.clone();
    let library = factory_draft(&store, &project, WorkflowFactoryTemplate::DeliveryLibrary).await?;
    let subflow = publish_factory_member(&store, &project, library, "review-and-test").await?;
    let draft = factory_draft(&store, &project, WorkflowFactoryTemplate::TaskDelivery).await?;
    let mut package = draft.package;
    let root = &mut package.members[0];
    let PackageArtifactBody::Pipeline { graph } = &mut root.body else {
        unreachable!()
    };
    let replaced = ["review-candidate", "verify-candidate"];
    graph
        .nodes
        .retain(|node| !replaced.contains(&node.id.as_str()));
    graph.wires.retain(|wire| {
        !replaced.contains(&wire.from.node.as_str()) && !replaced.contains(&wire.to.node.as_str())
    });
    graph.nodes.push(NodeInstance {
        id: "review-test".into(),
        kind: BuiltinNode::Subflow,
        configuration: NodeConfiguration::None,
    });
    graph.wires.extend([
        link("implement", "implemented", "review-test", "ready"),
        link("implement", "candidate", "review-test", "candidate"),
        link("review-test", "complete", "accept", "ready"),
        link("review-test", "review", "accept", "review"),
        link("review-test", "checks", "accept", "checks"),
    ]);
    root.references.insert(
        "review-test".into(),
        PackageReference::Published {
            version: subflow.clone(),
        },
    );
    let draft = store
        .save_workflow_package(project.clone(), draft.id, draft.revision, package)
        .await?;
    let version = publish_factory_member(&store, &project, draft, "task-delivery").await?;
    let run = store
        .start_workflow_run(
            project.clone(),
            WorkflowRevision {
                id: version.id,
                revision: version.revision,
            },
        )
        .await?;
    let fixture = approve_delivery_fixture_in_run(&store, temp.path(), repo, None, run).await?;
    let unit = &fixture.units[0];
    let task = &unit.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("Missing logical unit worktree")?
        .worktree;
    let git_program =
        std::env::split_paths(&std::env::var_os("PATH").context("Git for Windows needs PATH")?)
            .filter(|path| path.is_absolute())
            .map(|path| path.join("git.exe"))
            .find(|path| path.is_file())
            .context("Git for Windows unavailable")?;
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![CandidateGate {
                name: "Check committed file content".into(),
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

    let writer = queue_implementation(&store, &fixture, &worktree, unit).await?;
    let target =
        capture_implementation(&store, &fixture, &worktree, unit, &["first.txt"], writer).await?;
    let candidate_artifact = select_candidate(&store, &fixture, target, &["first.txt"]).await?;
    let ArtifactValue::Candidate(candidate) = &candidate_artifact.value else {
        unreachable!()
    };
    let mut request = submission(
        &fixture.checkout,
        &fixture.run,
        REVIEW,
        Some(unit.plan_revision.clone()),
        SessionMode::ReadOnly,
    );
    let assignment = first_reviewer();
    request.scope.provider = assignment.provider;
    request.scope.profile_id = assignment.profile_id;
    request.scope.checkout = worktree.root.clone();
    request.scope.executable = Path::new(&fixture.checkout.repository_root).join("codex.exe");
    request.request.checkout_id = Some(worktree.checkout_id.clone());
    request.request.task_id = Some(task.clone());
    request.unit_id = Some(unit.unit.id.clone());
    request.artifacts = NodeInputs::from([("candidate".into(), candidate_artifact.id.clone())]);
    let job = store.submit_workflow_agent(request).await?;
    let reviewer = claim_fixture_job(&store, job, fixture_admission()).await?;
    seal_review(&store, candidate, &reviewer).await?;
    let WorkflowUnitOutcome::Artifact { artifact: checks } = store
        .execute_workflow_unit(unit_request(
            &fixture,
            unit,
            CHECK,
            NodeInputs::from([("candidate".into(), candidate_artifact.id.clone())]),
            WorkflowUnitRequest::VerifyCandidate,
        ))
        .await?
    else {
        anyhow::bail!("Missing parallel candidate check");
    };
    let ArtifactValue::Checks(reference) = &checks.value else {
        unreachable!()
    };
    let verification = store
        .candidate_verification(project.clone(), task.clone(), reference.id.clone())
        .await?
        .context("Missing real check receipt")?;
    assert_eq!(verification.candidate_id, candidate.id);
    assert!(verification.allows_acceptance());
    let waiting = store
        .inspect_workflow_execution(project.clone(), fixture.run.id.clone())
        .await?
        .context("Missing scoped run")?;
    assert!(
        !waiting
            .control
            .iter()
            .any(|receipt| receipt.node_id == JOIN)
    );

    let review_run = reviewer.run.id;
    store
        .finish_agent_attempt(review_run.clone(), RunStatus::Succeeded, None)
        .await?;
    let review = store
        .publish_workflow_artifact(
            project.clone(),
            fixture.run.id.clone(),
            REVIEW.into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: review_run.clone(),
            },
        )
        .await?;
    let accepted = accept_candidate(
        &store,
        &fixture,
        unit,
        &candidate_artifact,
        &review,
        &checks,
    )
    .await?;
    let execution = store
        .inspect_workflow_execution(project.clone(), fixture.run.id.clone())
        .await?
        .context("Missing completed branches")?;
    let child = execution
        .definition
        .subflows
        .iter()
        .find(|scope| scope.path == "review-test")
        .context("Missing pinned review subflow")?;
    assert_eq!(child.version, subflow);
    assert_eq!(child.outputs["review"].node, REVIEW);
    assert_eq!(child.outputs["checks"].node, CHECK);
    assert!(
        execution
            .control
            .iter()
            .any(|receipt| receipt.node_id == JOIN && receipt.selected_ports == ["joined"])
    );
    assert!(execution.jobs.iter().any(|job| {
        job.node_id == REVIEW
            && job
                .attempts
                .iter()
                .any(|attempt| attempt.run_id == review_run)
    }));
    let receipt = execution
        .outputs
        .iter()
        .find(|output| output.artifact.id == accepted.id)
        .context("Missing acceptance evidence")?;
    assert!(receipt.input_receipts.iter().any(|input| input.inputs["review"] == review.id && input.inputs["checks"] == checks.id));
    let accepted = AcceptedUnit {
        candidate: candidate_artifact,
        acceptance: accepted,
    };
    let outcome = commit(&store, &fixture, unit, &accepted, None).await?;
    let committed = verified(&store, &fixture, unit, outcome).await?;
    assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, committed.sha);
    Ok(())
}

async fn factory_draft(
    store: &Store,
    project: &str,
    template: WorkflowFactoryTemplate,
) -> Result<WorkflowPackageDraft> {
    let entry = workflow_factory_templates()
        .into_iter()
        .find(|entry| entry.id == template)
        .context("Missing factory template")?;
    store
        .create_workflow_factory_draft(project.into(), entry.id, entry.revision)
        .await
}

async fn publish_factory_member(
    store: &Store,
    project: &str,
    draft: WorkflowPackageDraft,
    key: &str,
) -> Result<PackageArtifactVersion> {
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("Factory package needs explicit publication");
    };
    publication
        .members
        .into_iter()
        .find(|member| member.key == key)
        .map(|member| member.artifact.version)
        .context("Missing published template member")
}

fn link(from: &str, output: &str, to: &str, input: &str) -> Wire {
    Wire {
        from: Endpoint {
            node: from.into(),
            port: output.into(),
        },
        to: Endpoint {
            node: to.into(),
            port: input.into(),
        },
    }
}
