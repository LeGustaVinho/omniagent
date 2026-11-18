use std::path::Path;

use serde_json::json;

use super::*;
use crate::{
    tasks::TaskPlacement,
    workflows::{
        ArtifactValue, WorkflowGraph,
        tests::{approved_delivery_repository, simple_graph},
    },
};

#[tokio::test]
async fn typed_start_rules_bind_the_real_agile_event_without_a_markdown_source() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("typed-rules.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let graph = |required| {
        serde_json::from_value::<WorkflowGraph>(json!({
            "name": "Handle work creation",
            "nodes": [
                {"id":"entry","kind":"typed_start","configuration":{"kind":"typed_start","fields":[
                    {"name":"extra","valueType":"text","required":required}
                ]}},
                {"id":"end","kind":"end","configuration":{"kind":"end"}}
            ],
            "wires": [
                {"from":{"node":"entry","port":"start"},"to":{"node":"end","port":"ready"}}
            ]
        }))
    };
    let accepted = store
        .save_workflow_revision(project.clone(), "rules".into(), graph(false)?)
        .await?;
    let manual = graph(true)?;
    let manual_only = store
        .save_workflow_revision(project.clone(), "manual".into(), manual)
        .await?;
    let spec = |pipeline| WorkflowTriggerSpec {
        name: "Watch task creation".into(),
        enabled: true,
        event: WorkflowTriggerEvent::Created,
        work: TriggerWorkKind::Task,
        target_state: None,
        pipeline,
        source: WorkflowTriggerSource {
            checkout_id: repo.checkout_id.clone(),
            revision_id: None,
        },
    };
    let invalid = store.save_workflow_trigger_rule(project.clone(), None, None, spec(manual_only)).await
        .expect_err("A rule must be rejected before its required manual inputs can cause a failed activation");
    assert!(invalid.to_string().contains("manual launch"));
    let rule = store
        .save_workflow_trigger_rule(project.clone(), None, None, spec(accepted))
        .await?;
    let task = store
        .create_project_task(project.clone(), "Typed rule Task".into())
        .await?;
    let activations = store.workflow_trigger_activations(project.clone()).await?;
    assert_eq!(activations.len(), 1);
    let activation = &activations[0];
    assert_eq!(activation.outcome, "launched", "{:?}", activation.detail);
    let run = activation
        .run_id
        .clone()
        .context("The rule did not produce a run")?;
    let event = store
        .project_work_events(project.clone(), None, 10)
        .await?
        .into_iter()
        .find(|event| event.id == activation.event_id)
        .context("The activation lost the Agile event")?;
    drop(store);
    let reopened = Store::open(database)?;
    let rules = reopened.workflow_trigger_rules(project.clone()).await?;
    assert_eq!(rules[0].id, rule.id);
    assert_eq!(rules[0].spec.source.checkout_id, repo.checkout_id);
    assert!(rules[0].spec.source.revision_id.is_none());
    let execution = reopened
        .inspect_workflow_execution(project, run)
        .await?
        .context("Missing rule execution")?;
    let receipt = execution.launch.context("Missing rule launch receipt")?;
    assert!(receipt.source.is_none());
    let pinned = receipt.event.context("The launch did not pin its event")?;
    assert_eq!(pinned.id, event.id);
    assert_eq!(pinned.work.task_id, task.id);
    assert_eq!(pinned.data, event.data);
    assert!(execution.outputs.into_iter().any(|output| output.current
        && output.artifact.port == "event"
        && output.artifact.value == ArtifactValue::AgileEvent(pinned.clone())));
    Ok(())
}

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
            simple_graph("Workflow for work events"),
        )
        .await?;
    let second = store
        .save_workflow_revision(
            project.clone(),
            "child-rule".into(),
            simple_graph("Workflow for work events"),
        )
        .await?;
    let spec = |name: &str, work, pipeline| WorkflowTriggerSpec {
        name: name.into(),
        enabled: true,
        event: WorkflowTriggerEvent::Created,
        work,
        target_state: None,
        pipeline,
        source: source.clone().into(),
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
    assert_eq!(
        activation.source.as_ref().map(|source| source.id.as_str()),
        Some(source.revision_id.as_str())
    );
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
            simple_graph("Workflow for work events"),
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
                    }
                    .into(),
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

