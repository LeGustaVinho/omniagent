use crate::{
    repository::verification::CandidateAcceptance,
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
    workflows::{
        BuiltinNode, CommandConfiguration, CommandOutputFormat, CommandRegistrationRequest,
        CommandTarget, DeliveryRoles, Endpoint, NodeConfiguration, NodeInstance, Wire,
        WorkflowCommandSubmission, WorkflowGraph, builtin_delivery_graph,
        tests::{approve_reviewed_plan_fixture, link_reviewed_plan_fixture},
    },
};

use super::*;

const PLAN: &str = r#"Correct the amount converter and document its use in two milestones.

```omniagent-plan
{"units":[{"purpose":"Convert amounts to cents and verify integer and fractional amounts","completionCriteria":["The converter and its regression tests agree on cents"]},{"purpose":"Publish a working example and matching usage documentation","completionCriteria":["The example and the documentation show the same 125-cent output"]}]}
```"#;
const CONVERTER: &str = "Convert-Amount.ps1";
const TESTS: &str = "Convert-Amount.Tests.ps1";
const USAGE: &str = "usage.md";
const EXAMPLE: &str = "Convert-Amount.Example.ps1";
const OPTIONAL_REASON: &str = "No optional package builder is registered for this project";

const FIXED_CONVERTER: &str =
    "function Convert-AmountToCents ([decimal]$Amount) { [int]($Amount * 100) }\n";
const FIXED_TESTS: &str = r#". (Join-Path $PSScriptRoot 'Convert-Amount.ps1')
if ((Convert-AmountToCents 3) -ne 300 -or (Convert-AmountToCents 1.23) -ne 123) { exit 17 }
$example = Get-Content -Raw (Join-Path $PSScriptRoot 'Convert-Amount.Example.ps1')
$usage = Get-Content -Raw (Join-Path $PSScriptRoot 'usage.md')
if (($example -match '1\.25') -ne ($usage -match '1\.25')) { exit 23 }
exit 0
"#;
const UPDATED_USAGE: &str = "Run `Convert-Amount.Example.ps1` to convert 1.25 into 125 cents.\n";
const UPDATED_EXAMPLE: &str =
    ". (Join-Path $PSScriptRoot 'Convert-Amount.ps1')\nConvert-AmountToCents 1.25\n";

