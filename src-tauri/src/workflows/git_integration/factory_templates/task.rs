use crate::{
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
    workflows::{ManualWorkflowLaunch, WorkflowLaunchSource},
};

use super::*;

const CHECK_NAME: &str = "Check the materialized Task delivery";

#[tokio::test]
async fn grouped_candidate_checks_overlap_but_keep_policy_receipts_in_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("parallel-checks.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let task = store
        .create_project_task(project.clone(), "Verify parallel checks".into())
        .await?;
    store.set_job_queue_capacity(2).await?;
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let marker = |name: &str| temp.path().join(name).to_string_lossy().replace('\'', "''");
    let (first, second, complete) = (marker("first"), marker("second"), marker("complete"));
    let identity =
        "if ((Get-Content -LiteralPath 'first.txt' -Raw).Trim() -ne 'first.txt') { exit 13 }";
    let await_marker = |path: &str| {
        format!(
            "$end = (Get-Date).AddSeconds(10); while (!(Test-Path -LiteralPath '{path}')) {{ if ((Get-Date) -gt $end) {{ exit 14 }}; Start-Sleep -Milliseconds 30 }}"
        )
    };
    let gate = |name: &str, script: String| CandidateGate {
        name: name.into(),
        required: false,
        action: CandidateGateAction::Command {
            executable: powershell.clone(),
            arguments: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
            timeout_seconds: Some(20),
        },
    };
    store.set_candidate_gate_policy(project.clone(), vec![
        gate("tail", format!("{identity}; if (!(Test-Path -LiteralPath '{complete}')) {{ exit 15 }}; exit 0")),
        gate("first", format!(
            "{identity}; New-Item -Path 'isolation.txt' -ItemType File | Out-Null; New-Item -Path '{first}' -ItemType File | Out-Null; {}; New-Item -Path '{complete}' -ItemType File | Out-Null; exit 0",
            await_marker(&second)
        )),
        gate("second", format!(
            "{identity}; New-Item -Path '{second}' -ItemType File | Out-Null; {}; if (Test-Path -LiteralPath 'isolation.txt') {{ exit 16 }}; exit 0",
            await_marker(&first)
        )),
    ]).await?;

    let mut graph = crate::workflows::builtin_delivery_graph(Default::default());
    let check_node = graph
        .nodes
        .iter_mut()
        .find(|node| node.kind == BuiltinNode::VerifyCandidate)
        .context("The graph has no candidate verification node")?;
    check_node.configuration = NodeConfiguration::RequiredChecks {
        names: vec![],
        parallel_groups: vec![
            vec!["first".into(), "tail".into()],
            vec!["second".into(), "missing".into()],
        ],
    };
    let version = store
        .save_workflow_revision(project.clone(), "parallel-checks".into(), graph)
        .await?;
    let run = store.start_workflow_run(project.clone(), version).await?;
    let fixture = approve_delivery_fixture_in_run(
        &store,
        temp.path(),
        repo.clone(),
        Some(task.id.clone()),
        run,
    )
    .await?;
    let [unit] = fixture.units.as_slice() else {
        anyhow::bail!("Missing single logical unit")
    };
    let worktree = store
        .live_managed_worktree(project.clone(), task.id.clone())
        .await?
        .context("The task has no managed worktree")?
        .worktree;
    let job = queue_implementation(&store, &fixture, &worktree, unit).await?;
    let capture =
        capture_implementation(&store, &fixture, &worktree, unit, &["first.txt"], job).await?;
    let candidate = select_candidate(&store, &fixture, capture, &["first.txt"]).await?;
    // The live checkout changes after publishing the candidate. All gates must still see the frozen tree.
    fs::write(worktree.root.join("first.txt"), "outside the selected tree")?;
    let checks = verify_candidate(&store, &fixture, unit, &candidate).await?;
    let ArtifactValue::Checks(reference) = checks.value else {
        anyhow::bail!("Missing check receipt")
    };
    drop(store);
    let store = Store::open(database)?;
    let verified = store
        .candidate_verification(project.clone(), task.id.clone(), reference.id)
        .await?
        .context("The group receipts were lost after reopen")?;
    assert_eq!(
        verified
            .gates
            .iter()
            .map(|gate| gate.gate.name.as_str())
            .collect::<Vec<_>>(),
        ["tail", "first", "second", "missing"],
    );
    assert!(
        verified.gates[..3].iter().all(|gate| gate.gate.required
            && matches!(&gate.outcome, Some(CandidateGateOutcome::Passed { .. }))),
        "{:?}",
        verified.gates
    );
    assert!(verified.gates[3].gate.required);
    assert!(
        matches!(&verified.gates[3].outcome, Some(CandidateGateOutcome::CouldNotRun { reason })
        if reason.contains("missing"))
    );
    assert!(verified.finished_at.is_some());
    assert!(!verified.allows_acceptance());
    assert!(!worktree.root.join("isolation.txt").exists());

    let build = marker("build");
    let mut quality = gate("quality", format!("{identity}; exit 0"));
    quality.required = true;
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![
                quality,
                gate(
                    "build",
                    format!(
                        "{identity}; New-Item -Path '{build}' -ItemType File | Out-Null; exit 0"
                    ),
                ),
            ],
        )
        .await?;
    let ArtifactValue::Candidate(selection) = &candidate.value else {
        anyhow::bail!("The selected candidate evidence is unavailable");
    };
    let partial = store
        .verify_selected_candidate_with_stage(
            project.clone(),
            selection,
            vec!["quality".into()],
            vec![],
            vec!["build".into()],
            false,
        )
        .await?;
    assert!(partial.passes_stage_checks());
    assert!(!partial.allows_acceptance());
    assert!(partial.gates[1].deferred && partial.gates[1].outcome.is_none());
    assert!(!temp.path().join("build").exists());
    assert!(
        store
            .accept_candidate_verification(
                project.clone(),
                task.id.clone(),
                selection.id.clone(),
                partial.id,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("complete verification")
    );
    let full = store
        .verify_selected_candidate_with_requirements(
            project.clone(),
            task.id.clone(),
            selection.id.clone(),
            vec!["build".into()],
        )
        .await?;
    assert!(full.allows_acceptance());
    assert!(temp.path().join("build").is_file());
    store
        .accept_candidate_verification(project.clone(), task.id, selection.id.clone(), full.id)
        .await?;
    Ok(())
}

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
        parallel_groups: Vec::new(),
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
