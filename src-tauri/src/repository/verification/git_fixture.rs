use super::*;

#[tokio::test]
async fn candidate_checks_use_the_selected_tree_and_supersede_stale_acceptance() -> Result<()> {
    use omniagent::repository::verification::{
        CandidateGate, CandidateGateAction, CandidateGateOutcome, CandidateSelection,
        CandidateTarget,
    };

    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Selected changes".into())
        .await?;
    let other = store
        .create_project_task(project.project_id.clone(), "Other scope".into())
        .await?;
    let worktree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    let runner = Arc::new(CandidateWriter);
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: false,
    };
    let parent = candidate_parent(&store, &project, &runner).await?;
    fs::write(
        worktree.root.join("pass-token.txt"),
        b"outside the first selection\n",
    )?;
    queue_approved_writer(
        &store,
        &env,
        &worktree,
        &parent,
        &task.id,
        "candidate verification",
    )
    .await?;
    let attempt = store
        .claim_next_job(env)
        .await?
        .context("writer was not claimed")?;
    let run_id = attempt.run.id.clone();
    let outcome = start_managed_turn(store.clone(), runner, attempt, None)
        .wait()
        .await?;
    assert_eq!(outcome, RunStatus::Succeeded);
    store
        .finish_agent_attempt(parent, RunStatus::Succeeded, None)
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.project_id.clone(), run_id.clone())
        .await?
    else {
        anyhow::bail!("missing confirmed writer capture");
    };
    let target = CandidateTarget {
        project_id: project.project_id.clone(),
        task_id: task.id.clone(),
        unit_id: "first unit".into(),
        run_id,
        capture_revision: artifact.revision,
    };
    let power_shell =
        std::path::PathBuf::from(std::env::var_os("SystemRoot").context("missing SystemRoot")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let gate_args = vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
        "if (Test-Path -LiteralPath 'pass-token.txt') { Write-Output 'marker present'; exit 0 } else { Write-Output 'marker missing'; exit 17 }".into()];
    let live = Command::new(&power_shell)
        .args(&gate_args)
        .current_dir(&worktree.root)
        .output()?;
    assert!(
        live.status.success(),
        "the unselected file would make the live check pass"
    );
    store
        .set_candidate_gate_policy(
            project.project_id.clone(),
            vec![
                CandidateGate {
                    name: "required tests".into(),
                    required: true,
                    action: CandidateGateAction::Command {
                        executable: power_shell,
                        arguments: gate_args,
                        timeout_seconds: Some(30),
                    },
                },
                CandidateGate {
                    name: "optional build".into(),
                    required: false,
                    action: CandidateGateAction::Unavailable {
                        reason: "No fixture build command".into(),
                    },
                },
            ],
        )
        .await?;

    let first = store
        .select_worktree_candidate(
            target.clone(),
            CandidateSelection {
                tracked_paths: Some(vec!["staged.txt".into()]),
                untracked_paths: vec!["candidate.bin".into()],
            },
        )
        .await?;
    assert!(
        store
            .select_worktree_candidate(
                CandidateTarget {
                    task_id: other.id.clone(),
                    ..target.clone()
                },
                first.selection.clone()
            )
            .await
            .is_err()
    );
    fs::write(
        worktree.root.join("candidate.bin"),
        b"changed after capture",
    )?;
    let isolated = store
        .materialize_selected_candidate(
            project.project_id.clone(),
            task.id.clone(),
            first.id.clone(),
        )
        .await?;
    assert_eq!(git(isolated.path(), &["write-tree"])?, first.tree);
    assert_eq!(
        git(isolated.path(), &["rev-parse", "HEAD^{tree}"])?,
        first.tree
    );
    assert_eq!(
        git(isolated.path(), &["rev-parse", "HEAD^"])?,
        first.base_commit
    );
    assert!(git(isolated.path(), &["status", "--porcelain"])?.is_empty());
    assert_eq!(
        fs::read(isolated.path().join("staged.txt"))?,
        b"Unstaged by the CLI\n"
    );
    assert_eq!(
        fs::read(isolated.path().join("candidate.bin"))?,
        [0, 255, 68, 13, 10]
    );
    assert!(!isolated.path().join("committed.txt").exists());
    assert!(!isolated.path().join("pass-token.txt").exists());
    assert!(
        !isolated
            .path()
            .join(".git/objects/info/alternates")
            .exists()
    );

    let failed = store
        .verify_selected_candidate(
            project.project_id.clone(),
            task.id.clone(),
            first.id.clone(),
        )
        .await?;
    assert!(!failed.allows_acceptance());
    assert!(
        matches!(&failed.gates[0].outcome, Some(CandidateGateOutcome::Failed { exit_code: 17, stdout, .. }) if stdout.contains("marker missing")),
        "{failed:?}"
    );
    assert!(
        store
            .accept_selected_candidate(project.project_id.clone(), task.id.clone(), first.id)
            .await
            .is_err()
    );
    let second = store
        .select_worktree_candidate(
            target.clone(),
            CandidateSelection {
                tracked_paths: None,
                untracked_paths: vec!["pass-token.txt".into(), "candidate.bin".into()],
            },
        )
        .await?;
    assert!(
        store
            .accept_selected_candidate(
                project.project_id.clone(),
                task.id.clone(),
                second.id.clone()
            )
            .await
            .is_err()
    );
    let missing = store
        .verify_selected_candidate_with_requirements(
            project.project_id.clone(),
            task.id.clone(),
            second.id.clone(),
            vec!["integration checks".into()],
        )
        .await?;
    assert!(!missing.allows_acceptance());
    let required = missing
        .gates
        .last()
        .context("missing pinned required gate")?;
    assert!(required.gate.required);
    assert!(matches!(
        &required.outcome,
        Some(CandidateGateOutcome::CouldNotRun { reason }) if reason.contains("integration checks")
    ));
    let passed = store
        .verify_selected_candidate(
            project.project_id.clone(),
            task.id.clone(),
            second.id.clone(),
        )
        .await?;
    assert!(passed.allows_acceptance(), "{passed:?}");
    assert!(
        store
            .accept_candidate_verification(
                project.project_id.clone(),
                task.id.clone(),
                second.id.clone(),
                missing.id,
            )
            .await
            .is_err()
    );
    let accepted = store
        .accept_candidate_verification(
            project.project_id.clone(),
            task.id.clone(),
            second.id,
            passed.id,
        )
        .await?;
    assert!(
        matches!(&accepted.verification.gates[1].outcome, Some(CandidateGateOutcome::CouldNotRun { reason }) if reason == "No fixture build command")
    );
    assert_eq!(
        store
            .current_candidate_acceptance(
                project.project_id.clone(),
                task.id.clone(),
                accepted.id.clone()
            )
            .await?,
        accepted
    );

    let third = store
        .select_worktree_candidate(
            target,
            CandidateSelection {
                tracked_paths: None,
                untracked_paths: vec!["pass-token.txt".into()],
            },
        )
        .await?;
    assert_ne!(third.tree, accepted.candidate.tree);
    assert!(
        store
            .accept_selected_candidate(project.project_id.clone(), task.id.clone(), third.id)
            .await
            .is_err()
    );
    assert!(
        store
            .current_candidate_acceptance(
                project.project_id.clone(),
                task.id.clone(),
                accepted.id.clone()
            )
            .await
            .is_err()
    );
    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(
        reopened
            .candidate_verification(
                project.project_id.clone(),
                task.id.clone(),
                failed.id.clone()
            )
            .await?,
        Some(failed)
    );
    assert_eq!(
        reopened
            .candidate_acceptance(project.project_id.clone(), task.id, accepted.id.clone())
            .await?,
        Some(accepted.clone())
    );
    assert!(
        reopened
            .candidate_acceptance(project.project_id, other.id, accepted.id)
            .await?
            .is_none()
    );
    Ok(())
}
