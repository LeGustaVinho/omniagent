use std::{env, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};

use crate::{
    conversations::SessionScope,
    providers::{
        profiles::{ConfiguredExecutable, DESKTOP_PERMISSION_REVISION, desktop_profile},
        runtime::start_managed_turn,
    },
    runner::{DirectWindowsRunner, Runner},
};

use super::{
    AcceptedUnit, AdmissionEnvironment, ArtifactValue, AttemptWorktreeEvidence, CandidateGate,
    CandidateGateAction, CandidateGateOutcome, CandidateTarget, CommitSource, Delivery,
    DeliveryGitPolicy, EventSource, Job, ManagedWorktree, NodeInputs, Provider, ProviderEventData,
    RunStatus, SessionMode, Store, TaskDeliveryEvidence, TaskWorkflowState, VerifiedCommit,
    WorkflowUnitOutcome, WorkflowUnitRequest, accept_candidate, approved_delivery_fixture,
    claim_fixture_job, commit, fs, git, implementation_submission, repository, select_candidate,
    submission, unit_request, user_commit, verified, verify_candidate,
};

const DEADLINE: Duration = Duration::from_secs(150);
const CHECK: &str = "Check the selected Git patch";

fn live_profile(provider: Provider, root: &Path, mode: SessionMode) -> Result<SessionScope> {
    let desktop = desktop_profile(
        ConfiguredExecutable {
            provider,
            executable: None,
        },
        None,
    )?;
    let selected = desktop
        .models
        .into_iter()
        .find(|model| {
            model.option.selectable
                && match provider {
                    Provider::Codex => model.option.selection.model_id == "gpt-6-luna",
                    Provider::Claude => matches!(
                        model.option.selection.model_id.as_str(),
                        "claude-haiku-5-5" | "haiku"
                    ),
                    _ => false,
                }
        })
        .with_context(|| format!("{provider:?} did not offer the live smoke model"))?;
    let effort = selected
        .efforts
        .iter()
        .any(|effort| effort == "low")
        .then(|| "low".into());
    Ok(SessionScope {
        provider,
        mode,
        checkout: root.to_owned(),
        profile_id: selected.option.selection.profile_id,
        account_binding: desktop.account_binding,
        executable: desktop.executable,
        cli_version: desktop.version,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: DESKTOP_PERMISSION_REVISION.into(),
        model: Some(selected.option.selection.model_id),
        effort,
    })
}

async fn run_live(
    store: &Store,
    job: Job,
    expected: Provider,
    label: &str,
) -> Result<(String, String)> {
    let project = job.project_id.clone();
    let attempt = claim_fixture_job(
        store,
        job,
        AdmissionEnvironment {
            runner: DirectWindowsRunner.identity().into(),
            bridge_available: false,
        },
    )
    .await?;
    let run = attempt.run.id.clone();
    let conversation = attempt.conversation_id.clone();
    let status = tokio::time::timeout(
        DEADLINE,
        start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), attempt, None).wait(),
    )
    .await
    .with_context(|| format!("{label} using {expected:?} timed out"))??;
    ensure!(
        status == RunStatus::Succeeded,
        "{label} using {expected:?} finished {status:?}: {:?}",
        store.job_failure(project, run.clone()).await?
    );
    let evidence = store
        .agent_dispatch(conversation.clone(), run.clone())
        .await?
        .with_context(|| format!("{label} did not save its provider dispatch"))?;
    assert_eq!(evidence.delivery, Delivery::Submitted);
    assert_eq!(evidence.snapshot.scope.provider, expected);
    eprintln!("{label}: {expected:?} completed a recorded provider dispatch");
    Ok((run, conversation))
}

async fn reject_other_scope(
    store: &Store,
    root: &Path,
    project: &str,
    writer: &ManagedWorktree,
    file: &str,
) -> Result<String> {
    let other = store
        .create_project_task(project.into(), "Unrelated parent".into())
        .await?;
    let subtask = store
        .create_project_subtask(project.into(), other.id, "Unrelated commit".into())
        .await?;
    let checkout = repository::provision_task_worktree(
        store,
        root.join("unrelated-worktrees"),
        project.into(),
        subtask.id,
        writer.checkout_id.clone(),
    )
    .await?;
    fs::copy(writer.root.join(file), checkout.root.join(file))?;
    user_commit(&checkout.root, file, "Git commit from a different Sub-Task")
}

