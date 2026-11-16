use std::{fs, path::PathBuf, process::Command};

use crate::{
    conversations::{AttemptPlan, SessionScope},
    dispatch::{ContextSnapshot, Delivery, EventSource, NativeHistory},
    jobs::AdmissionEnvironment,
    mcp_bridge::{RunBridge, invoke_test_mcp_tool},
    providers::{
        Provider, ProviderEvent, ProviderEventData, ProviderLocation, SessionMode, TurnOutcome,
        questions::QuestionRoute,
    },
    repository,
    runs::RunStatus,
    workflows::{BuiltinNode, NodeConfiguration, NodeInstance, WorkflowGraph},
};

use super::*;

fn graph(name: &str) -> WorkflowGraph {
    WorkflowGraph {
        name: name.into(),
        nodes: vec![NodeInstance {
            id: "start".into(),
            kind: BuiltinNode::Start,
            configuration: NodeConfiguration::None,
        }],
        wires: vec![],
    }
}

async fn attempt(store: &Store, checkout: PathBuf) -> Result<AttemptPlan> {
    fs::create_dir(&checkout)?;
    let checkout = dunce::canonicalize(checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    let project = repository::register(store, checkout.clone()).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Ordinary conversation".into())
        .await?;
    let executable = checkout.join("codex.exe");
    fs::write(&executable, b"fixture")?;
    let plan = store
        .prepare_agent_attempt(
            chat.id,
            chat.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "authoring-fixture".into(),
                account_binding: None,
                executable,
                checkout,
                cli_version: "fixture".into(),
                runner: "package-authoring-test".into(),
                permission_revision: "read-only".into(),
                mode: SessionMode::ReadOnly,
                model: Some("author-model".into()),
                effort: None,
            },
            "Normal dispatch".into(),
        )
        .await?;
    store.mark_agent_running(plan.run.id.clone()).await?;
    Ok(plan)
}

