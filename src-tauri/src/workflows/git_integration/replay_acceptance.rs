use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    jobs::{QueueState, RecoveryAction},
    workflows::{
        DeliveryRoles, WorkflowLaunchOrigin, WorkflowLaunchSource, builtin_delivery_graph,
        inspection::WorkflowExecutionInspection,
        tests::{approve_reviewed_plan_fixture, link_reviewed_plan_fixture},
        triggers::{TriggerWorkKind, WorkflowTriggerEvent, WorkflowTriggerSpec, activate},
    },
};

use super::*;

#[tokio::test]
async fn restart_recovers_an_unlaunched_writer_once_and_fences_a_writer_with_effects() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("writer-replay.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    let [first, next] = fixture.units.as_slice() else {
        anyhow::bail!("Expected two approved logical units");
    };
    let project = &fixture.project;
    let task = &first.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("Missing prepared writer worktree")?
        .worktree;
    let first_accepted =
        capture_and_review(&store, &fixture, &worktree, first, "first.txt").await?;
    let first_outcome = commit(&store, &fixture, first, &first_accepted, None).await?;
    let first_commit = verified(&store, &fixture, first, first_outcome.clone()).await?;
    let already_completed = settled_outputs(&inspect(&store, &fixture).await?, first);
    assert!(
        already_completed
            .values()
            .any(|artifact| artifact.id == fixture.release.id)
    );
    assert!(already_completed.values().any(|artifact| matches!(
        &artifact.value, ArtifactValue::Git(reference) if reference.id == first_commit.id
    )));

    let job = queue_implementation(&store, &fixture, &worktree, next).await?;
    let before_launch = claim_fixture_job(&store, job.clone(), fixture_admission()).await?;
    assert!(repository::capture_worktree_start(&store, before_launch.run.id.clone()).await?);
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        first_commit.sha
    );
    assert!(git(&worktree.root, &["status", "--porcelain"])?.is_empty());
    drop(store);

    let resumed = Store::open(database.clone())?;
    resumed.recover_agent_attempts_after_restart().await?;
    resumed.recover_agent_attempts_after_restart().await?;
    let reviewed = resumed
        .inspect_job(project.clone(), job.id.clone(), fixture_admission())
        .await?
        .context("Missing recovered workflow job")?;
    assert_eq!(
        reviewed.job.latest_attempt.as_ref().unwrap().status,
        RunStatus::Interrupted
    );
    assert_eq!(reviewed.job.queued_recovery, Some(RecoveryAction::Retry));
    let other = Store::open(database.clone())?;
    let (a, b) = tokio::join!(
        resumed.claim_next_job(fixture_admission()),
        other.claim_next_job(fixture_admission())
    );
    let claims = [a?, b?].into_iter().flatten().collect::<Vec<_>>();
    let [retry] = claims.as_slice() else {
        anyhow::bail!("The interrupted writer must have exactly one linked retry");
    };
    assert_eq!(
        retry.previous_run_id.as_deref(),
        Some(before_launch.run.id.as_str())
    );
    assert_eq!(retry.context_id, before_launch.context_id);
    assert_eq!(retry.input, before_launch.input);
    assert_eq!(retry.scope.checkout, before_launch.scope.checkout);
    let linked = resumed
        .workflow_attempt(project.clone(), retry.run.id.clone())
        .await?
        .context("The retry lost its workflow node scope")?;
    assert_eq!(linked.pin.workflow_run_id, fixture.run.id);
    assert_eq!(linked.pin.unit_id.as_deref(), Some(next.unit.id.as_str()));
    assert_eq!(
        settled_outputs(&inspect(&resumed, &fixture).await?, first),
        already_completed
    );

    let second_file = worktree.root.join("second.txt");
    let initial_bytes = fs::read(&second_file)?;
    perform_writer(
        &resumed,
        retry,
        &worktree.root,
        temp.path(),
        "second.txt",
        "interrupted worktree effect",
    )
    .await?;
    repository::save_worktree_candidate(&resumed, retry.run.id.clone()).await?;
    let saved = saved_diff(&resumed, project, &retry.run.id).await?;
    assert_eq!(saved.start.starting.head, first_commit.sha);
    let live_patch = worktree_patch(&saved)?;
    assert!(live_patch.contains("+interrupted worktree effect"));
    assert!(!live_patch.contains("first.txt"));
    let interrupted_with_effects = retry.run.id.clone();
    drop(other);
    drop(resumed);

    let recovered = Store::open(database.clone())?;
    recovered.recover_agent_attempts_after_restart().await?;
    let state = recovered
        .inspect_job(project.clone(), job.id.clone(), fixture_admission())
        .await?
        .context("Missing writer after the second restart")?;
    assert_eq!(
        state.job.latest_attempt.as_ref().unwrap().status,
        RunStatus::Interrupted
    );
    assert_eq!(
        state.job.latest_attempt.as_ref().unwrap().run_id,
        interrupted_with_effects
    );
    assert_eq!(state.job.queued_recovery, None);
    assert!(state.recovery_actions.is_empty());
    assert!(
        recovered
            .queue_job_recovery(
                project.clone(),
                job.id.clone(),
                interrupted_with_effects.clone(),
                RecoveryAction::Retry
            )
            .await
            .is_err()
    );
    assert!(
        recovered
            .claim_next_job(fixture_admission())
            .await?
            .is_none()
    );
    assert_eq!(
        saved_diff(&recovered, project, &interrupted_with_effects).await?,
        saved
    );
    assert!(
        recovered
            .project_task_delivery_state(project.clone(), task.clone())
            .await?
            .context("Missing work state")?
            .pending_repository_changes
    );

    let rejected = recovered
        .discard_pending_worktree_changes(
            project.clone(),
            worktree.checkout_id.clone(),
            task.clone(),
        )
        .await;
    assert!(rejected.is_err());
    assert!(fs::read_to_string(&second_file)?.contains("interrupted worktree effect"));
    fs::write(&second_file, initial_bytes)?;
    recovered
        .discard_pending_worktree_changes(
            project.clone(),
            worktree.checkout_id.clone(),
            task.clone(),
        )
        .await?;
    assert!(
        !recovered
            .project_task_delivery_state(project.clone(), task.clone())
            .await?
            .context("Missing reviewed work state")?
            .pending_repository_changes
    );

    let replacement = queue_implementation(&recovered, &fixture, &worktree, next).await?;
    assert_ne!(replacement.id, job.id);
    assert_eq!(replacement.pipeline, job.pipeline);
    let writer = claim_fixture_job(&recovered, replacement, fixture_admission()).await?;
    assert!(writer.previous_run_id.is_none());
    assert_ne!(writer.run.id, interrupted_with_effects);
    perform_writer(
        &recovered,
        &writer,
        &worktree.root,
        temp.path(),
        "second.txt",
        "reviewed replacement",
    )
    .await?;
    repository::save_worktree_candidate(&recovered, writer.run.id.clone()).await?;
    recovered
        .finish_agent_attempt(writer.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    let written = saved_diff(&recovered, project, &writer.run.id).await?;
    let current = select_candidate(
        &recovered,
        &fixture,
        CandidateTarget {
            project_id: project.clone(),
            task_id: task.clone(),
            unit_id: next.unit.id.clone(),
            capture_revision: written.revision,
            run_id: writer.run.id.clone(),
        },
        &["second.txt"],
    )
    .await?;
    let review = review_candidate(
        &recovered,
        &fixture,
        &worktree,
        next,
        &current,
        &first_reviewer(),
    )
    .await?;
    let checks = verify_candidate(&recovered, &fixture, next, &current).await?;
    let acceptance =
        accept_candidate(&recovered, &fixture, next, &current, &review, &checks).await?;
    let accepted = AcceptedUnit {
        candidate: current,
        acceptance,
    };
    let outcome = commit(&recovered, &fixture, next, &accepted, None).await?;
    let completed = verified(&recovered, &fixture, next, outcome.clone()).await?;
    assert_eq!(completed.base_commit, first_commit.sha);
    assert_eq!(completed.capture_run_id, writer.run.id);
    assert_eq!(
        git(&worktree.root, &["show", "HEAD:second.txt"])?,
        "reviewed replacement"
    );
    assert!(
        queue_implementation(&recovered, &fixture, &worktree, first)
            .await
            .is_err()
    );
    assert_eq!(
        commit(&recovered, &fixture, first, &first_accepted, None).await?,
        first_outcome
    );
    assert_eq!(
        commit(&recovered, &fixture, next, &accepted, None).await?,
        outcome
    );
    assert_eq!(
        settled_outputs(&inspect(&recovered, &fixture).await?, first),
        already_completed
    );
    assert_eq!(
        git(
            &worktree.root,
            &[
                "rev-list",
                "--count",
                &format!("{}..HEAD", worktree.base_commit)
            ]
        )?,
        "2"
    );
    let attempts = inspect(&recovered, &fixture)
        .await?
        .jobs
        .into_iter()
        .find(|entry| entry.job.id == job.id)
        .context("Missing linked attempts in execution inspection")?;
    assert_eq!(attempts.attempts.len(), 2);
    assert!(
        attempts
            .attempts
            .iter()
            .any(|attempt| attempt.run_id == interrupted_with_effects
                && attempt.previous_run_id.as_deref() == Some(before_launch.run.id.as_str())
                && attempt.status == RunStatus::Interrupted)
    );
    Ok(())
}