#[tokio::test]
async fn two_milestones_require_independent_checks_acceptance_and_verified_integration()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("delivery-acceptance.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let source = Path::new(&repo.repository_root).to_owned();
    for (path, text) in [
        (
            "brief.md",
            "Correct the amount converter to return cents. Then update the example and documentation together.\n",
        ),
        (
            CONVERTER,
            "function Convert-AmountToCents ([decimal]$Amount) { [int]($Amount * 10) }\n",
        ),
        (
            TESTS,
            ". (Join-Path $PSScriptRoot 'Convert-Amount.ps1')\nif ((Convert-AmountToCents 3) -ne 30) { exit 17 }\nexit 0\n",
        ),
        (
            USAGE,
            "Run the existing example to convert a whole amount.\n",
        ),
        (
            EXAMPLE,
            ". (Join-Path $PSScriptRoot 'Convert-Amount.ps1')\nConvert-AmountToCents 1\n",
        ),
    ] {
        fs::write(source.join(path), text)?;
    }
    git(
        &source,
        &["add", "--", "brief.md", CONVERTER, TESTS, USAGE, EXAMPLE],
    )?;
    let base = commit_staged(&source, "Baseline converter and example")?;
    git(&source, &["branch", "integration", &base])?;
    let project = repo.project_id.clone();
    store
        .set_delivery_milestone_policy(
            project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedMerge,
                destination_branch: Some("integration".into()),
            },
        )
        .await?;
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("Windows root is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let policy_revision = register_gates(&store, &project, &powershell).await?;
    let graph = graph_with_writing_command(&store, &project, &powershell).await?;
    let version = store
        .save_workflow_revision(project.clone(), "amount-converter-delivery".into(), graph)
        .await?;
    let run = store.start_workflow_run(project.clone(), version).await?;
    let linked = link_reviewed_plan_fixture(
        &store,
        &repo,
        None,
        &run,
        PLAN,
        "Correct and document the amount converter",
    )
    .await?;
    let fixture =
        approve_reviewed_plan_fixture(&store, temporary.path(), repo.clone(), run, linked).await?;
    let [first, last] = fixture.units.as_slice() else {
        anyhow::bail!("the reviewed plan must materialize two logical units");
    };
    assert_eq!(
        first.unit.purpose,
        "Convert amounts to cents and verify integer and fractional amounts"
    );
    assert_eq!(
        last.unit.purpose,
        "Publish a working example and matching usage documentation"
    );
    let task = &first.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("approval did not provision a managed worktree")?
        .worktree;
    verify_unreleased_writers(&store, &fixture, &repo, &worktree, task).await?;
    assert_eq!(
        store.delivery_git_policy(project.clone()).await?,
        DeliveryGitPolicy::AutomaticCommitUserMerge
    );

    let first_capture = capture_milestone(
        &store,
        &fixture,
        &worktree,
        first,
        &[(CONVERTER, FIXED_CONVERTER), (TESTS, FIXED_TESTS)],
    )
    .await?;
    let incomplete =
        select_candidate(&store, &fixture, first_capture.clone(), &[CONVERTER]).await?;
    let incomplete_review = review_candidate(
        &store,
        &fixture,
        &worktree,
        first,
        &incomplete,
        &first_reviewer(),
    )
    .await?;
    let failed = verify_candidate(&store, &fixture, first, &incomplete).await?;
    let failed_check = checked_evidence(&store, &project, task, &failed).await?;
    assert_eq!(failed_check.policy_revision, policy_revision);
    assert!(matches!(
        &failed_check.gates[0].outcome,
        Some(CandidateGateOutcome::Failed { exit_code: 17, .. })
    ));
    assert!(!failed_check.allows_acceptance());
    assert!(
        accept_diff(
            &store,
            &fixture,
            first,
            &incomplete,
            &incomplete_review,
            &failed
        )
        .await
        .err()
        .context("a failed required check permitted acceptance")?
        .to_string()
        .contains("required checks")
    );
    assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, base);
    assert!(
        store
            .project_task_delivery_state(project.clone(), task.clone())
            .await?
            .context("Task is unavailable")?
            .completion
            .is_none()
    );

    let selected = select_candidate(&store, &fixture, first_capture, &[CONVERTER, TESTS]).await?;
    let review = review_candidate(
        &store,
        &fixture,
        &worktree,
        first,
        &selected,
        &first_reviewer(),
    )
    .await?;
    assert!(
        accept_diff(&store, &fixture, first, &selected, &review, &failed)
            .await
            .is_err()
    );
    let checks = verify_candidate(&store, &fixture, first, &selected).await?;
    let (first_accepted, first_outcome) =
        accept_diff(&store, &fixture, first, &selected, &review, &checks).await?;
    let first_acceptance = recorded_acceptance(
        &store,
        &fixture,
        first,
        &first_accepted,
        &checks,
        &policy_revision,
    )
    .await?;
    let first_commit = verified(&store, &fixture, first, first_outcome.clone()).await?;
    assert_eq!(first_commit.source, CommitSource::Automatic);
    assert_eq!(first_commit.acceptance_id, first_acceptance.id);
    assert_eq!(
        first_commit.verification_id,
        first_acceptance.verification.id
    );
    assert_eq!(first_commit.tree, first_acceptance.candidate.tree);
    assert_commit_paths(&worktree.root, &first_commit.sha, &[CONVERTER, TESTS])?;
    assert_eq!(
        git(&source, &["rev-parse", "refs/heads/integration"])?,
        base
    );
    assert_ne!(
        store
            .project_task_delivery_state(project.clone(), task.clone())
            .await?
            .context("Task is unavailable")?
            .state,
        TaskWorkflowState::Completed
    );
    drop(store);

    let store = Store::open(database.clone())?;
    assert_eq!(
        commit(&store, &fixture, first, &first_accepted, None).await?,
        first_outcome
    );
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        first_commit.sha
    );
    assert_eq!(
        git(
            &worktree.root,
            &["rev-list", "--count", &format!("{base}..HEAD")]
        )?,
        "1"
    );
    store
        .set_delivery_git_policy(project.clone(), DeliveryGitPolicy::UserCommitsAndMerges)
        .await?;

    let last_capture = capture_milestone(
        &store,
        &fixture,
        &worktree,
        last,
        &[(USAGE, UPDATED_USAGE), (EXAMPLE, UPDATED_EXAMPLE)],
    )
    .await?;
    let selected = select_candidate(&store, &fixture, last_capture, &[USAGE, EXAMPLE]).await?;
    let review = review_candidate(
        &store,
        &fixture,
        &worktree,
        last,
        &selected,
        &first_reviewer(),
    )
    .await?;
    let checks = verify_candidate(&store, &fixture, last, &selected).await?;
    let (last_accepted, waiting) =
        accept_diff(&store, &fixture, last, &selected, &review, &checks).await?;
    assert_eq!(waiting, WorkflowUnitOutcome::WaitingForCommit);
    assert!(
        store
            .project_task_delivery_state(project.clone(), task.clone())
            .await?
            .context("manual delivery Task is unavailable")?
            .waiting_for_commit
    );
    let last_acceptance = recorded_acceptance(
        &store,
        &fixture,
        last,
        &last_accepted,
        &checks,
        &policy_revision,
    )
    .await?;
    assert_ne!(last_acceptance.id, first_acceptance.id);
    assert_ne!(
        last_acceptance.verification.id,
        first_acceptance.verification.id
    );
    assert_eq!(last_acceptance.candidate.base_commit, first_commit.sha);
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        first_commit.sha
    );

    git(&worktree.root, &["add", "--", USAGE, EXAMPLE])?;
    let tree = git(&worktree.root, &["write-tree"])?;
    assert_eq!(tree, last_acceptance.candidate.tree);
    let foreign = git(
        &worktree.root,
        &[
            "-c",
            "user.name=Acceptance fixture",
            "-c",
            "user.email=fixture@omniagent.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit-tree",
            &tree,
            "-p",
            &first_commit.sha,
            "-m",
            "Exact tree on a different ref",
        ],
    )?;
    git(&worktree.root, &["branch", "wrong-delivery", &foreign])?;
    assert!(
        commit(&store, &fixture, last, &last_accepted, Some(foreign))
            .await
            .unwrap_err()
            .to_string()
            .contains("branch")
    );
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        first_commit.sha
    );
    let external = commit_staged(&worktree.root, "Document and demonstrate cents conversion")?;
    let final_outcome = commit(
        &store,
        &fixture,
        last,
        &last_accepted,
        Some(external.clone()),
    )
    .await?;
    let last_commit = verified(&store, &fixture, last, final_outcome.clone()).await?;
    assert_eq!(last_commit.source, CommitSource::External);
    assert_eq!(last_commit.sha, external);
    assert_eq!(last_commit.acceptance_id, last_acceptance.id);
    assert_eq!(last_commit.verification_id, last_acceptance.verification.id);
    assert_eq!(last_commit.base_commit, first_commit.sha);
    assert_eq!(last_commit.branch, worktree.branch);
    assert_commit_paths(&worktree.root, &last_commit.sha, &[USAGE, EXAMPLE])?;

    let WorkflowUnitOutcome::Artifact {
        artifact: final_git,
    } = &final_outcome
    else {
        anyhow::bail!("missing final workflow commit");
    };
    let result = || {
        unit_request(
            &fixture,
            last,
            "result",
            NodeInputs::from([
                ("task".into(), fixture.task.id.clone()),
                ("git".into(), final_git.id.clone()),
            ]),
            WorkflowUnitRequest::Result,
        )
    };
    let waiting = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("Task is unavailable")?;
    assert!(waiting.waiting_for_merge);
    assert!(waiting.completion.is_none());
    assert_eq!(
        store.execute_workflow_unit(result()).await?,
        WorkflowUnitOutcome::WaitingForMerge
    );
    let preview = store
        .delivery_merge_preview(
            project.clone(),
            task.clone(),
            fixture.run.id.clone(),
            last.unit.id.clone(),
            last_commit.id.clone(),
        )
        .await?;
    assert_eq!(preview.expected_destination_sha, base);
    git(
        &source,
        &["update-ref", "refs/heads/integration", &external, &base],
    )?;
    let merge = || {
        unit_request(
            &fixture,
            last,
            "merge",
            NodeInputs::from([("git".into(), final_git.id.clone())]),
            WorkflowUnitRequest::VerifyMerge {
                expected_source_branch: preview.source_branch.clone(),
                expected_source_sha: preview.source_sha.clone(),
                destination_branch: preview.destination_branch.clone(),
                expected_destination_sha: preview.expected_destination_sha.clone(),
                external_sha: Some(external.clone()),
                drift_acceptance_id: None,
                prepare_drift: false,
                perform: false,
            },
        )
    };
    let integrated = store.execute_workflow_unit(merge()).await?;
    let WorkflowUnitOutcome::Artifact {
        artifact: integration,
    } = &integrated
    else {
        anyhow::bail!("expected verified integration evidence");
    };
    assert!(
        matches!(&integration.value, ArtifactValue::Merge(reference) if reference.commit_evidence_id == last_commit.id)
    );
    let finalized = store.execute_workflow_unit(result()).await?;
    let done = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("Task is unavailable")?;
    assert_eq!(done.state, TaskWorkflowState::Completed);
    let completion = done
        .completion
        .as_ref()
        .context("Task completion has no evidence")?;
    assert!(
        matches!(&completion.evidence, TaskDeliveryEvidence::VerifiedCommit { unit_evidence_ids, completion_sha, .. }
        if unit_evidence_ids == &[first_commit.id.clone(), last_commit.id.clone()] && completion_sha == &external)
    );

    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(
        commit(
            &reopened,
            &fixture,
            last,
            &last_accepted,
            Some(external.clone())
        )
        .await?,
        final_outcome
    );
    assert_eq!(reopened.execute_workflow_unit(merge()).await?, integrated);
    assert_eq!(reopened.execute_workflow_unit(result()).await?, finalized);
    assert_eq!(
        reopened
            .project_task_delivery_state(project.clone(), task.clone())
            .await?,
        Some(done)
    );
    for record in [first_acceptance, last_acceptance] {
        assert_eq!(
            reopened
                .candidate_acceptance(project.clone(), task.clone(), record.id.clone())
                .await?,
            Some(record)
        );
    }
    assert_eq!(
        git(&source, &["rev-parse", "refs/heads/integration"])?,
        external
    );
    assert_eq!(
        git(
            &worktree.root,
            &["rev-list", "--count", &format!("{base}..HEAD")]
        )?,
        "2"
    );
    assert_eq!(git(&source, &["rev-parse", "HEAD"])?, base);
    Ok(())
}

