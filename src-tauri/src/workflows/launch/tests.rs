use std::path::Path;

use super::*;
use crate::{
    tasks::TaskPlacement,
    workflows::{
        ArtifactKind, Endpoint, NodeConfiguration, NodeInputs, NodeInstance, Wire, WorkflowGraph,
        WorkflowHostSubmission, tests::approved_delivery_repository,
    },
};

#[tokio::test]
async fn a_manual_launch_pins_the_published_revision_source_and_subtask_across_restart()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id.clone();
    let note = Path::new(&repo.checkout_root).join("brief.md");
    let original = store
        .capture_repository_markdown(repo.checkout_id.clone(), note.clone())
        .await?;
    let source = WorkflowLaunchSource {
        checkout_id: repo.checkout_id.clone(),
        revision_id: original.revision.id.clone(),
    };
    let parent = store
        .create_project_task(project.clone(), "Parent".into())
        .await?;
    let subtask = store
        .create_project_work(
            project.clone(),
            "Selected sub-task".into(),
            TaskPlacement::Subtask {
                parent_task_id: parent.id.clone(),
            },
        )
        .await?;
    let graph = |name: &str| WorkflowGraph {
        name: name.into(),
        nodes: [
            ("entry-from-editor", BuiltinNode::Start),
            ("frozen-destination", BuiltinNode::LoadContext),
        ]
        .into_iter()
        .map(|(id, kind)| NodeInstance {
            id: id.into(),
            kind,
            configuration: NodeConfiguration::None,
        })
        .collect(),
        wires: [("start", "ready"), ("document", "document")]
            .into_iter()
            .map(|(from, to)| Wire {
                from: Endpoint {
                    node: "entry-from-editor".into(),
                    port: from.into(),
                },
                to: Endpoint {
                    node: "frozen-destination".into(),
                    port: to.into(),
                },
            })
            .collect(),
    };
    let first = store
        .save_workflow_revision(
            project.clone(),
            "edited-workflow".into(),
            graph("Original revision"),
        )
        .await?;
    let request = ManualWorkflowLaunch {
        project_id: project.clone(),
        workflow: first.clone(),
        source,
        request_id: Uuid::new_v4().to_string(),
        work_id: Some(subtask.id.clone()),
    };
    let (run, repeated) = tokio::join!(
        store.launch_workflow_manual(request.clone()),
        store.launch_workflow_manual(request.clone()),
    );
    let run = run?;
    assert_eq!(repeated?, run);
    assert_eq!(
        store.list_workflow_runs(project.clone()).await?,
        [run.clone()]
    );
    assert!(
        store
            .launch_workflow_manual(ManualWorkflowLaunch {
                work_id: Some(parent.id.clone()),
                ..request.clone()
            })
            .await
            .is_err()
    );

    let outputs = store
        .current_workflow_host_outputs(project.clone(), run.id.clone())
        .await?;
    let input_artifact = outputs["entry-from-editor"].clone();
    let initial_context = store
        .resolve_workflow_artifact(
            project.clone(),
            run.id.clone(),
            outputs["frozen-destination"].clone(),
            ArtifactKind::Markdown,
        )
        .await?
        .context("The start context was not materialized")?;
    assert_eq!(
        initial_context.value,
        ArtifactValue::Markdown(original.revision.clone())
    );
    assert_eq!(
        store
            .execute_workflow_host(WorkflowHostSubmission {
                project_id: project.clone(),
                run_id: run.id.clone(),
                node_id: "frozen-destination".into(),
                inputs: NodeInputs::from([("document".into(), input_artifact)]),
                request: WorkflowHostRequest::LoadContext,
            })
            .await?,
        initial_context
    );
    let (project_check, run_check, exact, wrong) = (
        project.clone(),
        run.id.clone(),
        subtask.id.clone(),
        parent.id,
    );
    store
        .execute(move |db| {
            require_launch_work(db, &project_check, &run_check, Some(&exact))?;
            assert!(require_launch_work(db, &project_check, &run_check, Some(&wrong)).is_err());
            assert!(require_launch_work(db, &project_check, &run_check, None).is_err());
            Ok(())
        })
        .await?;
    std::fs::write(note, "New text outside the saved revision")?;
    let newest = store
        .save_workflow_revision(
            project.clone(),
            first.id.clone(),
            graph("Later published revision"),
        )
        .await?;
    assert_ne!(newest, first);
    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(reopened.launch_workflow_manual(request.clone()).await?, run);
    assert_eq!(
        reopened
            .workflow_launch(project.clone(), run.id.clone())
            .await?,
        Some(WorkflowLaunchReceipt {
            workflow_run_id: run.id.clone(),
            version: first,
            source: original.revision,
            origin: WorkflowLaunchOrigin::Agile {
                request_id: request.request_id.clone(),
                work_id: subtask.id,
            },
            checklist_item: None,
        })
    );
    assert!(
        reopened
            .workflow_launch("foreign".into(), run.id.clone())
            .await?
            .is_none()
    );
    assert!(
        reopened
            .launch_workflow_manual(ManualWorkflowLaunch {
                project_id: "foreign".into(),
                ..request.clone()
            })
            .await
            .is_err()
    );
    assert_eq!(reopened.list_workflow_runs(project).await?, [run]);
    Ok(())
}
