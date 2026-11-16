use std::{fs, path::Path, process::Command};

use super::*;
use crate::{repository, workflows::DeliveryRoles};

async fn register(store: &Store, root: &Path) -> Result<String> {
    fs::create_dir(root)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()?
            .success()
    );
    Ok(repository::register(store, root.to_path_buf())
        .await?
        .project_id)
}

fn pipeline(event: &ProjectWorkEvent) -> &AgilePipelineOrigin {
    event
        .origin
        .pipeline()
        .expect("pipeline origin must be retained")
}

#[tokio::test]
async fn scoped_events_preserve_automation_chain_and_receipts_after_restart() -> Result<()> {
    use TaskWorkflowState::*;
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("events.sqlite");
    let store = Store::open(database.clone())?;
    let project = register(&store, &temp.path().join("project")).await?;
    let other = register(&store, &temp.path().join("other")).await?;
    let parent = store
        .create_project_task(project.clone(), "Delivery".into())
        .await?;
    let foreign = store
        .create_project_task(other.clone(), "Foreign".into())
        .await?;
    let created = store
        .project_work_events(project.clone(), None, 500)
        .await?;
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].task_id, parent.id);
    assert_eq!(
        created[0].data,
        AgileEventData::Created {
            placement: TaskPlacement::Task { epic_id: None }
        }
    );
    assert_eq!(created[0].origin, AgileEventOrigin::Operator);

    let first = store
        .start_builtin_delivery(project.clone(), DeliveryRoles::default())
        .await?;
    let loop_candidate = store
        .start_builtin_delivery(project.clone(), DeliveryRoles::default())
        .await?;
    let origin = store
        .bind_pipeline_work_event_origin(project.clone(), first.id.clone(), created[0].id.clone())
        .await?;
    assert_eq!(origin.automation_chain[0].pipeline_id, first.version.id);
    assert_eq!(
        store
            .bind_pipeline_work_event_origin(
                project.clone(),
                first.id.clone(),
                created[0].id.clone()
            )
            .await?,
        origin
    );
    let child = store
        .create_pipeline_work(
            project.clone(),
            first.id.clone(),
            "Checklist".into(),
            TaskPlacement::Subtask {
                parent_task_id: parent.id.clone(),
            },
        )
        .await?;
    assert!(
        store
            .create_pipeline_work(
                project.clone(),
                first.id.clone(),
                "Foreign child".into(),
                TaskPlacement::Subtask {
                    parent_task_id: foreign.id
                }
            )
            .await
            .is_err()
    );
    let child_event = store
        .project_work_events(project.clone(), Some(created[0].sequence.parse()?), 1)
        .await?
        .pop()
        .context("missing child creation")?;
    assert_eq!(
        child_event.data,
        AgileEventData::Created {
            placement: TaskPlacement::Subtask {
                parent_task_id: parent.id.clone()
            }
        }
    );
    assert_eq!(pipeline(&child_event), &origin);
    assert!(
        store
            .bind_pipeline_work_event_origin(
                project.clone(),
                loop_candidate.id,
                child_event.id.clone()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .bind_pipeline_work_event_origin(
                other.clone(),
                first.id.clone(),
                child_event.id.clone()
            )
            .await
            .is_err()
    );

    let follow_up = "another-pipeline";
    store
        .register_job_pipeline(project.clone(), follow_up.into())
        .await?;
    let follow_origin = store
        .bind_pipeline_work_event_origin(project.clone(), follow_up.into(), child_event.id.clone())
        .await?;
    assert_eq!(
        follow_origin.automation_chain,
        [
            origin.automation_chain[0].clone(),
            AgilePipelineLink {
                pipeline_id: follow_up.into(),
                run_id: follow_up.into()
            }
        ]
    );
    store
        .ensure_project_checklist(project.clone(), child.id.clone())
        .await?;
    let item = store
        .append_project_checklist_item(
            project.clone(),
            child.id.clone(),
            "Confirm the evidence".into(),
        )
        .await?;
    let checked = store
        .set_pipeline_checklist_item_checked(
            project.clone(),
            follow_up.into(),
            child.id.clone(),
            item.id.clone(),
            item.revision,
            true,
        )
        .await?;
    assert!(
        store
            .set_pipeline_checklist_item_checked(
                project.clone(),
                follow_up.into(),
                child.id.clone(),
                item.id.clone(),
                item.revision,
                false
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .set_pipeline_checklist_item_checked(
                project.clone(),
                follow_up.into(),
                child.id.clone(),
                item.id.clone(),
                checked.revision,
                true
            )
            .await?,
        checked
    );
    let reopened = store
        .set_project_checklist_item_checked(
            project.clone(),
            child.id.clone(),
            item.id.clone(),
            checked.revision,
            false,
        )
        .await?;
    let before_reject = store
        .project_work_events(project.clone(), None, 500)
        .await?;
    assert!(
        store
            .transition_pipeline_work(
                project.clone(),
                follow_up.into(),
                child.id.clone(),
                Backlog,
                Completed
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .project_work_events(project.clone(), None, 500)
            .await?,
        before_reject
    );

    store
        .set_project_checklist_item_checked(
            project.clone(),
            child.id.clone(),
            item.id.clone(),
            reopened.revision,
            true,
        )
        .await?;
    store
        .transition_pipeline_work(
            project.clone(),
            follow_up.into(),
            child.id.clone(),
            Backlog,
            Completed,
        )
        .await?;
    store
        .reopen_task_delivery(project.clone(), child.id.clone())
        .await?;
    let events = store
        .project_work_events(project.clone(), None, 500)
        .await?;
    assert_eq!(events.len(), 9);
    assert_eq!(
        events[2].data,
        AgileEventData::ChecklistChecked {
            item_id: item.id.clone(),
            revision: checked.revision
        }
    );
    assert_eq!(pipeline(&events[2]), &follow_origin);
    assert_eq!(
        events[3].data,
        AgileEventData::ChecklistReopened {
            item_id: item.id,
            revision: reopened.revision
        }
    );
    assert_eq!(events[3].origin, AgileEventOrigin::Operator);
    let completed = events
        .iter()
        .filter(|event| {
            matches!(
                event.data,
                AgileEventData::StateChanged {
                    previous: Backlog,
                    current: Completed
                }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 2);
    assert!(completed.iter().any(|event| event.task_id == parent.id));
    assert!(completed.iter().any(|event| event.task_id == child.id));
    assert!(
        completed
            .iter()
            .all(|event| pipeline(event) == &follow_origin)
    );
    assert_eq!(
        events
            .iter()
            .map(|event| &event.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        events.len()
    );
    assert!(!serde_json::to_string(&events)?.contains("Confirm the evidence"));

    drop(store);
    let store = Store::open(database)?;
    assert_eq!(
        store
            .project_work_events(project.clone(), None, 500)
            .await?,
        events
    );
    assert_eq!(
        store
            .project_work_events(project, Some(events[4].sequence.parse()?), 2)
            .await?,
        events[5..7]
    );
    assert_eq!(store.project_work_events(other, None, 500).await?.len(), 1);
    Ok(())
}