async fn register_gates(store: &Store, project: &str, powershell: &Path) -> Result<String> {
    store
        .set_candidate_gate_policy(
            project.into(),
            vec![
                CandidateGate {
                    name: "Converter and example tests".into(),
                    required: true,
                    action: CandidateGateAction::Command {
                        executable: powershell.to_path_buf(),
                        arguments: vec![
                            "-NoProfile".into(),
                            "-NonInteractive".into(),
                            "-ExecutionPolicy".into(),
                            "Bypass".into(),
                            "-File".into(),
                            TESTS.into(),
                        ],
                        timeout_seconds: Some(30),
                    },
                },
                CandidateGate {
                    name: "Optional package build".into(),
                    required: false,
                    action: CandidateGateAction::Unavailable {
                        reason: OPTIONAL_REASON.into(),
                    },
                },
            ],
        )
        .await
}

async fn graph_with_writing_command(
    store: &Store,
    project: &str,
    powershell: &Path,
) -> Result<WorkflowGraph> {
    let command = store
        .register_workflow_command(
            project.into(),
            CommandRegistrationRequest {
                name: "Repository-writing release probe".into(),
                target: CommandTarget::Executable {
                    path: powershell.to_path_buf(),
                },
            },
        )
        .await?;
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.nodes.push(NodeInstance {
        id: "writing-command".into(),
        kind: BuiltinNode::Command,
        configuration: NodeConfiguration::Command(CommandConfiguration {
            registration_id: command.id,
            registration_revision: command.revision,
            arguments: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                "Set-Content -LiteralPath 'command-wrote.txt' -Value forbidden".into(),
            ],
            input: serde_json::from_value(json!({}))?,
            output: CommandOutputFormat::Text,
        }),
    });
    for (port, target) in [
        ("prepared", "ready"),
        ("release", "release"),
        ("worktree", "worktree"),
    ] {
        graph.wires.push(Wire {
            from: Endpoint {
                node: "prepare".into(),
                port: port.into(),
            },
            to: Endpoint {
                node: "writing-command".into(),
                port: target.into(),
            },
        });
    }
    Ok(graph)
}

