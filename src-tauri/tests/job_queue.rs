use std::{fs, path::Path, process::Command};

use anyhow::{Result, ensure};
use omniagent::{
    conversations::{AttemptPlan, SessionScope},
    jobs::{JobOrigin, JobRequest, JobRequestConflict, QueueState},
    providers::{Provider, SessionMode},
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
};

async fn origin(store: &Store, path: &Path) -> Result<AttemptPlan> {
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
                mode: SessionMode::ReadOnly,
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