async fn live_delivery(
    root: &Path,
    policy: DeliveryGitPolicy,
    writer: &SessionScope,
    reviewer: &SessionScope,
) -> Result<()> {
    fs::create_dir(root)?;
    let store = Store::open(root.join("workflow.sqlite"))?;
    let fixture = approved_delivery_fixture(&store, root).await?;
    let project = &fixture.project;
    let [first, second] = fixture.units.as_slice() else {
        anyhow::bail!("expected two approved units")
    };
    let task = &first.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("missing prepared worktree")?
        .worktree;
    let paths = env::var_os("PATH").context("Git for Windows requires PATH")?;
    let git_exe = env::split_paths(&paths)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("git.exe"))
        .find(|path| path.is_file())
        .context("Git for Windows is unavailable")?;
    if policy == DeliveryGitPolicy::UserCommitsAndMerges {
        store
            .set_delivery_git_policy(project.clone(), policy)
            .await?;
    }
    assert_eq!(store.delivery_git_policy(project.clone()).await?, policy);
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![CandidateGate {
                name: CHECK.into(),
                required: true,
                action: CandidateGateAction::Command {
                    executable: git_exe,
                    arguments: ["diff", "--check", "HEAD^", "HEAD"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    timeout_seconds: Some(30),
                },
            }],
        )
        .await?;
    let mut receipts: Vec<VerifiedCommit> = Vec::new();
    let mut previous = worktree.base_commit.clone();
    for (unit, file) in [(first, "first.txt"), (second, "second.txt")] {
        let ordinal = unit.unit.ordinal;
        let expected = format!("omniagent-delivery-unit-{ordinal}");
        let mut implementation = implementation_submission(&fixture, &worktree, unit)?;
        implementation.scope = SessionScope {
            checkout: worktree.root.clone(),
            ..writer.clone()
        };
        implementation.request.instructions = format!(
            "This is a disposable integration fixture. Edit only {file} in the current checkout. Its complete content should be {expected:?} without the quotes, followed by one newline. Save the file, then reply WRITE_OK_{ordinal}. Leave Git staging and commits to the workflow and keep every other file unchanged."
        );
        let job = store.submit_workflow_agent(implementation).await?;
        let label = format!("{policy:?} unit {ordinal} implementation");
        let (captured_run, _) = run_live(&store, job, Provider::Codex, &label).await?;
        let actual = fs::read_to_string(worktree.root.join(file))?.replace("\r\n", "\n");
        assert_eq!(actual.strip_suffix('\n').unwrap_or(&actual), expected);
        assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, previous);
        let Some(AttemptWorktreeEvidence::Saved { artifact: capture }) = store
            .attempt_worktree_evidence(project.clone(), captured_run.clone())
            .await?
        else {
            anyhow::bail!("live implementation did not produce confirmed capture evidence")
        };
        assert_eq!(capture.start.starting.head, previous);
        let target = CandidateTarget {
            project_id: project.clone(),
            task_id: task.clone(),
            unit_id: unit.unit.id.clone(),
            run_id: captured_run.clone(),
            capture_revision: capture.revision,
        };
        let candidate = select_candidate(&store, &fixture, target, &[file]).await?;
        let ArtifactValue::Candidate(selected) = &candidate.value else {
            anyhow::bail!("missing selected candidate")
        };

        let mut review = submission(
            &fixture.checkout,
            &fixture.run,
            "review-candidate",
            Some(unit.plan_revision.clone()),
            SessionMode::ReadOnly,
        );
        review.scope = SessionScope {
            checkout: worktree.root.clone(),
            ..reviewer.clone()
        };
        review.request.checkout_id = Some(worktree.checkout_id.clone());
        review.request.task_id = Some(task.clone());
        review.request.instructions = format!(
            "Use Read on {file} in the current checkout, then compare its content with {expected:?}. Treat the quote marks here as delimiters. A final newline is allowed, even if Read displays a blank final row. If the file matches, reply REVIEW_OK_{ordinal}; otherwise reply REVIEW_MISMATCH and give the reason. Keep the checkout read-only."
        );
        review.unit_id = Some(unit.unit.id.clone());
        review.artifacts = NodeInputs::from([("candidate".into(), candidate.id.clone())]);
        let job = store.submit_workflow_agent(review).await?;
        let label = format!("{policy:?} unit {ordinal} review");
        let (review_id, review_chat) = run_live(&store, job, Provider::Claude, &label).await?;
        let review_dispatch = store
            .agent_dispatch(review_chat.clone(), review_id.clone())
            .await?
            .context("missing review dispatch")?;
        let reviewed = review_dispatch
            .snapshot
            .scoped_launch
            .and_then(|launch| launch.candidate_checkout)
            .context("Claude did not receive a materialized candidate checkout")?;
        assert_eq!(reviewed.candidate_id, selected.id);
        assert_eq!(reviewed.tree, selected.tree);
        let marker = format!("REVIEW_OK_{ordinal}");
        let observations = store
            .agent_observations(review_chat, review_id.clone(), 0)
            .await?;
        let candidate_file = reviewed.working_directory.join(file);
        ensure!(
            observations.iter().any(|record| {
                if record.source != EventSource::Provider
                    || record.event.location.parent_item_id.is_some()
                {
                    return false;
                }
                let ProviderEventData::ContextRead { path, tool } = &record.event.data else {
                    return false;
                };
                tool == "Read"
                    && dunce::simplified(&reviewed.working_directory.join(path))
                        == dunce::simplified(&candidate_file)
            }),
            "{label} did not report a Read of the selected file in the isolated checkout"
        );
        let mut final_text = None;
        let mut last_message = None;
        for record in &observations {
            if record.source != EventSource::Provider
                || record.event.location.parent_item_id.is_some()
            {
                continue;
            }
            match &record.event.data {
                ProviderEventData::AssistantFinal { text } => final_text = Some(text.as_str()),
                ProviderEventData::AssistantMessage { text } => last_message = Some(text.as_str()),
                _ => {}
            }
        }
        let verdict = final_text.or(last_message).unwrap_or_default();
        let verdict_tokens = verdict.split(|c: char| !c.is_ascii_alphanumeric() && c != '_');
        ensure!(
            verdict_tokens.clone().any(|entry| entry == marker)
                && !verdict_tokens
                    .clone()
                    .any(|entry| entry == "REVIEW_MISMATCH"),
            "{label} did not affirm the selected candidate: {}",
            crate::diagnostics::preview(verdict)
        );
        let review = store
            .publish_workflow_artifact(
                project.clone(),
                fixture.run.id.clone(),
                "review-candidate".into(),
                "review".into(),
                ArtifactValue::Review { run_id: review_id },
            )
            .await?;
        let checks = verify_candidate(&store, &fixture, unit, &candidate).await?;
        let ArtifactValue::Checks(reference) = &checks.value else {
            anyhow::bail!("missing configured check receipt")
        };
        let gate_result = store
            .candidate_verification(project.clone(), task.clone(), reference.id.clone())
            .await?
            .context("missing candidate verification")?;
        assert!(
            gate_result.allows_acceptance()
                && gate_result.gates.iter().any(|gate| {
                    gate.gate.required
                        && gate.gate.name == CHECK
                        && matches!(gate.outcome, Some(CandidateGateOutcome::Passed { .. }))
                })
        );
        let acceptance =
            accept_candidate(&store, &fixture, unit, &candidate, &review, &checks).await?;
        let accepted = AcceptedUnit {
            candidate,
            acceptance,
        };
        let outcome = commit(&store, &fixture, unit, &accepted, None).await?;
        let outcome = if policy == DeliveryGitPolicy::UserCommitsAndMerges {
            assert_eq!(outcome, WorkflowUnitOutcome::WaitingForCommit);
            let state = store
                .project_task_delivery_state(project.clone(), task.clone())
                .await?
                .context("missing manual state")?;
            assert!(state.waiting_for_commit && state.state == TaskWorkflowState::InReview);
            assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, previous);
            eprintln!(
                "{policy:?} unit {ordinal}: verified checks accepted, commit waited for the operator"
            );
            if ordinal == 1 {
                let foreign = reject_other_scope(&store, root, project, &worktree, file).await?;
                assert!(
                    commit(&store, &fixture, unit, &accepted, Some(foreign))
                        .await
                        .is_err()
                );
                assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, previous);
                eprintln!("{policy:?} unit {ordinal}: rejected a SHA from another Sub-Task");
            }
            let sha = user_commit(
                &worktree.root,
                file,
                &format!("Operator commits unit {ordinal}"),
            )?;
            commit(&store, &fixture, unit, &accepted, Some(sha)).await?
        } else {
            outcome
        };
        let WorkflowUnitOutcome::Artifact {
            artifact: git_artifact,
        } = &outcome
        else {
            anyhow::bail!("missing Git artifact")
        };
        let result_inputs = NodeInputs::from([
            ("task".into(), fixture.task.id.clone()),
            ("git".into(), git_artifact.id.clone()),
        ]);
        let receipt = verified(&store, &fixture, unit, outcome).await?;
        assert_eq!(receipt.base_commit, previous);
        assert_eq!(receipt.checkout_id, worktree.checkout_id);
        assert_eq!(receipt.capture_run_id, captured_run);
        assert_eq!(
            receipt.source,
            if policy == DeliveryGitPolicy::UserCommitsAndMerges {
                CommitSource::External
            } else {
                CommitSource::Automatic
            }
        );
        assert!(!receipt.pending_changes_retained);
        assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, receipt.sha);
        previous = receipt.sha.clone();
        receipts.push(receipt);
        if ordinal == 1 {
            let state = store
                .project_task_delivery_state(project.clone(), task.clone())
                .await?
                .context("missing task progress")?;
            assert_ne!(state.state, TaskWorkflowState::Completed);
            assert!(state.completion.is_none());
            eprintln!("{policy:?} unit {ordinal}: first commit verified and task remained open");
        } else {
            store
                .execute_workflow_unit(unit_request(
                    &fixture,
                    unit,
                    "result",
                    result_inputs,
                    WorkflowUnitRequest::Result,
                ))
                .await?;
        }
    }
    assert_eq!(receipts.len(), 2);
    let state = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("missing completed task")?;
    assert_eq!(state.state, TaskWorkflowState::Completed);
    let delivery = state
        .completion
        .context("missing complete delivery receipt")?;
    let TaskDeliveryEvidence::VerifiedCommit {
        unit_evidence_ids,
        completion_sha,
        ..
    } = delivery.evidence
    else {
        anyhow::bail!("repository delivery must have verified Git evidence")
    };
    assert_eq!(
        unit_evidence_ids,
        receipts
            .into_iter()
            .map(|receipt| receipt.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(completion_sha, previous);
    eprintln!(
        "{policy:?}: two real Codex writes, two real Claude reviews, and two verified commits completed the task"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires installed authenticated Codex with GPT-6 Luna and Claude Code with Haiku 5.5"]
async fn live_cross_cli_delivery_verifies_two_units_in_automatic_and_manual_git_modes() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let writer = live_profile(Provider::Codex, temp.path(), SessionMode::ApprovedWrite)?;
    let reviewer = live_profile(Provider::Claude, temp.path(), SessionMode::ReadOnly)?;
    live_delivery(
        &temp.path().join("automatic"),
        DeliveryGitPolicy::AutomaticCommitUserMerge,
        &writer,
        &reviewer,
    )
    .await?;
    live_delivery(
        &temp.path().join("manual"),
        DeliveryGitPolicy::UserCommitsAndMerges,
        &writer,
        &reviewer,
    )
    .await
}