async fn verify_unreleased_writers(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    repo: &ProjectCheckout,
    worktree: &ManagedWorktree,
    task: &str,
) -> Result<()> {
    let missing = store
        .submit_workflow_command(WorkflowCommandSubmission {
            project_id: fixture.project.clone(),
            run_id: fixture.run.id.clone(),
            node_id: "writing-command".into(),
            inputs: NodeInputs::from([("worktree".into(), fixture.worktree.id.clone())]),
        })
        .await
        .unwrap_err();
    assert!(
        missing
            .to_string()
            .contains("workflow input release needs an artifact")
    );
    let unscoped = store
        .submit_workflow_command(WorkflowCommandSubmission {
            project_id: fixture.project.clone(),
            run_id: fixture.run.id.clone(),
            node_id: "writing-command".into(),
            inputs: NodeInputs::from([
                ("worktree".into(), fixture.worktree.id.clone()),
                ("release".into(), fixture.decision.id.clone()),
            ]),
        })
        .await
        .unwrap_err();
    assert!(
        unscoped
            .to_string()
            .contains("artifact for workflow input release is unavailable")
    );
    assert!(!worktree.root.join("command-wrote.txt").exists());
    store
        .set_project_job_rules(
            fixture.project.clone(),
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
    let mut review = submission(
        repo,
        &fixture.run,
        "review-plan",
        Some(fixture.plan.id.clone()),
        SessionMode::ReadOnly,
    );
    review.scope.provider = Provider::Claude;
    review.scope.profile_id = "unreleased-review".into();
    review.request.requires_bridge = true;
    review.artifacts = NodeInputs::from([("plan".into(), fixture.plan.id.clone())]);
    let job = store.submit_workflow_agent(review).await?;
    let env = AdmissionEnvironment {
        bridge_available: true,
        ..fixture_admission()
    };
    let reviewer = claim_fixture_job(store, job, env.clone()).await?;
    store.mark_agent_running(reviewer.run.id.clone()).await?;
    let origin = store
        .attempt_pipeline_origin(fixture.project.clone(), reviewer.run.id.clone())
        .await?
        .context("review lost its workflow scope")?;
    assert!(origin.release.is_none());
    let bridge = RunBridge::start(store, reviewer.run.id.clone()).await?;
    let refused = invoke_test_mcp_tool(&bridge, "submit_job", json!({
        "requestId": "milestone-bypass", "request": {"mode": "approved_write", "instructions": "Try writing without the upstream release", "checkoutId": worktree.checkout_id, "taskId": task }
    })).await?;
    assert!(refused["error"].is_object(), "{refused}");
    let run = reviewer.run.id.clone();
    let created = store.execute(move |db| Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE origin_run_id = ?1 AND request_id = 'milestone-bypass')",
        [run], |row| row.get::<_, bool>(0),
    )?)).await?;
    assert!(!created);
    store
        .finish_agent_attempt(reviewer.run.id, RunStatus::Succeeded, None)
        .await?;
    assert!(store.claim_next_job(env).await?.is_none());
    assert!(!worktree.root.join("command-wrote.txt").exists());
    Ok(())
}

