use std::path::Path;

use crate::{
    repository::ProjectCheckout,
    tasks::{AgileEventData, AgileEventOrigin, ChecklistItem, TaskWorkflowState::*},
    workflows::{
        BuiltinNode, Endpoint, NodeInstance, Wire, WorkflowGraph, WorkflowHostRequest,
        WorkflowHostSubmission, WorkflowLaunchSource,
        tests::approved_delivery_repository,
        triggers::{TriggerWorkKind, WorkflowTriggerEvent, WorkflowTriggerSpec},
    },
};

use super::*;

fn wire(from: (&str, &str), to: (&str, &str)) -> Wire {
    Wire {
        from: Endpoint {
            node: from.0.into(),
            port: from.1.into(),
        },
        to: Endpoint {
            node: to.0.into(),
            port: to.1.into(),
        },
    }
}

async fn start_progress_fixture(store: &Store, repo: &ProjectCheckout) -> Result<WorkflowRun> {
    let graph = WorkflowGraph {
        name: "Review checklist before handoff".into(),
        nodes: vec![
            NodeInstance {
                id: "start".into(),
                kind: BuiltinNode::Start,
                configuration: NodeConfiguration::None,
            },
            NodeInstance {
                id: "find".into(),
                kind: BuiltinNode::FindWork,
                configuration: NodeConfiguration::FindWork {
                    query: FindWorkConfiguration {
                        kind: FindWorkKind::Checklist,
                        result: FindWorkResultMode::Single,
                        query: Some("conexão tipada".into()),
                        checked: Some(false),
                        ..Default::default()
                    },
                },
            },
            NodeInstance {
                id: "update".into(),
                kind: BuiltinNode::UpdateProgress,
                configuration: NodeConfiguration::UpdateProgress {
                    action: UpdateProgressConfiguration::Checklist { checked: true },
                },
            },
        ],
        wires: vec![
            wire(("start", "start"), ("find", "ready")),
            wire(("find", "completed"), ("update", "ready")),
            wire(("find", "target"), ("update", "target")),
        ],
    };
    let document = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let version = store
        .save_workflow_revision(repo.project_id.clone(), "progress-fixture".into(), graph)
        .await?;
    let run = store
        .start_workflow_run(repo.project_id.clone(), version)
        .await?;
    store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: repo.project_id.clone(),
            run_id: run.id.clone(),
            node_id: "start".into(),
            inputs: Default::default(),
            request: WorkflowHostRequest::Start {
                document: document.revision,
            },
        })
        .await?;
    Ok(run)
}

async fn checklist_fixture(store: &Store, project: &str) -> Result<(String, ChecklistItem)> {
    let task = store
        .create_project_task(project.into(), "Exercise the workflow".into())
        .await?;
    store
        .ensure_project_checklist(project.into(), task.id.clone())
        .await?;
    store
        .append_project_checklist_item(
            project.into(),
            task.id.clone(),
            "Keep the worktree clean".into(),
        )
        .await?;
    let selected = store
        .append_project_checklist_item(project.into(), task.id.clone(), "Conexão tipada".into())
        .await?;
    Ok((task.id, selected))
}

