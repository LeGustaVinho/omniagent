use std::{path::Path, process::Command};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    jobs::JobRequest,
    providers::{Provider, SessionMode},
    repository::{self, provision_task_worktree},
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
    tasks::{ProjectWork, TaskPlacement},
};

fn git(checkout: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .current_dir(checkout)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn legacy_task_ids_jobs_and_worktrees_survive_the_hierarchy_migration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    git(&source, &["init", "--quiet"])?;
    git(
        &source,
        &[
            "-c",
            "user.name=OmniAgent Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "Base",
        ],
    )?;
    let database = temp.path().join("omniagent.sqlite3");
    let store = Store::open(database.clone())?;
    let project = repository::register(&store, source.clone()).await?;
    let project_id = project.project_id.clone();
    let legacy_task = store
        .create_project_task(project_id.clone(), "Existing task".into())
        .await?;
    let worktree_root = temp.path().join("worktrees");
    let worktree = provision_task_worktree(
        &store,
        worktree_root.clone(),
        project_id.clone(),
        legacy_task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    let conversation = store
        .create_conversation(project.checkout_id.clone(), "Legacy fixture".into())
        .await?;
    let parent = store
        .prepare_agent_attempt(
            conversation.id,
            conversation.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "fixture".into(),
                account_binding: None,
                checkout: dunce::canonicalize(source)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: "fixture-read-only".into(),
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            },
            "Legacy job origin".into(),
        )
        .await?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    let legacy_job = store
        .submit_agent_job(
            parent.run.id.clone(),
            "legacy-request".into(),
            JobRequest {
                checkout_id: Some(worktree.checkout_id.clone()),
                task_id: Some(legacy_task.id.clone()),
                mode: SessionMode::ReadOnly,
                instructions: "Inspect the managed checkout".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
        )
        .await?;
    store
        .finish_agent_attempt(parent.run.id, RunStatus::Succeeded, None)
        .await?;
    drop(store);

    let legacy = rusqlite::Connection::open(&database)?;
    legacy.execute_batch(
        "DROP TABLE project_checklists;
         DROP TABLE project_task_memberships;
         DROP TABLE project_epics;
         PRAGMA user_version = 51;",
    )?;
    drop(legacy);

    let store = Store::open(database)?;
    let project_view = store.project_work_hierarchy(project_id.clone()).await?;
    assert!(project_view.epics.is_empty());
    assert_eq!(
        project_view.work,
        [ProjectWork {
            task: legacy_task.clone(),
            placement: TaskPlacement::Task { epic_id: None },
            checklist_id: None,
        }]
    );
    assert_eq!(
        store.job(project_id.clone(), legacy_job.id.clone()).await?,
        Some(legacy_job)
    );
    assert_eq!(
        provision_task_worktree(
            &store,
            worktree_root.clone(),
            project_id.clone(),
            legacy_task.id.clone(),
            project.checkout_id.clone()
        )
        .await?,
        worktree,
    );

    let epic = store
        .create_project_epic(project_id.clone(), "Milestone".into())
        .await?;
    store
        .set_project_task_epic(
            project_id.clone(),
            legacy_task.id.clone(),
            Some(epic.id.clone()),
        )
        .await?;
    let child = store
        .create_project_subtask(project_id.clone(), legacy_task.id.clone(), "Child".into())
        .await?;
    let root_checklist = store
        .ensure_project_checklist(project_id.clone(), legacy_task.id.clone())
        .await?;
    let child_checklist = store
        .ensure_project_checklist(project_id.clone(), child.id.clone())
        .await?;
    assert_ne!(root_checklist, child_checklist);
    assert_eq!(
        store
            .ensure_project_checklist(project_id.clone(), child.id.clone())
            .await?,
        child_checklist
    );

    let other = temp.path().join("other");
    std::fs::create_dir(&other)?;
    git(&other, &["init", "--quiet"])?;
    let other_project = repository::register(&store, other).await?.project_id;
    let foreign_epic = store
        .create_project_epic(other_project.clone(), "Foreign epic".into())
        .await?;
    assert!(
        store
            .set_project_task_epic(
                project_id.clone(),
                legacy_task.id.clone(),
                Some(foreign_epic.id)
            )
            .await
            .is_err()
    );
    assert!(
        store
            .set_project_task_epic(project_id.clone(), child.id.clone(), Some(epic.id.clone()))
            .await
            .is_err()
    );
    assert!(
        store
            .create_project_subtask(
                other_project.clone(),
                legacy_task.id.clone(),
                "Cross-project child".into()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .create_project_subtask(project_id.clone(), child.id.clone(), "Grandchild".into())
            .await
            .is_err()
    );
    assert!(
        store
            .create_project_subtask(project_id.clone(), epic.id.clone(), "Wrong parent".into())
            .await
            .is_err()
    );
    assert!(
        store
            .ensure_project_checklist(project_id.clone(), epic.id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .ensure_project_checklist(other_project.clone(), child.id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .project_work_hierarchy(other_project)
            .await?
            .work
            .is_empty()
    );

    let hierarchy = store.project_work_hierarchy(project_id.clone()).await?;
    assert_eq!(hierarchy.epics.as_slice(), [&epic].map(Clone::clone));
    assert_eq!(
        hierarchy.work,
        [
            ProjectWork {
                task: child.clone(),
                placement: TaskPlacement::Subtask {
                    parent_task_id: legacy_task.id.clone()
                },
                checklist_id: Some(child_checklist),
            },
            ProjectWork {
                task: legacy_task.clone(),
                placement: TaskPlacement::Task {
                    epic_id: Some(epic.id)
                },
                checklist_id: Some(root_checklist),
            },
        ]
    );
    assert_eq!(
        store
            .project_task(project_id.clone(), child.id.clone())
            .await?,
        Some(child.clone())
    );
    let child_worktree = provision_task_worktree(
        &store,
        worktree_root,
        project_id.clone(),
        child.id,
        project.checkout_id,
    )
    .await
    .context("sub-task should be a worktree scope")?;
    assert_ne!(child_worktree.checkout_id, worktree.checkout_id);

    store
        .set_project_task_epic(project_id.clone(), legacy_task.id, None)
        .await?;
    let ungrouped = store.project_work_hierarchy(project_id).await?;
    assert_eq!(ungrouped.work[0].placement, hierarchy.work[0].placement);
    assert_eq!(
        ungrouped.work[1].placement,
        TaskPlacement::Task { epic_id: None }
    );
    Ok(())
}