async fn capture_milestone(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    worktree: &ManagedWorktree,
    unit: &WorkflowDeliveryUnit,
    files: &[(&str, &str)],
) -> Result<CandidateTarget> {
    let writer = queue_implementation(store, fixture, worktree, unit).await?;
    capture_implementation_changes(store, fixture, worktree, unit, writer, |root| {
        for (path, text) in files {
            fs::write(root.join(path), text)?;
        }
        Ok(())
    })
    .await
}

async fn accept_diff(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    candidate: &WorkflowArtifact,
    review: &WorkflowArtifact,
    checks: &WorkflowArtifact,
) -> Result<(AcceptedUnit, WorkflowUnitOutcome)> {
    let WorkflowUnitOutcome::AcceptanceWithCommit { artifact, commit } = store
        .execute_workflow_unit(unit_request(
            fixture,
            unit,
            "accept",
            NodeInputs::from([
                ("candidate".into(), candidate.id.clone()),
                ("review".into(), review.id.clone()),
                ("checks".into(), checks.id.clone()),
            ]),
            WorkflowUnitRequest::AcceptDiff {
                completion_criteria: unit.unit.completion_criteria.clone(),
            },
        ))
        .await?
    else {
        anyhow::bail!("accepted milestone did not invoke its pinned commit node");
    };
    Ok((
        AcceptedUnit {
            candidate: candidate.clone(),
            acceptance: artifact,
        },
        *commit,
    ))
}

