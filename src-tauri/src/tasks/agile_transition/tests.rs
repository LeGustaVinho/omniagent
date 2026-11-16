use std::{fs, process::Command};

use anyhow::Context;

use super::*;
use crate::{
    repository,
    tasks::{TaskDetails, TaskWorkflowState::*},
};

async fn project() -> Result<(tempfile::TempDir, Store, String)> {
    let temp = tempfile::tempdir()?;
    let checkout = temp.path().join("repo");
    fs::create_dir(&checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    let store = Store::open(temp.path().join("profile.sqlite"))?;
    let registered = repository::register(&store, checkout).await?;
    Ok((temp, store, registered.project_id))
}

#[tokio::test]
async fn concurrent_moves_compare_the_effective_state_and_keep_task_fields() -> Result<()> {
    let (_temp, store, project) = project().await?;
    let task = store
        .create_project_task(project.clone(), "Check release".into())
        .await?;
    let original = TaskDetails {
        priority: 20,
        acceptance_criteria: vec!["Retain the candidate".into()],
        ..Default::default()
    };
    store
        .set_project_task_details(project.clone(), task.id.clone(), original.clone())
        .await?;

    let (ready, failed) = tokio::join!(
        store.transition_project_work(project.clone(), task.id.clone(), Backlog, Ready),
        store.transition_project_work(project.clone(), task.id.clone(), Backlog, Failed),
    );
    let (changed, stale) = match (ready, failed) {
        (Ok(changed), Err(stale)) | (Err(stale), Ok(changed)) => (changed, stale),
        other => anyhow::bail!("Expected one winning transition: {other:?}"),
    };
    assert!(stale.to_string().contains("work state changed"));
    let details = store.project_task_details(project, task.id).await?.unwrap();
    assert_eq!(
        details,
        TaskDetails {
            workflow_state: changed.state,
            ..original
        }
    );
    Ok(())
}

#[tokio::test]
async fn dependency_and_checklist_guards_preserve_parent_rollups_and_delivery_history() -> Result<()>
{
    let (_temp, store, project) = project().await?;
    let epic = store
        .create_project_epic(project.clone(), "Release".into())
        .await?;
    let parent = store
        .create_project_task_in_epic(project.clone(), epic.id.clone(), "Notes".into())
        .await?;
    let child = store
        .create_project_subtask(project.clone(), parent.id.clone(), "Publish".into())
        .await?;
    let dependency = store
        .create_project_task(project.clone(), "Approve".into())
        .await?;
    store
        .set_project_task_details(
            project.clone(),
            child.id.clone(),
            TaskDetails {
                prerequisites: vec![dependency.id.clone()],
                ..Default::default()
            },
        )
        .await?;
    let blocked = store
        .project_task_delivery_state(project.clone(), child.id.clone())
        .await?
        .unwrap();
    assert_eq!(blocked.state, Blocked);
    assert_eq!(blocked.prerequisite_blockers[0].task_id, dependency.id);
    assert_eq!(blocked.prerequisite_blockers[0].title, "Approve");
    let error = store
        .transition_project_work(project.clone(), child.id.clone(), Blocked, InProgress)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("dependencies"));
    assert!(
        store
            .record_manual_task_delivery(project.clone(), child.id.clone(), None)
            .await
            .is_err()
    );
    store
        .transition_project_work(project.clone(), dependency.id, Backlog, Completed)
        .await?;
    let started = store
        .transition_project_work(project.clone(), child.id.clone(), Backlog, InProgress)
        .await?;
    assert!(started.prerequisite_blockers.is_empty());

    store
        .ensure_project_checklist(project.clone(), child.id.clone())
        .await?;
    let item = store
        .append_project_checklist_item(project.clone(), child.id.clone(), "Review notes".into())
        .await?;
    let error = store
        .transition_project_work(project.clone(), child.id.clone(), InProgress, Completed)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("checklist"));
    assert!(
        store
            .project_task_deliveries(project.clone(), child.id.clone())
            .await?
            .is_empty()
    );
    store
        .revise_project_checklist_item(
            project.clone(),
            child.id.clone(),
            item.id,
            item.revision,
            item.body,
            true,
        )
        .await?;
    let done = store
        .transition_project_work(project.clone(), child.id.clone(), InProgress, Completed)
        .await?;
    let receipt = done
        .completion
        .context("completed child should have a receipt")?;
    assert_eq!(
        store
            .project_task_delivery_state(project.clone(), parent.id.clone())
            .await?
            .unwrap()
            .state,
        Completed
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id.clone())
            .await?,
        Some(Completed)
    );

    assert!(
        store
            .transition_project_work(project.clone(), parent.id.clone(), Completed, Ready)
            .await
            .is_err()
    );
    assert!(
        store
            .transition_project_work(project.clone(), child.id.clone(), Completed, InProgress)
            .await
            .is_err()
    );
    store
        .transition_project_work(project.clone(), child.id.clone(), Completed, Ready)
        .await?;
    assert_eq!(
        store
            .project_task_delivery_state(project.clone(), parent.id)
            .await?
            .unwrap()
            .state,
        Backlog
    );
    assert_eq!(
        store
            .project_epic_delivery_state(project.clone(), epic.id)
            .await?,
        Some(Backlog)
    );
    assert_eq!(
        store.project_task_deliveries(project, child.id).await?,
        [receipt]
    );
    Ok(())
}
