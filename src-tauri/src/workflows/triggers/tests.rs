use std::path::Path;

use super::*;
use crate::{
    tasks::TaskPlacement,
    workflows::{DeliveryRoles, builtin_delivery_graph, tests::approved_delivery_repository},
};

#[tokio::test]
async fn rules_pin_activation_deduplicate_and_stop_a_cycle_after_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("triggers.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let source = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let source = WorkflowLaunchSource {
        checkout_id: repo.checkout_id,
        revision_id: source.revision.id,
    };
    let first = store
        .save_workflow_revision(
            project.clone(),
            "task-rule".into(),
            builtin_delivery_graph(DeliveryRoles::default()),
        )
        .await?;
    let second = store
        .save_workflow_revision(
            project.clone(),
            "child-rule".into(),
            builtin_delivery_graph(DeliveryRoles::default()),
        )
        .await?;
    let spec = |name: &str, work, pipeline| WorkflowTriggerSpec {
        name: name.into(),
        enabled: true,
        event: WorkflowTriggerEvent::Created,
        work,
        target_state: None,
        pipeline,
        source: source.clone(),
    };
    let a = store
        .save_workflow_trigger_rule(
            project.clone(),
            None,
            None,
            spec("Launch for Tasks", TriggerWorkKind::Task, first.clone()),
        )
        .await?;
    let b = store
        .save_workflow_trigger_rule(
            project.clone(),
            None,
            None,
            spec(
                "Launch for Sub-Tasks",
                TriggerWorkKind::Subtask,
                second.clone(),
            ),
        )
        .await?;
    let task = store
        .create_project_task(project.clone(), "Initial Task".into())
        .await?;
    let receipts = store.workflow_trigger_activations(project.clone()).await?;
    assert_eq!(receipts.len(), 1);
    let run_a = receipts[0].run_id.clone().context("missing work launch")?;
    assert_eq!(receipts[0].rule_id, a.id);
    let activation = store
        .workflow_launch(project.clone(), run_a.clone())
        .await?
        .context("missing run receipt")?;
    assert_eq!(activation.version, first);
    assert_eq!(activation.source.id, source.revision_id);
    assert_eq!(
        activation.origin,
        WorkflowLaunchOrigin::Rule {
            rule_id: a.id.clone(),
            event_id: receipts[0].event_id.clone(),
            work_id: task.id.clone(),
        }
    );
    let child = store
        .create_pipeline_work(
            project.clone(),
            run_a.clone(),
            "Generated Sub-Task".into(),
            TaskPlacement::Subtask {
                parent_task_id: task.id,
            },
        )
        .await?;
    let receipts = store.workflow_trigger_activations(project.clone()).await?;
    assert_eq!(receipts[0].rule_id, b.id);
    assert_eq!(receipts[0].work_id, child.id);
    let run_b = receipts[0].run_id.clone().context("missing child launch")?;
    let generated = store
        .create_pipeline_work(
            project.clone(),
            run_b,
            "Generated Task".into(),
            TaskPlacement::Task { epic_id: None },
        )
        .await?;
    let events = store.project_work_events(project.clone(), None, 20).await?;
    let event = events
        .into_iter()
        .find(|event| event.id == receipts[1].event_id)
        .context("missing original event")?;
    let new = store
        .save_workflow_trigger_rule(
            project.clone(),
            Some(a.id.clone()),
            Some(a.revision),
            spec("Tasks v2", TriggerWorkKind::Task, first.clone()),
        )
        .await?;
    assert_eq!(new.revision, 2);
    assert!(
        store
            .save_workflow_trigger_rule(
                project.clone(),
                Some(a.id.clone()),
                Some(a.revision),
                spec("Stale", TriggerWorkKind::Task, first),
            )
            .await
            .is_err()
    );
    drop(store);

    let restarted = Store::open(database)?;
    restarted
        .execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    let receipts = restarted
        .workflow_trigger_activations(project.clone())
        .await?;
    assert_eq!(receipts.len(), 3);
    assert_eq!(receipts[0].outcome, "skipped_cycle");
    assert_eq!(receipts[0].work_id, generated.id);
    assert!(receipts[0].run_id.is_none());
    assert_eq!(receipts[2].run_id.as_deref(), Some(run_a.as_str()));
    assert_eq!(receipts[2].rule_revision, 1);
    assert_eq!(receipts[2].rule_name, "Launch for Tasks");
    assert!(
        restarted
            .workflow_trigger_rules("foreign".into())
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn status_and_checklist_rules_isolate_a_failed_launch_from_the_work_change() -> Result<()> {
    use TaskWorkflowState::{Backlog, Blocked, InProgress, Ready};
    use WorkflowTriggerEvent::{ChecklistChecked, ChecklistReopened, StateChanged};

    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("events.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let document = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let pipeline = store
        .save_workflow_revision(
            project.clone(),
            "progress-rules".into(),
            builtin_delivery_graph(DeliveryRoles::default()),
        )
        .await?;
    for (event, state) in [
        (StateChanged, Some(Ready)),
        (StateChanged, Some(Blocked)),
        (ChecklistChecked, None),
        (ChecklistReopened, None),
    ] {
        store
            .save_workflow_trigger_rule(
                project.clone(),
                None,
                None,
                WorkflowTriggerSpec {
                    name: format!("{event:?} {state:?}"),
                    enabled: true,
                    event,
                    work: TriggerWorkKind::All,
                    target_state: state,
                    pipeline: pipeline.clone(),
                    source: WorkflowLaunchSource {
                        checkout_id: repo.checkout_id.clone(),
                        revision_id: document.revision.id.clone(),
                    },
                },
            )
            .await?;
    }
    let task = store
        .create_project_task(project.clone(), "Review the change".into())
        .await?;
    store
        .transition_project_work(project.clone(), task.id.clone(), Backlog, InProgress)
        .await?;
    assert!(
        store
            .workflow_trigger_activations(project.clone())
            .await?
            .is_empty()
    );
    store
        .transition_project_work(project.clone(), task.id.clone(), InProgress, Ready)
        .await?;
    store
        .ensure_project_checklist(project.clone(), task.id.clone())
        .await?;
    let item = store
        .append_project_checklist_item(project.clone(), task.id.clone(), "Confirm".into())
        .await?;
    let checked = store
        .set_project_checklist_item_checked(
            project.clone(),
            task.id.clone(),
            item.id.clone(),
            item.revision,
            true,
        )
        .await?;
    store
        .execute(|db| {
            db.execute_batch(
                "CREATE TRIGGER reject_test_launch BEFORE INSERT ON workflow_launches
            BEGIN SELECT RAISE(ABORT, 'forced launch error'); END;",
            )?;
            Ok(())
        })
        .await?;
    store
        .set_project_checklist_item_checked(
            project.clone(),
            task.id.clone(),
            item.id,
            checked.revision,
            false,
        )
        .await?;
    let events = store
        .project_work_events(project.clone(), None, 100)
        .await?;
    let receipts = store.workflow_trigger_activations(project.clone()).await?;
    assert_eq!(receipts.len(), 3);
    assert_eq!(receipts[0].outcome, "failed");
    assert!(receipts[0].run_id.is_none());
    let matching_events = receipts
        .iter()
        .map(|receipt| {
            events
                .iter()
                .find(|event| event.id == receipt.event_id)
                .map(|event| &event.data)
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        matching_events[..],
        [
            Some(AgileEventData::ChecklistReopened { .. }),
            Some(AgileEventData::ChecklistChecked { .. }),
            Some(AgileEventData::StateChanged { current: Ready, .. }),
        ]
    ));
    let runs: i64 = store
        .execute(|db| {
            Ok(
                db.query_row("SELECT count(*) FROM workflow_run_pins", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .await?;
    assert_eq!(runs, 2);
    Ok(())
}