async fn recorded_acceptance(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    accepted: &AcceptedUnit,
    checks: &WorkflowArtifact,
    policy_revision: &str,
) -> Result<CandidateAcceptance> {
    let ArtifactValue::Acceptance(reference) = &accepted.acceptance.value else {
        anyhow::bail!("expected a typed acceptance");
    };
    let ArtifactValue::Checks(verification) = &checks.value else {
        anyhow::bail!("expected typed checks");
    };
    let ArtifactValue::Candidate(selected) = &accepted.candidate.value else {
        anyhow::bail!("expected a typed candidate");
    };
    let receipt = store
        .candidate_acceptance(
            fixture.project.clone(),
            unit.task_id.clone(),
            reference.evidence.id.clone(),
        )
        .await?
        .context("missing acceptance receipt")?;
    assert_eq!(&receipt.candidate, selected);
    assert_eq!(receipt.candidate.target.unit_id, unit.unit.id);
    assert_eq!(receipt.verification.id, verification.id);
    assert_eq!(receipt.verification.policy_revision, policy_revision);
    assert!(
        matches!(receipt.verification.gates.as_slice(), [required, optional]
        if required.gate.required && matches!(required.outcome, Some(CandidateGateOutcome::Passed { .. }))
            && !optional.gate.required && matches!(&optional.outcome, Some(CandidateGateOutcome::CouldNotRun { reason }) if reason == OPTIONAL_REASON))
    );
    Ok(receipt)
}

fn commit_staged(root: &Path, message: &str) -> Result<String> {
    git(
        root,
        &[
            "-c",
            "user.name=Acceptance fixture",
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

fn assert_commit_paths(root: &Path, sha: &str, files: &[&str]) -> Result<()> {
    let changed = git(
        root,
        &["diff-tree", "--no-commit-id", "--name-only", "-r", sha],
    )?;
    let mut actual = changed.lines().collect::<Vec<_>>();
    let mut expected = files.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);
    Ok(())
}
