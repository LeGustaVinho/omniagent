use std::path::Path;

use super::*;
use crate::{
    tasks::TaskPlacement,
    workflows::{ArtifactKind, tests::approved_delivery_repository},
};
use serde_json::json;
use uuid::Uuid;

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
    let graph = crate::workflows::tests::simple_graph;
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
    let retired = store
        .save_workflow_revision(
            project.clone(),
            BUILTIN_DELIVERY_ID.into(),
            graph("Retired repository delivery"),
        )
        .await?;
    let error = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            workflow: retired,
            ..request.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Repository delivery has been removed");

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
    let initial_context = store
        .resolve_workflow_artifact(
            project.clone(),
            run.id.clone(),
            outputs["start"].clone(),
            ArtifactKind::Markdown,
        )
        .await?
        .context("The Start document was not recorded")?;
    assert_eq!(
        initial_context.value,
        ArtifactValue::Markdown(original.revision.clone())
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
    let saved_run = run.id.clone();
    store
        .execute(move |db| {
            let payload: Vec<u8> = db.query_row(
                "SELECT payload FROM workflow_launches WHERE pipeline_run_id = ?1",
                [&saved_run],
                |row| row.get(0),
            )?;
            let mut historic: Map<String, Value> =
                serde_json::from_slice(&protection::unprotect(&payload)?)?;
            assert!(historic.remove("checkoutId").is_some());
            db.execute(
                "UPDATE workflow_launches SET payload = ?1 WHERE pipeline_run_id = ?2",
                params![
                    protection::protect(&serde_json::to_vec(&historic)?)?,
                    saved_run
                ],
            )?;
            Ok(())
        })
        .await?;
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
            checkout_id: original.revision.checkout_id.clone(),
            source: Some(original.revision),
            source_commit: None,
            automatic: false,
            origin: WorkflowLaunchOrigin::Agile {
                request_id: request.request_id.clone(),
                work_id: subtask.id,
            },
            checklist_item: None,
            inputs: Default::default(),
            event: None,
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

#[tokio::test]
async fn typed_start_records_exact_inputs_without_markdown_and_deduplicates_launches() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let foreign_dir = directory.path().join("foreign");
    std::fs::create_dir(&foreign_dir)?;
    let foreign = approved_delivery_repository(&store, &foreign_dir).await?;
    assert_ne!(repo.project_id, foreign.project_id);
    let project = repo.project_id.clone();
    let work = store
        .create_project_task(project.clone(), "Scoped Task".into())
        .await?;
    let foreign_work = store
        .create_project_task(foreign.project_id, "Foreign Task".into())
        .await?;
    let workflow = store.save_workflow_revision(
        project.clone(),
        "typed-workflow".into(),
        serde_json::from_value(json!({
            "name": "Typed workflow",
            "nodes": [
                {"id":"entry", "kind":"typed_start", "configuration":{"kind":"typed_start","fields":[
                    {"name":"title", "valueType":"text", "required":true},
                    {"name":"count", "valueType":"number", "required":true},
                    {"name":"empty", "valueType":"null", "required":false}
                ]}},
                {"id":"end", "kind":"end", "configuration":{"kind":"end"}}
            ],
            "wires": [
                {"from":{"node":"entry","port":"start"},"to":{"node":"end","port":"ready"}},
                {"from":{"node":"entry","port":"inputs"},"to":{"node":"end","port":"result"}}
            ]
        }))?,
    ).await?;
    let base = store
        .workflow_authoring_bases(project.clone())
        .await?
        .into_iter()
        .find(|base| base.version == workflow)
        .context("Typed Start metadata is missing")?;
    let base = serde_json::to_value(base)?;
    let schema = &base["start"];
    assert_eq!(
        schema["fields"][0],
        json!({"name": "title", "valueType": "text", "required": true})
    );
    assert_eq!(schema["requiresDocument"], false);
    assert_eq!(schema["requiresWork"], false);
    assert_eq!(schema["requiresEvent"], false);
    let inputs =
        serde_json::from_value(json!({ "title": "Pinned text", "count": 0, "empty": null }))?;
    let request = TypedWorkflowLaunch {
        project_id: project.clone(),
        workflow: workflow.clone(),
        checkout_id: repo.checkout_id.clone(),
        document_revision_id: None,
        request_id: Uuid::new_v4().to_string(),
        work_id: Some(work.id.clone()),
        inputs,
        automatic: false,
    };
    for invalid in [
        TypedWorkflowLaunch {
            checkout_id: foreign.checkout_id,
            ..request.clone()
        },
        TypedWorkflowLaunch {
            work_id: Some(foreign_work.id),
            ..request.clone()
        },
        TypedWorkflowLaunch {
            inputs: serde_json::from_value(json!({ "title": "Missing count" }))?,
            ..request.clone()
        },
        TypedWorkflowLaunch {
            inputs: serde_json::from_value(json!({ "title": "Wrong type", "count": "0" }))?,
            ..request.clone()
        },
        TypedWorkflowLaunch {
            inputs: serde_json::from_value(
                json!({ "title": "Extra field", "count": 0, "extra": true }),
            )?,
            ..request.clone()
        },
    ] {
        assert!(
            store
                .launch_workflow_request(WorkflowLaunchRequest::Typed(invalid))
                .await
                .is_err()
        );
    }
    let retry = WorkflowLaunchRequest::Typed(request.clone());
    let (first, repeated) = tokio::join!(
        store.launch_workflow_request(retry.clone()),
        store.launch_workflow_request(retry.clone())
    );
    let run = first?;
    assert_eq!(repeated?, run);
    let receipt = store
        .workflow_launch(project.clone(), run.id.clone())
        .await?
        .context("Missing Start receipt")?;
    assert_eq!(receipt.version, workflow);
    assert_eq!(receipt.checkout_id, repo.checkout_id);
    assert!(receipt.source.is_none());
    assert!(receipt.event.is_none());
    assert_eq!(receipt.inputs, request.inputs);
    let outputs = store
        .current_workflow_host_outputs(project.clone(), run.id.clone())
        .await?;
    let artifact = store
        .resolve_workflow_artifact(
            project.clone(),
            run.id.clone(),
            outputs["entry"].clone(),
            ArtifactKind::JsonObject,
        )
        .await?
        .context("Missing Start inputs artifact")?;
    assert_eq!(
        artifact.value,
        ArtifactValue::JsonObject(request.inputs.clone())
    );
    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(reopened.launch_workflow_request(retry).await?, run);
    let mut changed = request.clone();
    changed.inputs.remove("empty");
    assert!(
        reopened
            .launch_workflow_request(WorkflowLaunchRequest::Typed(changed))
            .await
            .is_err()
    );
    assert_eq!(
        reopened
            .workflow_launch(project.clone(), run.id.clone())
            .await?,
        Some(receipt)
    );
    assert_eq!(reopened.list_workflow_runs(project).await?, [run]);
    Ok(())
}