#[tokio::test]
async fn restart_holds_an_effectful_writer_without_a_confirmed_capture_for_reconciliation()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("uncaptured-writer.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let version = store
        .save_workflow_revision(
            repo.project_id.clone(),
            "replay-from-task".into(),
            builtin_delivery_graph(DeliveryRoles::default()),
        )
        .await?;
    let document = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let rule = store
        .save_workflow_trigger_rule(
            repo.project_id.clone(),
            None,
            None,
            WorkflowTriggerSpec {
                name: "Deliver new Tasks".into(),
                enabled: true,
                event: WorkflowTriggerEvent::Created,
                work: TriggerWorkKind::Task,
                target_state: None,
                pipeline: version.clone(),
                source: WorkflowLaunchSource {
                    checkout_id: repo.checkout_id.clone(),
                    revision_id: document.revision.id,
                },
            },
        )
        .await?;
    let task_materialized = store
        .create_project_task(
            repo.project_id.clone(),
            "Write with explicit recovery".into(),
        )
        .await?;
    let activations = store
        .workflow_trigger_activations(repo.project_id.clone())
        .await?;
    let [activation] = activations.as_slice() else {
        anyhow::bail!("Expected one real Task-created activation")
    };
    assert_eq!(activation.rule_id, rule.id);
    assert_eq!(activation.work_id, task_materialized.id);
    let event = store
        .project_work_events(repo.project_id.clone(), None, 20)
        .await?
        .into_iter()
        .find(|event| event.id == activation.event_id)
        .context("Missing original work event")?;
    let run_id = activation
        .run_id
        .clone()
        .context("Missing trigger workflow run")?;
    let launched = store
        .workflow_run(repo.project_id.clone(), run_id.clone())
        .await?
        .context("Missing pinned run")?;
    let plan = "Deliver the two changes separately.\n\n```omniagent-plan\n{\"units\":[{\"purpose\":\"First change\",\"completionCriteria\":[\"First file updated\"]},{\"purpose\":\"Second change\",\"completionCriteria\":[\"Second file updated\"]}]}\n```";
    let linked = link_reviewed_plan_fixture(
        &store,
        &repo,
        Some(task_materialized.id.clone()),
        &launched,
        plan,
        "Write with explicit recovery",
    )
    .await?;
    let fixture =
        approve_reviewed_plan_fixture(&store, temp.path(), repo, launched, linked).await?;
    let [first, unit] = fixture.units.as_slice() else {
        anyhow::bail!("Expected two released units")
    };
    let project = &fixture.project;
    let task = &unit.task_id;
    assert_eq!(*task, task_materialized.id);
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("Missing prepared writer worktree")?
        .worktree;
    let first_accepted =
        capture_and_review(&store, &fixture, &worktree, first, "first.txt").await?;
    let first_outcome = commit(&store, &fixture, first, &first_accepted, None).await?;
    let first_commit = verified(&store, &fixture, first, first_outcome).await?;
    let first_outputs = settled_outputs(&inspect(&store, &fixture).await?, first);
    let job = queue_implementation(&store, &fixture, &worktree, unit).await?;
    let attempt = claim_fixture_job(&store, job.clone(), fixture_admission()).await?;
    let pin = store
        .workflow_attempt(project.clone(), attempt.run.id.clone())
        .await?
        .context("Original writer missing its exact stage pin")?
        .pin;
    let path = worktree.root.join("second.txt");
    let baseline = fs::read(&path)?;
    start_writer(&store, &attempt).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 13, "contextId": attempt.context_id, "scope": attempt.scope,
        "submittedMessages": attempt.input, "nativeHistory": { "mode": "fresh" },
    }))?;
    let dispatch = store
        .prepare_dispatch(
            attempt.run.id.clone(),
            snapshot,
            vec![b"writer dispatch".to_vec()],
        )
        .await?;
    store
        .settle_dispatch(dispatch.clone(), Delivery::Submitted)
        .await?;
    write_effect(
        &worktree.root,
        temp.path(),
        "second.txt",
        "uncaptured effect",
    )?;
    drop(store);

    let recovered = Store::open(database.clone())?;
    recovered.recover_agent_attempts_after_restart().await?;
    recovered
        .execute(move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            activate(
                &tx,
                &event.project_id,
                &event.task_id,
                &event.id,
                &event.data,
                &event.origin,
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    let replay = recovered
        .workflow_trigger_activations(project.clone())
        .await?;
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].event_id, activation.event_id);
    assert_eq!(replay[0].work_id, *task);
    assert_eq!(replay[0].run_id.as_deref(), Some(run_id.as_str()));
    let launch = recovered
        .workflow_launch(project.clone(), run_id.clone())
        .await?
        .context("Lost original launch receipt")?;
    assert_eq!(launch.version, version);
    assert_eq!(
        launch.origin,
        WorkflowLaunchOrigin::Rule {
            rule_id: rule.id,
            event_id: activation.event_id.clone(),
            work_id: task.clone()
        }
    );
    assert_eq!(
        settled_outputs(&inspect(&recovered, &fixture).await?, first),
        first_outputs
    );
    let job_state = recovered
        .inspect_job(project.clone(), job.id.clone(), fixture_admission())
        .await?
        .context("Missing interrupted writer")?;
    assert_eq!(
        job_state.job.latest_attempt.as_ref().unwrap().status,
        RunStatus::Interrupted
    );
    assert!(job_state.recovery_actions.is_empty());
    assert!(job_state.job.queued_recovery.is_none());
    assert!(matches!(
        recovered
            .attempt_worktree_evidence(project.clone(), attempt.run.id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Unavailable { .. })
    ));
    assert!(fs::read_to_string(&path)?.contains("uncaptured effect"));
    let replacement = queue_implementation(&recovered, &fixture, &worktree, unit).await?;
    assert!(
        recovered
            .claim_next_job(fixture_admission())
            .await?
            .is_none()
    );
    let blocked = recovered
        .inspect_job(project.clone(), replacement.id.clone(), fixture_admission())
        .await?
        .context("Missing writer blocked by uncertain scope")?;
    assert!(blocked.blockers.iter().any(|blocker| matches!(
        blocker, AdmissionBlocker::Writer { run_id, needs_review: true, .. } if run_id == &attempt.run.id
    )), "{:?}", blocked.blockers);
    recovered
        .set_pending_job_state(project.clone(), replacement.id, QueueState::Cancelled)
        .await?;
    let effect = recovered
        .interrupted_writer_review(project.clone(), job.id.clone(), attempt.run.id.clone())
        .await?
        .context("Missing interrupted writer effect review")?;
    assert_eq!(effect.starting_head, first_commit.sha);
    assert!(!effect.worktree_restored);
    assert_eq!(serde_json::to_value(&effect.dispatches)?[0]["id"], dispatch);
    assert!(
        recovered
            .reconcile_interrupted_writer(
                project.clone(),
                job.id.clone(),
                attempt.run.id.clone(),
                effect.revision.clone(),
                true
            )
            .await
            .is_err()
    );
    fs::write(&path, &baseline)?;
    assert!(
        recovered
            .discard_pending_worktree_changes(
                project.clone(),
                worktree.checkout_id.clone(),
                task.clone(),
            )
            .await
            .is_err()
    );
    assert!(
        recovered
            .claim_next_job(fixture_admission())
            .await?
            .is_none()
    );
    git(
        &worktree.root,
        &["update-index", "--assume-unchanged", "second.txt"],
    )?;
    let index_changed = recovered
        .interrupted_writer_review(project.clone(), job.id.clone(), attempt.run.id.clone())
        .await?
        .context("Missing review for changed index flags")?;
    assert!(!index_changed.index_restored);
    assert!(
        recovered
            .reconcile_interrupted_writer(
                project.clone(),
                job.id.clone(),
                attempt.run.id.clone(),
                index_changed.revision,
                true
            )
            .await
            .is_err()
    );
    git(
        &worktree.root,
        &["update-index", "--no-assume-unchanged", "second.txt"],
    )?;
    let restored = recovered
        .interrupted_writer_review(project.clone(), job.id.clone(), attempt.run.id.clone())
        .await?
        .context("Missing restored writer review")?;
    assert!(restored.index_restored && restored.worktree_restored);
    assert_ne!(effect.revision, restored.revision);
    assert!(
        recovered
            .reconcile_interrupted_writer(
                project.clone(),
                job.id.clone(),
                attempt.run.id.clone(),
                effect.revision,
                true
            )
            .await
            .is_err()
    );
    assert!(
        recovered
            .reconcile_interrupted_writer(
                project.clone(),
                job.id.clone(),
                attempt.run.id.clone(),
                restored.revision.clone(),
                false
            )
            .await
            .is_err()
    );
    let receipt = recovered
        .reconcile_interrupted_writer(
            project.clone(),
            job.id.clone(),
            attempt.run.id.clone(),
            restored.revision,
            true,
        )
        .await?;
    assert_eq!(receipt.source_run_id, attempt.run.id);
    assert!(
        recovered
            .claim_next_job(fixture_admission())
            .await?
            .is_none()
    );
    drop(recovered);
    let recovered = Store::open(database)?;
    recovered.recover_agent_attempts_after_restart().await?;
    assert!(
        recovered
            .claim_next_job(fixture_admission())
            .await?
            .is_none()
    );
    let state = recovered
        .inspect_job(project.clone(), job.id.clone(), fixture_admission())
        .await?
        .context("Lost retry after restart")?;
    assert_eq!(state.recovery_actions, [RecoveryAction::Retry]);
    assert!(state.job.queued_recovery.is_none());
    assert_eq!(
        recovered
            .interrupted_writer_review(project.clone(), job.id.clone(), attempt.run.id.clone())
            .await?
            .context("Lost durable effect review")?
            .receipt,
        Some(receipt)
    );
    assert!(
        recovered
            .queue_job_recovery(
                project.clone(),
                job.id.clone(),
                attempt.run.id.clone(),
                RecoveryAction::Resume
            )
            .await
            .is_err()
    );
    recovered
        .queue_job_recovery(
            project.clone(),
            job.id.clone(),
            attempt.run.id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    let retry = claim_fixture_job(&recovered, job, fixture_admission()).await?;
    assert_eq!(
        retry.previous_run_id.as_deref(),
        Some(attempt.run.id.as_str())
    );
    assert_eq!(retry.input, attempt.input);
    assert_eq!(
        recovered
            .workflow_attempt(project.clone(), retry.run.id.clone())
            .await?
            .context("Linked retry lost workflow pin")?
            .pin,
        pin
    );
    fs::write(&path, b"worktree changed after retry was queued")?;
    assert!(
        repository::capture_worktree_start(&recovered, retry.run.id.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("checkout changed before retry")
    );
    fs::write(&path, baseline)?;
    perform_writer(
        &recovered,
        &retry,
        &worktree.root,
        temp.path(),
        "second.txt",
        "recovered after operator review",
    )
    .await?;
    repository::save_worktree_candidate(&recovered, retry.run.id.clone()).await?;
    recovered
        .finish_agent_attempt(retry.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    let saved = saved_diff(&recovered, project, &retry.run.id).await?;
    assert_eq!(saved.start.starting.head, first_commit.sha);
    assert!(worktree_patch(&saved)?.contains("+recovered after operator review"));
    assert_eq!(
        settled_outputs(&inspect(&recovered, &fixture).await?, first),
        first_outputs
    );
    Ok(())
}

async fn perform_writer(
    store: &Store,
    writer: &AttemptPlan,
    root: &Path,
    fixture_root: &Path,
    file: &str,
    text: &str,
) -> Result<()> {
    start_writer(store, writer).await?;
    write_effect(root, fixture_root, file, text)
}

async fn start_writer(store: &Store, writer: &AttemptPlan) -> Result<()> {
    assert!(repository::capture_worktree_start(store, writer.run.id.clone()).await?);
    assert!(
        store
            .record_agent_launch_intent(writer.run.id.clone())
            .await?
    );
    store.mark_agent_running(writer.run.id.clone()).await
}

fn write_effect(root: &Path, fixture_root: &Path, file: &str, text: &str) -> Result<()> {
    let patch = fixture_root.join("writer-fixture.patch");
    let previous = git(root, &["show", &format!("HEAD:{file}")])?;
    fs::write(
        &patch,
        format!(
            "diff --git a/{file} b/{file}\n--- a/{file}\n+++ b/{file}\n@@ -1 +1 @@\n-{previous}\n+{text}\n"
        ),
    )?;
    git(
        root,
        &[
            "apply",
            "--ignore-space-change",
            "--whitespace=nowarn",
            patch.to_str().context("Fixture path lost UTF-8")?,
        ],
    )?;
    Ok(())
}

async fn inspect(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
) -> Result<WorkflowExecutionInspection> {
    store
        .inspect_workflow_execution(fixture.project.clone(), fixture.run.id.clone())
        .await?
        .context("Workflow run is unavailable")
}

fn settled_outputs(
    inspection: &WorkflowExecutionInspection,
    first: &WorkflowDeliveryUnit,
) -> BTreeMap<String, WorkflowArtifact> {
    inspection
        .outputs
        .iter()
        .filter(|entry| {
            entry
                .artifact
                .unit_id
                .as_ref()
                .is_none_or(|unit| unit == &first.unit.id)
        })
        .map(|entry| (entry.artifact.id.clone(), entry.artifact.clone()))
        .collect()
}

async fn saved_diff(
    store: &Store,
    project: &str,
    run: &str,
) -> Result<Box<repository::CandidateRevision>> {
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.into(), run.into())
        .await?
    else {
        anyhow::bail!("Missing candidate checkpoint for {run}");
    };
    Ok(artifact)
}

fn worktree_patch(candidate: &repository::CandidateRevision) -> Result<String> {
    let changes = candidate
        .candidate
        .changes
        .as_ref()
        .context("Missing captured diff")?;
    Ok(String::from_utf8(
        STANDARD.decode(&changes.unstaged_patch_base64)?,
    )?)
}