#[tokio::test]
async fn inspection_cannot_apply_the_pending_checklist_change_and_restart_settles_it_once()
-> Result<()> {
    let suite = tempfile::tempdir()?;
    let database = suite.path().join("agile-stage.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, suite.path()).await?;
    let project = repo.project_id.clone();
    let (task, selected) = checklist_fixture(&store, &project).await?;
    let run = start_progress_fixture(&store, &repo).await?;

    for _ in 0..2 {
        let inspected = store
            .inspect_workflow_execution(project.clone(), run.id.clone())
            .await?
            .context("The staged workflow is missing")?;
        assert!(
            inspected
                .outputs
                .iter()
                .all(|output| output.artifact.node_instance_id != "update")
        );
        let list = store
            .project_checklist(project.clone(), task.clone())
            .await?
            .unwrap();
        assert_eq!(list.items[1], selected);
    }
    drop(store);

    let reopened = Store::open(database.clone())?;
    reopened.set_job_queue_paused(true).await?;
    assert_eq!(reopened.settle_ready_agile_stages().await?, 0);
    reopened.set_job_queue_paused(false).await?;
    assert_eq!(reopened.settle_ready_agile_stages().await?, 1);
    let inspected = reopened
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    let output = inspected
        .outputs
        .iter()
        .find(|entry| entry.artifact.port == "progress")
        .context("Update progress did not publish its receipt")?;
    let ArtifactValue::AgileProgressReceipt(receipt) = &output.artifact.value else {
        bail!("Expected a typed Agile receipt");
    };
    assert!(receipt.changed && output.current);
    assert_eq!(output.input_receipts.len(), 1);
    assert!(output.input_receipts[0].inputs.contains_key("target"));
    assert_eq!(
        receipt.previous,
        AgileSelection::Checklist {
            item: ChecklistProgressReceipt::from(&selected)
        }
    );
    let mut revised = selected.clone();
    revised.checked = true;
    revised.revision += 1;
    assert_eq!(
        receipt.target.selection,
        AgileSelection::Checklist {
            item: ChecklistProgressReceipt::from(&revised)
        }
    );
    let events = reopened
        .project_work_events(project.clone(), None, 20)
        .await?;
    let assigned = events.iter().filter(|event| matches!(&event.data, AgileEventData::ChecklistChecked { item_id, .. } if *item_id == selected.id)).collect::<Vec<_>>();
    assert_eq!(assigned.len(), 1);
    assert_eq!(receipt.event_ids, vec![assigned[0].id.clone()]);
    assert!(
        matches!(&assigned[0].origin, AgileEventOrigin::Pipeline { context } if context.run_id == run.id)
    );
    drop(reopened);

    let reopened = Store::open(database)?;
    assert_eq!(reopened.settle_ready_agile_stages().await?, 0);
    assert_eq!(
        reopened.project_work_events(project, None, 20).await?,
        events
    );
    Ok(())
}

#[tokio::test]
async fn update_progress_does_not_overwrite_a_checklist_changed_before_settlement() -> Result<()> {
    let suite = tempfile::tempdir()?;
    let store = Store::open(suite.path().join("stale-agile-stage.sqlite"))?;
    let repo = approved_delivery_repository(&store, suite.path()).await?;
    let project = repo.project_id.clone();
    let (task, selected) = checklist_fixture(&store, &project).await?;
    let run = start_progress_fixture(&store, &repo).await?;
    let changed = store
        .set_project_checklist_item_checked(
            project.clone(),
            task.clone(),
            selected.id.clone(),
            selected.revision,
            true,
        )
        .await?;
    let newer = store
        .set_project_checklist_item_checked(
            project.clone(),
            task.clone(),
            selected.id.clone(),
            changed.revision,
            false,
        )
        .await?;
    let wanted_events = store.project_work_events(project.clone(), None, 20).await?;

    assert_eq!(store.settle_ready_agile_stages().await?, 1);
    let inspected = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    assert!(
        inspected
            .outputs
            .iter()
            .all(|output| output.artifact.node_instance_id != "update")
    );
    let checklist = store
        .project_checklist(project.clone(), task)
        .await?
        .unwrap();
    assert_eq!(ChecklistProgressReceipt::from(&checklist.items[1]), newer);
    assert_eq!(
        store.project_work_events(project, None, 20).await?,
        wanted_events
    );
    Ok(())
}

#[tokio::test]
async fn state_transition_rejects_an_old_snapshot_after_returning_to_the_same_state() -> Result<()>
{
    let suite = tempfile::tempdir()?;
    let store = Store::open(suite.path().join("work-state-agile.sqlite"))?;
    let repo = approved_delivery_repository(&store, suite.path()).await?;
    let project = repo.project_id;
    let task = store
        .create_project_task(project.clone(), "Document the new release".into())
        .await?;
    let pinned = store
        .execute({
            let (project, task) = (project.clone(), task.id.clone());
            move |db| task_target(db, &project, &task)
        })
        .await?;
    store
        .transition_project_work(project.clone(), task.id.clone(), Backlog, Ready)
        .await?;
    store
        .transition_project_work(project.clone(), task.id.clone(), Ready, Backlog)
        .await?;
    let error = store
        .execute(move |db| {
            update_progress(
                db,
                &project,
                "unused-stale-run",
                &pinned,
                &UpdateProgressConfiguration::Transition { next: Ready },
            )
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("work state changed"));
    Ok(())
}

#[tokio::test]
async fn create_subtask_settles_after_its_trigger_origin_and_skips_reactivation() -> Result<()> {
    let suite = tempfile::tempdir()?;
    let store = Store::open(suite.path().join("manage-work.sqlite"))?;
    let repo = approved_delivery_repository(&store, suite.path()).await?;
    let project = repo.project_id.clone();
    let graph = WorkflowGraph {
        name: "Create a review sub-task".into(),
        nodes: vec![
            NodeInstance {
                id: "start".into(),
                kind: BuiltinNode::Start,
                configuration: NodeConfiguration::None,
            },
            NodeInstance {
                id: "find".into(),
                kind: BuiltinNode::FindWork,
                configuration: NodeConfiguration::FindWork {
                    query: FindWorkConfiguration {
                        kind: FindWorkKind::Task,
                        result: FindWorkResultMode::Single,
                        query: Some("Release docs".into()),
                        ..Default::default()
                    },
                },
            },
            NodeInstance {
                id: "title".into(),
                kind: BuiltinNode::BuildText,
                configuration: serde_json::from_value(serde_json::json!({
                    "kind": "build_text", "template": "Review the released documentation"
                }))?,
            },
            NodeInstance {
                id: "create".into(),
                kind: BuiltinNode::ManageWork,
                configuration: NodeConfiguration::ManageWork {
                    action: ManageWorkConfiguration::CreateSubtask,
                },
            },
        ],
        wires: vec![
            wire(("start", "start"), ("find", "ready")),
            wire(("start", "start"), ("title", "ready")),
            wire(("find", "completed"), ("create", "ready")),
            wire(("find", "task"), ("create", "task")),
            wire(("title", "text"), ("create", "title")),
        ],
    };
    let version = store
        .save_workflow_revision(project.clone(), "review-subtask".into(), graph)
        .await?;
    let document = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    store
        .save_workflow_trigger_rule(
            project.clone(),
            None,
            None,
            WorkflowTriggerSpec {
                name: "Create review work".into(),
                enabled: true,
                event: WorkflowTriggerEvent::Created,
                work: TriggerWorkKind::All,
                target_state: None,
                pipeline: version,
                source: WorkflowLaunchSource {
                    checkout_id: repo.checkout_id,
                    revision_id: document.revision.id,
                }.into(),
            },
        )
        .await?;
    let parent = store
        .create_project_task(project.clone(), "Release docs".into())
        .await?;
    assert_eq!(
        store
            .project_work_hierarchy(project.clone())
            .await?
            .work
            .len(),
        1
    );
    let activations = store.workflow_trigger_activations(project.clone()).await?;
    let launch = activations
        .first()
        .context("The Task did not start the rule")?;
    let run_id = launch
        .run_id
        .as_ref()
        .context("The rule did not create a workflow run")?;
    assert_eq!(store.settle_ready_agile_stages().await?, 1);
    assert_eq!(store.settle_ready_agile_stages().await?, 0);
    let hierarchy = store.project_work_hierarchy(project.clone()).await?;
    let child = hierarchy.work.iter().find(|work| matches!(&work.placement, TaskPlacement::Subtask { parent_task_id } if *parent_task_id == parent.id))
        .context("The action did not create the expected child")?;
    assert_eq!(hierarchy.work.len(), 2);
    let activations = store.workflow_trigger_activations(project.clone()).await?;
    assert!(
        activations
            .iter()
            .any(|activation| activation.work_id == child.task.id
                && activation.outcome == "skipped_cycle")
    );
    let evidence = store
        .inspect_workflow_execution(project, run_id.clone())
        .await?
        .unwrap();
    let output = evidence
        .outputs
        .iter()
        .find(|output| {
            output.artifact.node_instance_id == "create" && output.artifact.port == "task"
        })
        .unwrap();
    assert!(
        matches!(&output.artifact.value, ArtifactValue::Task(task) if task.task_id == child.task.id)
    );
    assert_eq!(output.input_receipts.first().unwrap().inputs.len(), 2);
    Ok(())
}