#[tokio::test]
async fn typed_start_normalizes_one_request_and_pins_its_checkout_head() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("store.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let root = std::path::Path::new(&repo.checkout_root);
    let note = root.join("brief.md");
    std::fs::write(&note, "# Exact saved request")?;
    let saved = store
        .capture_repository_markdown(repo.checkout_id.clone(), note.clone())
        .await?;
    std::fs::write(&note, "# New draft")?;
    let workflow = store
        .save_workflow_revision(
            repo.project_id.clone(),
            "request-intake".into(),
            serde_json::from_value(json!({
                "name": "Request intake",
                "nodes": [
                    {"id":"entry","kind":"typed_start","configuration":{
                        "kind":"typed_start","fields":[],"promptOrMarkdown":true
                    }},
                    {"id":"end","kind":"end","configuration":{"kind":"end"}}
                ],
                "wires": [
                    {"from":{"node":"entry","port":"start"},"to":{"node":"end","port":"ready"}},
                    {"from":{"node":"entry","port":"inputs"},"to":{"node":"end","port":"result"}}
                ]
            }))?,
        )
        .await?;
    let request = TypedWorkflowLaunch {
        project_id: repo.project_id.clone(),
        workflow,
        checkout_id: repo.checkout_id,
        document_revision_id: None,
        request_id: "request-intake".into(),
        work_id: None,
        inputs: Default::default(),
        automatic: false,
    };
    let prompt: Map<String, Value> =
        serde_json::from_value(json!({"prompt": "Exact typed request"}))?;
    for (inputs, document_revision_id) in [
        (Map::new(), None),
        (serde_json::from_value(json!({"prompt": "  "}))?, None),
        (prompt.clone(), Some(saved.revision.id.clone())),
    ] {
        assert!(
            store
                .launch_workflow_request(WorkflowLaunchRequest::Typed(TypedWorkflowLaunch {
                    inputs,
                    document_revision_id,
                    ..request.clone()
                }))
                .await
                .is_err()
        );
    }
    for (inputs, document_revision_id, expected) in [
        (prompt, None, "Exact typed request"),
        (Map::new(), Some(saved.revision.id), "# Exact saved request"),
        (
            serde_json::from_value(json!({"prompt":"Needs revision"}))?,
            None,
            "Needs revision",
        ),
    ] {
        let request = WorkflowLaunchRequest::Typed(TypedWorkflowLaunch {
            request_id: Uuid::new_v4().to_string(),
            inputs,
            document_revision_id,
            ..request.clone()
        });
        let run = store.launch_workflow_request(request.clone()).await?;
        assert_eq!(store.launch_workflow_request(request).await?, run);
        let receipt = store
            .workflow_launch(repo.project_id.clone(), run.id.clone())
            .await?
            .unwrap();
        assert_eq!(receipt.source_commit, crate::repository::base_commit(root)?);
        let outputs = store
            .workflow_run_artifacts(repo.project_id.clone(), run.id)
            .await?;
        assert!(outputs.iter().any(|artifact| artifact.port == "result"
            && artifact.node_instance_id == "end"
            && matches!(&artifact.value, ArtifactValue::JsonObject(value) if value["request"] == expected)));
    }
    Ok(())
}