#[tokio::test]
async fn package_authoring_mcp_scopes_catalog_and_submits_an_exact_review_revision() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("authoring.sqlite"))?;
    let source = attempt(&store, directory.path().join("source")).await?;
    let foreign = attempt(&store, directory.path().join("foreign")).await?;
    let project = source.run.project_id.clone();
    let published = store
        .save_workflow_revision(
            project.clone(),
            "our-pipeline".into(),
            graph("Project pipeline"),
        )
        .await?;
    let foreign_version = store
        .save_workflow_revision(
            foreign.run.project_id.clone(),
            "foreign-pipeline".into(),
            graph("Foreign private pipeline"),
        )
        .await?;
    let normal_bridge = RunBridge::start(&store, source.run.id.clone()).await?;
    let forbidden = invoke_test_mcp_tool(
        &normal_bridge,
        "read_authoring_catalog",
        json!({"view":"draft"}),
    )
    .await?;
    assert!(forbidden["error"].is_object(), "{forbidden}");
    drop(normal_bridge);

    let draft = store
        .start_workflow_package_authoring(
            project.clone(),
            source.conversation_id.clone(),
            source.run.checkout_id.clone(),
            source.scope.clone(),
            None,
            "Prepare a short review pipeline".into(),
        )
        .await?;
    let active = store
        .claim_next_job(AdmissionEnvironment {
            runner: source.scope.runner.clone(),
            bridge_available: true,
        })
        .await?
        .context("Package authoring did not reach the scheduler")?;
    assert_ne!(active.conversation_id, source.conversation_id);
    let bridge = RunBridge::start(&store, active.run.id.clone()).await?;
    let read_draft = json!({"view":"draft"});
    let predispatch =
        invoke_test_mcp_tool(&bridge, "read_authoring_catalog", read_draft.clone()).await?;
    assert!(predispatch["error"].is_object(), "{predispatch}");
    assert!(active.input[0].text.contains("submit_authoring_for_review"));
    submit_package_authoring_dispatch(&store, &active).await?;
    let catalog = invoke_test_mcp_tool(&bridge, "read_authoring_catalog", read_draft).await?;
    assert_eq!(
        catalog["result"]["structuredContent"]["revision"],
        draft.revision
    );
    let listing = invoke_test_mcp_tool(
        &bridge,
        "read_authoring_catalog",
        json!({"view":"published"}),
    )
    .await?;
    assert_eq!(
        listing["result"]["structuredContent"]["result"]["items"],
        json!([{
            "version":{"destination":"project","kind":"pipeline","id":published.id,"revision":published.revision},
            "name":"Project pipeline", "hasBlockers": false
        }])
    );
    let cross_project = invoke_test_mcp_tool(
        &bridge,
        "read_authoring_catalog",
        json!({
            "view":"published_member", "version": {
                "destination":"project", "kind":"pipeline",
                "id":foreign_version.id, "revision":foreign_version.revision
            }
        }),
    )
    .await?;
    assert!(cross_project["error"].is_object(), "{cross_project}");
    let wrong_authority = invoke_test_mcp_tool(
        &bridge,
        "submit_job",
        json!({
            "requestId":"must-not-delegate",
            "request":{"mode":"read_only","instructions":"Unrelated delegated job"}
        }),
    )
    .await?;
    assert!(wrong_authority["error"].is_object(), "{wrong_authority}");

    let changes = json!({
        "requestId":"edit-1", "expectedRevision": draft.revision, "operations": [
            {"operation":"rename_package", "name":"Scoped review"},
            {"operation":"put_member","member":{"key":"pipeline","name":"Review pipeline",
                "body":{"kind":"pipeline","graph":{"name":"Review pipeline",
                    "nodes":[{"id":"start","kind":"start","configuration":{"kind":"none"}}],
                    "wires":[]}}}}
        ]
    });
    let response =
        invoke_test_mcp_tool(&bridge, "apply_authoring_operations", changes.clone()).await?;
    let receipt = &response["result"]["structuredContent"];
    assert_eq!(receipt["status"], "applied", "{response}");
    let working = receipt["revision"]
        .as_str()
        .context("No revision receipt")?;
    assert_ne!(working, draft.revision);
    let repeat =
        invoke_test_mcp_tool(&bridge, "apply_authoring_operations", changes.clone()).await?;
    assert_eq!(repeat["result"]["structuredContent"], *receipt);
    let mut changed_retry = changes.clone();
    changed_retry["operations"][0]["name"] = json!("Different content");
    let repeated_id =
        invoke_test_mcp_tool(&bridge, "apply_authoring_operations", changed_retry).await?;
    assert_eq!(
        repeated_id["result"]["structuredContent"]["status"],
        "request_conflict"
    );
    let mut stale = changes;
    stale["requestId"] = json!("edit-stale");
    let revision_conflict =
        invoke_test_mcp_tool(&bridge, "apply_authoring_operations", stale).await?;
    assert_eq!(
        revision_conflict["result"]["structuredContent"],
        json!({"status":"revision_conflict","currentRevision":working})
    );
    let reviewing = store
        .workflow_package(project.clone(), draft.id.clone())
        .await?;
    assert_eq!(reviewing.revision, draft.revision);

    let submit = invoke_test_mcp_tool(
        &bridge,
        "submit_authoring_for_review",
        json!({"requestId":"submit-1","expectedRevision":working}),
    )
    .await?;
    assert_eq!(
        submit["result"]["structuredContent"],
        json!({"status":"submitted","revision":working})
    );
    let publish = invoke_test_mcp_tool(&bridge, "publish_workflow_package", json!({})).await?;
    assert!(publish["error"].is_object(), "{publish}");
    let reviewing = store
        .workflow_package(project.clone(), draft.id.clone())
        .await?;
    assert_eq!(reviewing.revision, working);
    assert_eq!(reviewing.package.name, "Scoped review");
    assert!(reviewing.publication.is_none());
    assert_eq!(reviewing.request.context("Missing request")?.state, "saved");
    assert_eq!(
        store.workflow_authoring_bases(project.clone()).await?.len(),
        1
    );
    store
        .finish_agent_attempt(active.run.id, RunStatus::Succeeded, None)
        .await?;
    let archived =
        invoke_test_mcp_tool(&bridge, "read_authoring_catalog", json!({"view":"draft"})).await?;
    assert!(archived["error"].is_object(), "{archived}");

    let feedback = "Rename this pipeline for the next review";
    let requested = store
        .request_workflow_package_changes(
            project.clone(),
            draft.id.clone(),
            working.into(),
            source.run.checkout_id.clone(),
            source.scope.clone(),
            feedback.into(),
        )
        .await?;
    assert_eq!(requested.revision, working);
    let follow_up = store
        .claim_next_job(AdmissionEnvironment {
            runner: source.scope.runner.clone(),
            bridge_available: true,
        })
        .await?
        .context("Feedback did not create an authoring attempt")?;
    assert_ne!(follow_up.conversation_id, active.conversation_id);
    assert!(follow_up.input[0].text.contains(feedback));
    assert!(follow_up.input[0].text.contains(working));
    let bridge = RunBridge::start(&store, follow_up.run.id.clone()).await?;
    submit_package_authoring_dispatch(&store, &follow_up).await?;
    let revised = invoke_test_mcp_tool(
        &bridge,
        "apply_authoring_operations",
        json!({
            "requestId":"edit-1", "expectedRevision":working, "operations":[
                {"operation":"rename_package", "name":"Agent private work"}
            ],
        }),
    )
    .await?;
    let head = revised["result"]["structuredContent"]["revision"]
        .as_str()
        .with_context(|| format!("Feedback run did not save its private revision: {revised}"))?;
    let mut package = reviewing.package;
    package.name = "User edited while the agent worked".into();
    let edited = store
        .save_workflow_package(project.clone(), draft.id.clone(), working.into(), package)
        .await?;
    assert_ne!(edited.revision, working);
    let conflict = invoke_test_mcp_tool(
        &bridge,
        "submit_authoring_for_review",
        json!({"requestId":"submit-1","expectedRevision":head}),
    )
    .await?;
    assert_eq!(
        conflict["result"]["structuredContent"],
        json!({"status":"revision_conflict","currentRevision":edited.revision})
    );
    store
        .finish_agent_attempt(
            follow_up.run.id,
            RunStatus::Succeeded,
            Some(json!({"name":"Unauthorized JSON-only response","members":[]}).to_string()),
        )
        .await?;
    let retained = store
        .workflow_package(project.clone(), draft.id.clone())
        .await?;
    assert_eq!(retained.revision, edited.revision);
    assert_eq!(retained.package.name, "User edited while the agent worked");
    assert_eq!(
        retained.request.context("Missing feedback request")?.state,
        "superseded"
    );

    store
        .request_workflow_package_changes(
            project.clone(),
            draft.id.clone(),
            edited.revision.clone(),
            source.run.checkout_id,
            source.scope.clone(),
            "Review the current selection".into(),
        )
        .await?;
    let unsubmitted = store
        .claim_next_job(AdmissionEnvironment {
            runner: source.scope.runner,
            bridge_available: true,
        })
        .await?
        .context("The unsubmitted review request did not reach the scheduler")?;
    submit_package_authoring_dispatch(&store, &unsubmitted).await?;
    store
        .record_agent_observation(
            unsubmitted.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            unsubmitted.run.id,
            RunStatus::Succeeded,
            Some(json!({"name":"Unauthorized JSON-only response","members":[]}).to_string()),
        )
        .await?;
    let retained = store.workflow_package(project, draft.id).await?;
    assert_eq!(retained.revision, edited.revision);
    assert_eq!(
        retained.request.context("Missing submission status")?.state,
        "invalid"
    );
    Ok(())
}

async fn submit_package_authoring_dispatch(store: &Store, active: &AttemptPlan) -> Result<()> {
    let snapshot = ContextSnapshot {
        version: 13,
        context_id: active.context_id.clone(),
        scope: active.scope.clone(),
        session_id: None,
        job_policy_revision: None,
        role_context: None,
        base_commit: None,
        submitted_messages: active.input.clone(),
        conversation_references: Vec::new(),
        native_history: NativeHistory::Fresh,
        capabilities: None,
        filesystem_skills: Vec::new(),
        reported_skills: None,
        instructions: None,
        scoped_launch: None,
        requested_skill: None,
        plugin_inventory: None,
        attachments: Vec::new(),
        task_context: None,
        repository_markdown: None,
        question_route: QuestionRoute::Native,
        answered_question: None,
    };
    store.mark_agent_running(active.run.id.clone()).await?;
    let dispatch = store
        .prepare_dispatch(
            active.run.id.clone(),
            snapshot,
            vec![serde_json::to_vec(&active.input[0].text)?],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await
}