#[tokio::test]
async fn checklist_trigger_replays_the_exact_subtask_and_item_revision() -> Result<()> {
    use crate::tasks::ChecklistExecutionMode;

    let directory = tempfile::tempdir()?;
    let database = directory.path().join("checklist-launch.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id.clone();
    let root_task = store
        .create_project_task(project.clone(), "Parent".into())
        .await?;
    let subtask = store
        .create_project_subtask(
            project.clone(),
            root_task.id.clone(),
            "Follow up the selected checklist item".into(),
        )
        .await?;
    store
        .set_task_checklist_mode(
            project.clone(),
            subtask.id.clone(),
            Some(ChecklistExecutionMode::RunByItem),
        )
        .await?;
    store
        .ensure_project_checklist(project.clone(), subtask.id.clone())
        .await?;
    let selected = store
        .append_project_checklist_item(project.clone(), subtask.id.clone(), "Selected item".into())
        .await?;
    let source = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let version = store
        .save_workflow_revision(
            project.clone(),
            "checklist-workflow".into(),
            simple_graph("Workflow for work events"),
        )
        .await?;
    let rule = store
        .save_workflow_trigger_rule(
            project.clone(),
            None,
            None,
            WorkflowTriggerSpec {
                name: "Review checked Sub-Task item".into(),
                enabled: true,
                event: WorkflowTriggerEvent::ChecklistChecked,
                work: TriggerWorkKind::Subtask,
                target_state: None,
                pipeline: version.clone(),
                source: WorkflowLaunchSource {
                    checkout_id: repo.checkout_id.clone(),
                    revision_id: source.revision.id,
                }
                .into(),
            },
        )
        .await?;
    let pinned_item = store
        .set_project_checklist_item_checked(
            project.clone(),
            subtask.id.clone(),
            selected.id.clone(),
            selected.revision,
            true,
        )
        .await?;
    let receipts = store.workflow_trigger_activations(project.clone()).await?;
    assert_eq!(receipts.len(), 1);
    let launch = receipts
        .first()
        .context("The Sub-Task trigger did not fire")?;
    assert_eq!(launch.rule_id, rule.id);
    assert_eq!(launch.work_id, subtask.id);
    let run_id = launch
        .run_id
        .clone()
        .context("The checklist trigger did not record its run")?;
    // Reopen a receipt written before the item field was present. Reconstruction
    // must use the retained Agile event and keep its original item revision.
    let old_pin = store
        .workflow_launch(project.clone(), run_id.clone())
        .await?
        .context("Missing launch receipt")?;
    assert_eq!(old_pin.checklist_item.as_ref(), Some(&pinned_item));
    let legacy_bytes = {
        let mut legacy = old_pin.clone();
        legacy.checklist_item = None;
        crate::protection::protect(&serde_json::to_vec(&legacy)?)?
    };
    let original_id = run_id.clone();
    store
        .execute(move |db| {
            db.execute(
                "UPDATE workflow_launches SET payload = ?1 WHERE pipeline_run_id = ?2",
                params![legacy_bytes, original_id],
            )?;
            Ok(())
        })
        .await?;
    drop(store);

    let store = Store::open(database)?;
    assert_eq!(
        store
            .workflow_launch(project.clone(), run_id.clone())
            .await?,
        Some(old_pin)
    );
    let inspected = store
        .inspect_workflow_execution(project.clone(), run_id.clone())
        .await?
        .context("Missing workflow inspection")?;
    assert_eq!(inspected.run.version, version);
    assert_eq!(
        inspected
            .launch
            .context("Missing inspected launch receipt")?
            .checklist_item
            .as_ref(),
        Some(&pinned_item)
    );
    Ok(())
}
