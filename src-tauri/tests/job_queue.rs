use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::{AttemptPlan, Conversation, SessionScope},
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, Job, JobContext, JobOrigin, JobRequest,
        JobRequestConflict, QueueSettings, QueueState, RecoveryAction,
    },
    providers::{Capability, Provider, SessionIntent, SessionMode},
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
};

async fn origin(store: &Store, path: &Path) -> Result<AttemptPlan> {
    origin_with_mode(store, path, SessionMode::ReadOnly).await
}

async fn origin_with_mode(store: &Store, path: &Path, mode: SessionMode) -> Result<AttemptPlan> {
    fs::create_dir(path)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(path)
            .status()?
            .success()
    );
    let project = repository::register(store, path.into()).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Parent".into())
        .await?;
    let attempt = store
        .prepare_agent_attempt(
            chat.id,
            chat.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "test".into(),
                account_binding: None,
                checkout: dunce::canonicalize(path)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: "read-only".into(),
                mode,
                model: None,
                effort: None,
            },
            "Plan the fixture".into(),
        )
        .await?;
    store.mark_agent_running(attempt.run.id.clone()).await?;
    Ok(attempt)
}

fn request() -> JobRequest {
    JobRequest {
        checkout_id: None,
        task_id: None,
        mode: SessionMode::Plan,
        instructions: "Use the confidential fixture canary ☕".into(),
        dependencies: Vec::new(),
        requires_bridge: false,
    }
}

#[tokio::test]
async fn concurrent_replays_resolve_to_the_same_persisted_job_and_conflict_on_changed_input()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("jobs.sqlite3");
    let primary = Store::open(database.clone())?;
    let secondary = Store::open(database.clone())?;
    let parent = origin(&primary, &temp.path().join("parent")).await?;
    let run = parent.run.id;
    let (first, replay) = tokio::join!(
        primary.submit_agent_job(run.clone(), "stable-request".into(), request()),
        secondary.submit_agent_job(run.clone(), "stable-request".into(), request()),
    );
    let job = first?;
    assert_eq!(job, replay?);
    assert_eq!(
        job.origin,
        JobOrigin::Agent {
            run_id: run.clone(),
            request_id: "stable-request".into()
        }
    );
    let context = job.context.as_ref().unwrap();
    assert_eq!(context.conversation_id, parent.conversation_id);
    assert_eq!(context.context_id, parent.context_id);
    assert_eq!(job.checkout_id, parent.run.checkout_id);
    assert!(job.latest_attempt.is_none());

    let error = primary
        .submit_agent_job(
            run.clone(),
            "stable-request".into(),
            JobRequest {
                instructions: "Different instructions".into(),
                ..request()
            },
        )
        .await
        .unwrap_err();
    assert!(error.is::<JobRequestConflict>());
    primary
        .set_pending_job_state(job.project_id.clone(), job.id.clone(), QueueState::Paused)
        .await?;
    primary
        .finish_agent_attempt(run.clone(), RunStatus::Cancelled, None)
        .await?;
    drop(primary);
    drop(secondary);

    let reopened = Store::open(database.clone())?;
    let saved = reopened
        .submit_agent_job(
            run.clone(),
            "stable-request".into(),
            JobRequest {
                checkout_id: Some(job.checkout_id.clone()),
                ..request()
            },
        )
        .await?;
    assert_eq!(saved.id, job.id);
    assert_eq!(saved.queue_state, QueueState::Paused);
    assert!(
        reopened
            .submit_agent_job(run, "new-request".into(), request())
            .await
            .is_err()
    );
    let raw = rusqlite::Connection::open(database)?;
    let (rows, blob): (i64, Vec<u8>) =
        raw.query_row("SELECT count(*), request FROM jobs", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    assert_eq!(rows, 1);
    assert!(
        !blob
            .windows(b"confidential fixture canary".len())
            .any(|window| window == b"confidential fixture canary")
    );
    Ok(())
}

