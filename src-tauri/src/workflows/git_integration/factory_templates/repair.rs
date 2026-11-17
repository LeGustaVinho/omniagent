use super::*;

#[tokio::test]
async fn factory_repair_checks_each_candidate_under_its_release_and_stops_or_reports_the_limit()
-> Result<()> {
    exercise(true).await?;
    exercise(false).await
}

async fn exercise(pass_second: bool) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("repair.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id.clone();
    let draft = factory_draft(&store, &project, WorkflowFactoryTemplate::FixFailedChecks).await?;
    let draft = if pass_second {
        draft
    } else {
        let mut package = draft.package;
        let PackageArtifactBody::Pipeline { graph } = &mut package.members[0].body else {
            unreachable!()
        };
        let repair = graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "repair")
            .context("Missing repair loop")?;
        let NodeConfiguration::RepeatUntil(configuration) = &mut repair.configuration else {
            unreachable!()
        };
        configuration.max_iterations = 2;
        store
            .save_workflow_package(project.clone(), draft.id, draft.revision, package)
            .await?
    };
    let version = publish_factory_member(&store, &project, draft, "fix-failed-checks").await?;
    let run = store
        .start_workflow_run(
            project.clone(),
            WorkflowRevision {
                id: version.id,
                revision: version.revision,
            },
        )
        .await?;
    let fixture = approve_delivery_fixture_in_run(&store, dir.path(), repo, None, run).await?;
    let unit = &fixture.units[0];
    let task = &unit.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("Missing scoped repair checkout")?
        .worktree;
    let git_program = std::env::split_paths(&std::env::var_os("PATH").context("Git needs PATH")?)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("git.exe"))
        .find(|path| path.is_file())
        .context("Git for Windows unavailable")?;
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![CandidateGate {
                name: "Check whether the repair fixed the file".into(),
                required: true,
                action: CandidateGateAction::Command {
                    executable: git_program,
                    arguments: ["grep", "-q", "repair complete", "HEAD", "--", "first.txt"]
                        .map(str::to_owned)
                        .to_vec(),
                    timeout_seconds: Some(20),
                },
            }],
        )
        .await?;
    let request = |iteration| -> Result<WorkflowAgentSubmission> {
        let mut request = implementation_submission(&fixture, &worktree, unit)?;
        request.dispatch.node_instance_id = format!("repair::iteration-{iteration}::implement");
        Ok(request)
    };
    let mut unapproved = request(1)?;
    unapproved.dispatch.release_id = None;
    assert!(store.submit_workflow_agent(unapproved).await.is_err());
    assert!(store.submit_workflow_agent(request(2)?).await.is_err());
    let ArtifactValue::Release(release) = &fixture.release.value else {
        unreachable!()
    };
    let mut previous = None;
    for iteration in 1..=2 {
        let passed = pass_second && iteration == 2;
        let input = request(iteration)?;
        let writer_node = input.dispatch.node_instance_id.clone();
        let job = store.submit_workflow_agent(input).await?;
        assert_eq!(
            job.pipeline
                .as_ref()
                .and_then(|origin| origin.release.as_ref())
                .map(|pin| &pin.id),
            Some(&release.release.id)
        );
        let target =
            capture_implementation_changes(&store, &fixture, &worktree, unit, job, |root| {
                fs::write(
                    root.join("first.txt"),
                    if passed {
                        "repair complete\n".into()
                    } else {
                        format!("repair fails at iteration {iteration}\n")
                    },
                )?;
                Ok(())
            })
            .await?;
        let selected = store
            .select_worktree_candidate(
                target,
                CandidateSelection {
                    tracked_paths: Some(vec!["first.txt".into()]),
                    untracked_paths: vec![],
                },
            )
            .await?;
        let candidate = store
            .publish_workflow_candidate(
                project.clone(),
                fixture.run.id.clone(),
                writer_node,
                task.clone(),
                selected.id.clone(),
            )
            .await?;
        let check = format!("repair::iteration-{iteration}::verify");
        let verify = |artifact: String| {
            unit_request(
                &fixture,
                unit,
                &check,
                NodeInputs::from([("candidate".into(), artifact)]),
                WorkflowUnitRequest::VerifyCandidate,
            )
        };
        if let Some((prior_artifact, prior_selection)) = previous {
            assert_ne!(prior_selection, selected.id);
            assert!(
                store
                    .execute_workflow_unit(verify(prior_artifact))
                    .await
                    .is_err()
            );
        }
        let WorkflowUnitOutcome::Artifact { artifact } = store
            .execute_workflow_unit(verify(candidate.id.clone()))
            .await?
        else {
            anyhow::bail!("Missing repair verification evidence");
        };
        let ArtifactValue::Checks(reference) = artifact.value else {
            unreachable!()
        };
        let observed = store
            .candidate_verification(project.clone(), task.clone(), reference.id)
            .await?
            .context("Missing checks for the exact candidate")?;
        assert_eq!(observed.candidate_id, selected.id);
        assert_eq!(observed.allows_acceptance(), passed);
        require_signal(
            &store,
            &fixture,
            unit,
            &format!("repair::condition-{iteration}"),
            if passed { "matched" } else { "unmatched" },
        )
        .await?;
        previous = Some((candidate.id, selected.id));
    }
    assert!(store.submit_workflow_agent(request(3)?).await.is_err());
    let terminal = if pass_second {
        ("repair::stop", "joined")
    } else {
        ("repair::limit", "failed")
    };
    require_signal(&store, &fixture, unit, terminal.0, terminal.1).await?;
    let execution = store
        .inspect_workflow_execution(project, fixture.run.id.clone())
        .await?
        .context("Missing final workflow state")?;
    assert_eq!(
        execution
            .jobs
            .iter()
            .filter(|job| job.node_id.ends_with("::implement"))
            .count(),
        2
    );
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        worktree.base_commit
    );
    Ok(())
}

async fn require_signal(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    node: &str,
    port: &str,
) -> Result<()> {
    let execution = store
        .inspect_workflow_execution(fixture.project.clone(), fixture.run.id.clone())
        .await?
        .context("Missing repair run evidence")?;
    assert!(
        execution
            .control
            .iter()
            .any(|receipt| receipt.node_id == node
                && receipt.unit_id.as_deref() == Some(unit.unit.id.as_str())
                && receipt.selected_ports == [port]),
        "{node} did not select {port}"
    );
    Ok(())
}
