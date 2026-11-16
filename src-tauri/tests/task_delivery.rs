use std::{path::Path, process::Command};

use anyhow::{Context, Result, ensure};
use omniagent::{
    repository,
    store::Store,
    tasks::{TaskDeliveryEvidence, TaskDetails, TaskWorkflowState},
};

async fn project(store: &Store, repository_path: &Path) -> Result<String> {
    std::fs::create_dir(repository_path)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(repository_path)
            .status()?
            .success()
    );
    Ok(repository::register(store, repository_path.into())
        .await?
        .project_id)
}

async fn task_state(store: &Store, project: &str, task: &str) -> Result<TaskWorkflowState> {
    Ok(store
        .project_task_delivery_state(project.into(), task.into())
        .await?
        .context("missing task delivery state")?
        .state)
}

#[tokio::test]
async fn manual_delivery_resolves_checklist_and_reopening_recalculates_ancestors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("delivery.sqlite");
    let store = Store::open(database.clone())?;
    let project = project(&store, &temp.path().join("repo")).await?;
    let epic = store
        .create_project_epic(project.clone(), "Release".into())
        .await?;
    let parent = store
        .create_project_task(project.clone(), "Group".into())
        .await?;
    store
        .set_project_task_epic(project.clone(), parent.id.clone(), Some(epic.id.clone()))
        .await?;
    let work = store
        .create_project_subtask(
            project.clone(),
            parent.id.clone(),
            "Document the change".into(),
        )
        .await?;
    let skipped = store
        .create_project_subtask(
            project.clone(),
            parent.id.clone(),
            "Retire old instructions".into(),
        )
        .await?;
    store
        .ensure_project_checklist(project.clone(), work.id.clone())
        .await?;
    let item = store
        .append_project_checklist_item(project.clone(), work.id.clone(), "Review the draft".into())
        .await?;

    assert!(
        store
            .set_project_task_details(
                project.clone(),
                work.id.clone(),
                TaskDetails {
                    workflow_state: TaskWorkflowState::Completed,
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert!(
        store
            .record_manual_task_delivery(project.clone(), work.id.clone(), None)
            .await
            .is_err()
    );
    store
        .cancel_task_delivery(project.clone(), skipped.id)
        .await?;
    assert_eq!(
        task_state(&store, &project, &parent.id).await?,
        TaskWorkflowState::Backlog
    );

    store
        .revise_project_checklist_item(
            project.clone(),
            work.id.clone(),
            item.id,
            item.revision,
            item.body,
            true,
        )
        .await?;
    let delivery = store
        .record_manual_task_delivery(
            project.clone(),
            work.id.clone(),
            Some("Reviewed by the operator".into()),
        )
        .await?;
    assert_eq!(
        delivery.evidence,
        TaskDeliveryEvidence::Manual {
            note: Some("Reviewed by the operator".into())
        }
    );
    assert_eq!(
        task_state(&store, &project, &work.id).await?,
        TaskWorkflowState::Completed
    );
    assert_eq!(
        task_state(&store, &project, &parent.id).await?,
        TaskWorkflowState::Completed
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Completed)
    );

    store
        .reopen_task_delivery(project.clone(), work.id.clone())
        .await?;
    drop(store);
    let store = Store::open(database)?;
    assert_ne!(
        task_state(&store, &project, &work.id).await?,
        TaskWorkflowState::Completed
    );
    assert_eq!(
        task_state(&store, &project, &parent.id).await?,
        TaskWorkflowState::Backlog
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id)
            .await?,
        Some(TaskWorkflowState::Backlog)
    );
    assert_eq!(
        store.project_task_deliveries(project, work.id).await?,
        [delivery]
    );
    Ok(())
}

#[tokio::test]
async fn empty_and_cancelled_groups_and_tasks_with_direct_work_have_distinct_states() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("delivery.sqlite"))?;
    let project = project(&store, &temp.path().join("repo")).await?;
    let epic = store
        .create_project_epic(project.clone(), "Release".into())
        .await?;
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(TaskWorkflowState::Backlog)
    );
    let group = store
        .create_project_task(project.clone(), "Group only".into())
        .await?;
    store
        .set_project_task_epic(project.clone(), group.id.clone(), Some(epic.id.clone()))
        .await?;
    assert_eq!(
        task_state(&store, &project, &group.id).await?,
        TaskWorkflowState::Backlog
    );
    let cancelled = store
        .create_project_subtask(project.clone(), group.id.clone(), "Skip".into())
        .await?;
    store
        .cancel_task_delivery(project.clone(), cancelled.id)
        .await?;
    assert_eq!(
        task_state(&store, &project, &group.id).await?,
        TaskWorkflowState::Cancelled
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id)
            .await?,
        Some(TaskWorkflowState::Cancelled)
    );

    let parent = store
        .create_project_task(project.clone(), "Implement and document".into())
        .await?;
    store
        .ensure_project_checklist(project.clone(), parent.id.clone())
        .await?;
    let child = store
        .create_project_subtask(project.clone(), parent.id.clone(), "Test".into())
        .await?;
    store
        .record_manual_task_delivery(project.clone(), parent.id.clone(), None)
        .await?;
    assert_eq!(
        task_state(&store, &project, &parent.id).await?,
        TaskWorkflowState::InProgress
    );
    store
        .cancel_task_delivery(project.clone(), child.id)
        .await?;
    assert_eq!(
        task_state(&store, &project, &parent.id).await?,
        TaskWorkflowState::Completed
    );
    Ok(())
}