#[tokio::test]
async fn job_trust_is_scoped_to_the_origin_project_and_writes_need_a_managed_worktree() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("jobs.sqlite3"))?;
    let first = origin(&store, &temp.path().join("first")).await?.run;
    let second = origin(&store, &temp.path().join("second")).await?.run;
    let task = store
        .create_project_task(first.project_id.clone(), "Investigate".into())
        .await?;
    let one = store
        .submit_agent_job(
            first.id.clone(),
            "same".into(),
            JobRequest {
                task_id: Some(task.id.clone()),
                ..request()
            },
        )
        .await?;
    let two = store
        .submit_agent_job(second.id.clone(), "same".into(), request())
        .await?;
    assert_ne!(one.id, two.id);
    assert!(
        store
            .job(first.project_id.clone(), two.id.clone())
            .await?
            .is_none()
    );
    for denied in [
        JobRequest {
            checkout_id: Some(second.checkout_id),
            ..request()
        },
        JobRequest {
            dependencies: vec![two.id],
            ..request()
        },
        JobRequest {
            mode: SessionMode::ApprovedWrite,
            ..request()
        },
        JobRequest {
            task_id: Some(task.id.clone()),
            mode: SessionMode::ApprovedWrite,
            ..request()
        },
    ] {
        assert!(
            store
                .submit_agent_job(first.id.clone(), "reusable".into(), denied)
                .await
                .is_err()
        );
    }
    assert!(
        store
            .submit_agent_job(
                second.id,
                "cross-task".into(),
                JobRequest {
                    task_id: Some(task.id),
                    ..request()
                }
            )
            .await
            .is_err()
    );
    let dependent = store
        .submit_agent_job(
            first.id.clone(),
            "reusable".into(),
            JobRequest {
                dependencies: vec![one.id.clone(), one.id.clone()],
                ..request()
            },
        )
        .await?;
    let normalized_replay = store
        .submit_agent_job(
            first.id,
            "reusable".into(),
            JobRequest {
                dependencies: vec![one.id],
                ..request()
            },
        )
        .await?;
    assert_eq!(dependent.id, normalized_replay.id);
    assert_eq!(dependent.queue_state, QueueState::Queued);
    Ok(())
}

fn environment(bridge_available: bool) -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available,
    }
}

async fn approve(store: &Store, job: &Job) -> Result<()> {
    let pending = store
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .context("missing job")?;
    let approval = pending
        .blockers
        .into_iter()
        .find_map(|blocker| match blocker {
            AdmissionBlocker::Approval { approval_id } => Some(approval_id),
            _ => None,
        })
        .context("missing linked approval")?;
    store
        .decide_job_approval(job.project_id.clone(), approval, true)
        .await
}

#[tokio::test]
async fn queued_conversation_draft_and_snapshot_survive_reopen_without_creating_a_run() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let parent = origin(&store, &temp.path().join("project")).await?;
    store
        .finish_agent_attempt(parent.run.id, RunStatus::Cancelled, None)
        .await?;
    let chat = store
        .create_conversation(parent.run.checkout_id, "Durable queue".into())
        .await?;
    let job_context = JobContext {
        conversation_id: chat.id.clone(),
        context_id: chat.context_id.clone(),
    };
    store
        .save_conversation_draft(chat.id.clone(), "Saved exact prompt ☕".into())
        .await?;
    assert!(
        store
            .enqueue_conversation_turn(
                job_context.clone(),
                parent.scope.clone(),
                "Raced draft".into()
            )
            .await
            .is_err()
    );
    assert_eq!(
        store.conversation_draft(chat.id.clone()).await?,
        "Saved exact prompt ☕"
    );
    let settings = QueueSettings {
        paused: true,
        capacity: 2,
    };
    store.set_job_queue_settings(settings).await?;
    let queued = store
        .enqueue_conversation_turn(job_context, parent.scope, "Saved exact prompt ☕".into())
        .await?;
    assert!(store.conversation_draft(chat.id.clone()).await?.is_empty());
    assert!(
        store
            .latest_conversation_run(chat.id.clone())
            .await?
            .is_none()
    );
    assert_eq!(
        store.conversation(chat.id.clone()).await?.context_id,
        chat.context_id
    );
    drop(store);
    let store = Store::open(database)?;
    assert_eq!(store.job_queue_settings().await?, settings);
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let current = store
        .latest_conversation_job(chat.id.clone(), environment(false))
        .await?
        .context("pending job missing")?;
    assert_eq!(current.job, queued);
    assert_eq!(current.blockers, [AdmissionBlocker::QueuePaused]);
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            ..settings
        })
        .await?;
    let plan = store
        .claim_next_job(environment(false))
        .await?
        .context("did not claim saved job")?;
    assert_eq!(plan.conversation_id, chat.id);
    assert_eq!(
        plan.input.last().context("missing prompt")?.text,
        "Saved exact prompt ☕"
    );
    assert_eq!(
        store
            .latest_conversation_run(chat.id.clone())
            .await?
            .context("missing run")?,
        plan.run
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    store
        .finish_agent_attempt(plan.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    assert_eq!(
        store
            .job(queued.project_id, queued.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn inspector_and_claim_agree_on_approval_dependencies_bridge_and_capacity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let parallel = Store::open(database)?;
    let parent = origin(&store, &temp.path().join("project")).await?;
    let task = store
        .create_project_task(parent.run.project_id.clone(), "Queue task".into())
        .await?;
    let bridge_job = store
        .submit_agent_job(
            parent.run.id.clone(),
            "needs-bridge".into(),
            JobRequest {
                requires_bridge: true,
                ..request()
            },
        )
        .await?;
    let portable = store
        .submit_agent_job(
            parent.run.id.clone(),
            "portable".into(),
            JobRequest {
                task_id: Some(task.id.clone()),
                ..request()
            },
        )
        .await?;
    let dependent = store
        .submit_agent_job(
            parent.run.id,
            "depends-on-portable".into(),
            JobRequest {
                dependencies: vec![portable.id.clone()],
                ..request()
            },
        )
        .await?;
    let read =
        |job: &Job| store.inspect_job(job.project_id.clone(), job.id.clone(), environment(false));
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    for job in [&bridge_job, &portable, &dependent] {
        approve(&store, job).await?;
    }
    assert_eq!(
        read(&bridge_job).await?.unwrap().blockers,
        [AdmissionBlocker::MissingCapability {
            capability: Capability::RunScopedMcp
        }]
    );
    assert!(read(&portable).await?.unwrap().blockers.is_empty());
    assert_eq!(
        read(&dependent).await?.unwrap().blockers,
        [AdmissionBlocker::Dependency {
            job_id: portable.id.clone(),
            status: None,
            queue_state: QueueState::Queued
        }]
    );
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let (first, second) = tokio::join!(
        store.claim_next_job(environment(false)),
        parallel.claim_next_job(environment(false))
    );
    let claimed = [first?, second?].into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(claimed.len(), 1);
    let plan = &claimed[0];
    assert_ne!(plan.conversation_id, parent.conversation_id);
    assert_eq!(
        read(&portable)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        plan.run.id
    );
    assert!(
        read(&dependent)
            .await?
            .unwrap()
            .blockers
            .contains(&AdmissionBlocker::Capacity {
                active: 1,
                limit: 1
            })
    );
    store.mark_agent_running(plan.run.id.clone()).await?;
    let grandchild = store
        .submit_agent_job(plan.run.id.clone(), "nested".into(), request())
        .await?;
    assert_eq!(grandchild.task_id, Some(task.id));
    assert_eq!(
        grandchild.origin,
        JobOrigin::Agent {
            run_id: plan.run.id.clone(),
            request_id: "nested".into()
        }
    );
    assert!(
        store
            .request_job_interrupt(parent.conversation_id, plan.run.id.clone())
            .await
            .is_err()
    );
    store
        .request_job_interrupt(plan.conversation_id.clone(), plan.run.id.clone())
        .await?;
    let stopping = read(&portable).await?.unwrap().job.latest_attempt.unwrap();
    assert!(stopping.cancel_requested);
    assert_eq!(stopping.status, RunStatus::Running);
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    // The provider can complete successfully before the cancellation takes effect.
    store
        .finish_agent_attempt(plan.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    assert!(
        store
            .finish_agent_attempt(plan.run.id.clone(), RunStatus::Cancelled, None)
            .await
            .is_err()
    );
    let complete = read(&portable).await?.unwrap().job.latest_attempt.unwrap();
    assert!(!complete.cancel_requested);
    assert_eq!(complete.status, RunStatus::Succeeded);
    assert!(read(&dependent).await?.unwrap().blockers.is_empty());
    let dependency_plan = store
        .claim_next_job(environment(false))
        .await?
        .context("dependent became eligible")?;
    assert_eq!(
        read(&dependent)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        dependency_plan.run.id
    );
    store
        .finish_agent_attempt(dependency_plan.run.id, RunStatus::Cancelled, None)
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let bridge_plan = store
        .claim_next_job(environment(true))
        .await?
        .context("bridge job remained blocked")?;
    assert_eq!(
        read(&bridge_job)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        bridge_plan.run.id
    );
    store
        .finish_agent_attempt(bridge_plan.run.id, RunStatus::Cancelled, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn an_occupied_managed_worktree_and_a_declined_approval_block_the_same_jobs_in_both_paths()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let root = temp.path().join("project");
    let parent = origin_with_mode(&store, &root, SessionMode::ApprovedWrite).await?;
    let git = |args: &[&str]| -> Result<String> {
        let output = Command::new("git.exe")
            .args(args)
            .current_dir(&root)
            .output()?;
        ensure!(
            output.status.success(),
            "fixture Git operation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().into())
    };
    git(&[
        "-c",
        "user.name=OmniAgent Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=.git/no-fixture-hooks",
        "commit",
        "--allow-empty",
        "--quiet",
        "-m",
        "Base",
    ])?;
    let worktree = temp.path().join("managed");
    let branch = "codex/queue-writer-fixture";
    git(&[
        "-c",
        "core.hooksPath=.git/no-fixture-hooks",
        "worktree",
        "add",
        "--quiet",
        "-b",
        branch,
        worktree.to_str().unwrap(),
    ])?;
    let worktree = repository::register(&store, worktree).await?;
    let task = store
        .create_project_task(worktree.project_id.clone(), "Fixture writer".into())
        .await?;
    // Register the real linked checkout as managed for this admission fixture.
    rusqlite::Connection::open(&database)?.execute(
        "INSERT INTO managed_task_worktrees VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            worktree.project_id,
            task.id,
            worktree.checkout_id,
            branch,
            git(&["rev-parse", "HEAD"])?
        ],
    )?;
    let writer = || JobRequest {
        checkout_id: Some(worktree.checkout_id.clone()),
        task_id: Some(task.id.clone()),
        mode: SessionMode::ApprovedWrite,
        ..request()
    };
    let first = store
        .submit_agent_job(parent.run.id.clone(), "first-writer".into(), writer())
        .await?;
    let second = store
        .submit_agent_job(parent.run.id.clone(), "second-writer".into(), writer())
        .await?;
    let declined = store
        .submit_agent_job(parent.run.id, "declined-writer".into(), writer())
        .await?;
    let inspect =
        |job: &Job| store.inspect_job(job.project_id.clone(), job.id.clone(), environment(false));
    let approval = inspect(&declined)
        .await?
        .unwrap()
        .blockers
        .into_iter()
        .find_map(|blocker| match blocker {
            AdmissionBlocker::Approval { approval_id } => Some(approval_id),
            _ => None,
        })
        .context("missing writer approval")?;
    store
        .decide_job_approval(declined.project_id.clone(), approval, false)
        .await?;
    for job in [&first, &second] {
        approve(&store, job).await?;
    }
    let claimed = store
        .claim_next_job(environment(false))
        .await?
        .context("first writer did not claim")?;
    let snapshot = store
        .inspect_project_queue(worktree.project_id.clone(), environment(false))
        .await?;
    assert_eq!(snapshot.active, 1);
    assert_eq!(snapshot.settings.capacity, 3);
    assert_eq!(snapshot.jobs.len(), 3);
    for (entry, job) in snapshot.jobs.iter().zip([&first, &second, &declined]) {
        assert_eq!(entry, &inspect(job).await?.unwrap());
    }
    assert_eq!(
        inspect(&first)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        claimed.run.id
    );
    assert_eq!(
        inspect(&second).await?.unwrap().blockers,
        [AdmissionBlocker::Writer {
            checkout_id: worktree.checkout_id.clone(),
            run_id: claimed.run.id.clone(),
            needs_review: false,
        }]
    );
    assert!(
        inspect(&declined)
            .await?
            .unwrap()
            .blockers
            .iter()
            .any(|blocker| matches!(blocker, AdmissionBlocker::Policy { .. }))
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    store
        .finish_agent_attempt(claimed.run.id, RunStatus::Cancelled, None)
        .await?;
    assert!(inspect(&second).await?.unwrap().blockers.is_empty());
    let next = store
        .claim_next_job(environment(false))
        .await?
        .context("writer did not yield slot")?;
    assert_eq!(
        inspect(&second)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        next.run.id
    );
    store.mark_agent_running(next.run.id.clone()).await?;
    store
        .finish_agent_attempt(next.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    assert!(inspect(&second).await?.unwrap().recovery_actions.is_empty());
    assert!(
        store
            .queue_job_recovery(
                second.project_id,
                second.id,
                next.run.id.clone(),
                RecoveryAction::Retry
            )
            .await
            .is_err()
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    assert_eq!(
        inspect(&declined).await?.unwrap().blockers,
        [
            AdmissionBlocker::Policy {
                reason: "An operator declined this job"
            },
            AdmissionBlocker::Writer {
                checkout_id: worktree.checkout_id,
                run_id: next.run.id,
                needs_review: true,
            },
        ]
    );
    Ok(())
}

async fn conversation_job(
    store: &Store,
    checkout: &Path,
) -> Result<(Conversation, Job, AttemptPlan)> {
    let parent = origin(store, checkout).await?;
    let chat = store
        .create_conversation(parent.run.checkout_id, "Recoverable job".into())
        .await?;
    store
        .save_conversation_draft(chat.id.clone(), request().instructions.clone())
        .await?;
    let job = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: chat.id.clone(),
                context_id: chat.context_id.clone(),
            },
            parent.scope,
            request().instructions,
        )
        .await?;
    let plan = store
        .claim_next_job(environment(false))
        .await?
        .context("missing first attempt")?;
    Ok((chat, job, plan))
}

#[tokio::test]
async fn restart_keeps_inflight_and_queued_recovery_distinct_then_links_only_one_retry()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let (conversation, job, first) = conversation_job(&store, &temp.path().join("project")).await?;
    store.mark_agent_running(first.run.id.clone()).await?;
    drop(store);

    let store = Store::open(database.clone())?;
    store.recover_agent_attempts_after_restart().await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let interrupted = store
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .unwrap();
    assert_eq!(
        interrupted.job.latest_attempt.unwrap().status,
        RunStatus::Interrupted
    );
    assert_eq!(interrupted.recovery_actions, [RecoveryAction::Retry]);
    store
        .set_job_queue_settings(QueueSettings {
            paused: true,
            capacity: 1,
        })
        .await?;
    let (one, two) = tokio::join!(
        store.queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            first.run.id.clone(),
            RecoveryAction::Retry
        ),
        store.queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            first.run.id.clone(),
            RecoveryAction::Retry
        ),
    );
    assert_eq!(one?, two?);
    drop(store);

    let reopened = Store::open(database.clone())?;
    reopened.recover_agent_attempts_after_restart().await?;
    let saved = reopened
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .unwrap();
    assert_eq!(saved.job.queued_recovery, Some(RecoveryAction::Retry));
    assert_eq!(saved.blockers, [AdmissionBlocker::QueuePaused]);
    assert!(reopened.claim_next_job(environment(false)).await?.is_none());
    reopened
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let competing_connection = Store::open(database.clone())?;
    let (one, two) = tokio::join!(
        reopened.claim_next_job(environment(false)),
        competing_connection.claim_next_job(environment(false))
    );
    let claims = [one?, two?].into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    let retry = &claims[0];
    assert_eq!(retry.conversation_id, conversation.id);
    assert_eq!(
        retry.previous_run_id.as_deref(),
        Some(first.run.id.as_str())
    );
    assert_eq!(retry.input, first.input);
    assert_eq!(retry.context_id, first.context_id);
    assert!(matches!(retry.intent, SessionIntent::Start));
    assert_ne!(retry.agent_session_id, first.agent_session_id);
    let record = reopened.job(job.project_id, job.id).await?.unwrap();
    assert!(record.queued_recovery.is_none());
    let latest = record.latest_attempt.unwrap();
    assert_eq!(latest.ordinal, 2);
    assert_eq!(
        latest.previous_run_id.as_deref(),
        Some(first.run.id.as_str())
    );
    assert_eq!(latest.recovery, Some(RecoveryAction::Retry));
    let db = rusqlite::Connection::open(database)?;
    let events: i64 = db.query_row("SELECT count(*) FROM run_events WHERE turn_id = ?1 AND json_extract(payload, '$.status') = 'interrupted'", [&first.run.turn_id], |row| row.get(0))?;
    assert_eq!(events, 1);
    reopened
        .finish_agent_attempt(retry.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn resume_uses_the_recorded_partial_result_and_blocks_when_the_context_changes() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("queue.sqlite3"))?;
    let (conversation, job, first) = conversation_job(&store, &temp.path().join("project")).await?;
    store.mark_agent_running(first.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            first.run.id.clone(),
            RunStatus::Failed,
            Some("Retained partial answer".into()),
        )
        .await?;
    let checkpoint = store
        .conversation(conversation.id.clone())
        .await?
        .context_id;
    store
        .queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            first.run.id.clone(),
            RecoveryAction::Resume,
        )
        .await?;
    let resumed = store
        .claim_next_job(environment(false))
        .await?
        .context("missing resume attempt")?;
    assert!(matches!(resumed.intent, SessionIntent::Start));
    assert_eq!(resumed.previous_run_id, Some(first.run.id));
    assert_eq!(resumed.input[0], first.input[0]);
    assert_eq!(resumed.input[1].text, "Retained partial answer");
    assert!(
        resumed.input[2]
            .text
            .starts_with("Continue the interrupted task")
    );
    store.mark_agent_running(resumed.run.id.clone()).await?;
    store
        .finish_agent_attempt(resumed.run.id.clone(), RunStatus::Failed, None)
        .await?;
    store
        .queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            resumed.run.id,
            RecoveryAction::Retry,
        )
        .await?;
    store
        .replace_conversation_message(
            conversation.id,
            resumed.context_id.clone(),
            first.input[0].message_id.clone(),
            "Edited after queueing".into(),
        )
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let blocked = store
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .unwrap();
    assert_eq!(
        blocked.blockers,
        [AdmissionBlocker::ContextChanged {
            context_id: resumed.context_id
        }]
    );
    store
        .set_pending_job_state(job.project_id, job.id, QueueState::Cancelled)
        .await?;
    let db = rusqlite::Connection::open(temp.path().join("queue.sqlite3"))?;
    let saved: String = db.query_row(
        "SELECT result_context_id FROM agent_attempts WHERE run_id = ?1",
        [resumed.previous_run_id.unwrap()],
        |row| row.get(0),
    )?;
    assert_eq!(saved, checkpoint);
    Ok(())
}
