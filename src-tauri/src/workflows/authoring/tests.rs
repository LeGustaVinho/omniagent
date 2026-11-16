use std::{fs, path::Path};

use crate::{
    dispatch::{ContextSnapshot, Delivery, EventSource, NativeHistory},
    jobs::AdmissionEnvironment,
    providers::{
        ProviderEvent, ProviderEventData, ProviderLocation, TurnOutcome, questions::QuestionRoute,
    },
    runs::RunStatus,
    workflows::{
        BuiltinNode, NodeConfiguration, NodeInstance, tests::approved_delivery_repository,
    },
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
        wires: Vec::new(),
    }
}

pub(super) async fn finish_request(
    store: &Store,
    draft: &AuthoringDraft,
    scope: &SessionScope,
    output: String,
    includes: &[&str],
) -> Result<String> {
    finish_background_authoring_job(store, &draft.conversation_id, scope, output, includes).await
}

pub(super) async fn finish_background_authoring_job(
    store: &Store,
    source_conversation: &str,
    scope: &SessionScope,
    output: String,
    includes: &[&str],
) -> Result<String> {
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        })
        .await?
        .context("The authoring request did not reach the scheduler")?;
    assert_ne!(attempt.conversation_id, source_conversation);
    let [prompt] = attempt.input.as_slice() else {
        anyhow::bail!("Expected one authoring input")
    };
    for text in includes {
        assert!(
            prompt.text.contains(text),
            "Authoring input is missing {text}"
        );
    }
    let snapshot = ContextSnapshot {
        version: 13,
        context_id: attempt.context_id.clone(),
        scope: scope.clone(),
        session_id: None,
        job_policy_revision: None,
        role_context: None,
        base_commit: None,
        submitted_messages: attempt.input.clone(),
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
    let run = attempt.run.id;
    store.mark_agent_running(run.clone()).await?;
    let dispatch = store
        .prepare_dispatch(
            run.clone(),
            snapshot,
            vec![serde_json::to_vec(&prompt.text)?],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    store
        .record_agent_observation(
            run.clone(),
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
        .finish_agent_attempt(run.clone(), RunStatus::Succeeded, Some(output))
        .await?;
    Ok(run)
}

#[tokio::test]
async fn authoring_seals_chat_base_and_feedback_revisions_without_updating_published_workflows()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("authoring.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let conversation = store
        .create_conversation(repo.checkout_id.clone(), "Authoring".into())
        .await?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "author-fixture".into(),
        account_binding: None,
        checkout: Path::new(&repo.checkout_root).to_path_buf(),
        executable: Path::new(&repo.checkout_root).join("codex.exe"),
        cli_version: "fixture".into(),
        runner: "fixture-runner".into(),
        permission_revision: "read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("author-model".into()),
        effort: None,
    };
    fs::write(&scope.executable, b"fixture")?;
    store
        .save_conversation_draft(conversation.id.clone(), "Ordinary unsent chat".into())
        .await?;
    let base = store
        .save_workflow_revision(
            repo.project_id.clone(),
            "review".into(),
            graph("Published base title"),
        )
        .await?;
    let bases = store
        .workflow_authoring_bases(repo.project_id.clone())
        .await?;
    assert_eq!(bases.len(), 1);
    assert_eq!(bases[0].version, base);
    let queued = store
        .start_workflow_authoring(
            conversation.id.clone(),
            repo.checkout_id.clone(),
            scope.clone(),
            Some(base.clone()),
            "Add repository verification".into(),
        )
        .await?;
    assert_eq!(queued.request.state, "queued");
    let initial = queued.head.as_ref().unwrap();
    assert_eq!(initial.graph.name, "Published base title");
    assert!(
        store
            .request_workflow_authoring_changes(
                repo.project_id.clone(),
                queued.id.clone(),
                initial.id.clone(),
                scope.clone(),
                "Do not queue a second job".into()
            )
            .await
            .is_err()
    );
    let run = finish_request(
        &store,
        &queued,
        &scope,
        serde_json::to_string(&graph("Agent proposal"))?,
        &["Add repository verification", "Published base title"],
    )
    .await?;

    drop(store);
    let store = Store::open(database.clone())?;
    assert_eq!(
        store.conversation_draft(conversation.id.clone()).await?,
        "Ordinary unsent chat"
    );
    assert!(
        store
            .conversation_history(conversation.id.clone())
            .await?
            .messages
            .is_empty()
    );
    let draft = store
        .inspect_workflow_authoring(repo.project_id.clone(), queued.id.clone())
        .await?;
    assert_eq!(draft.base.as_ref(), Some(&base));
    let candidate = draft.head.as_ref().unwrap();
    assert_eq!(candidate.run_id.as_deref(), Some(run.as_str()));
    assert_eq!(candidate.graph.name, "Agent proposal");
    assert!(
        store
            .inspect_workflow_authoring("foreign".into(), draft.id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .request_workflow_authoring_changes(
                repo.project_id.clone(),
                draft.id.clone(),
                initial.id.clone(),
                scope.clone(),
                "Feedback on the archived revision".into()
            )
            .await
            .is_err()
    );

    let revised = store
        .save_workflow_authoring_canvas(
            repo.project_id.clone(),
            draft.id.clone(),
            candidate.id.clone(),
            graph("Direct editor revision"),
            Some(serde_json::from_value(serde_json::json!({
                "layout": { "start": { "x": 150, "y": -33 } },
                "updateDefinitions": {}
            }))?),
        )
        .await?;
    let editor_revision = revised.head.as_ref().unwrap();
    assert_ne!(editor_revision.id, candidate.id);
    assert!(
        store
            .save_workflow_authoring_draft(
                repo.project_id.clone(),
                draft.id.clone(),
                candidate.id.clone(),
                graph("Stale revision")
            )
            .await
            .is_err()
    );
    drop(store);
    let store = Store::open(database)?;
    let restored = store
        .inspect_workflow_authoring(repo.project_id.clone(), draft.id.clone())
        .await?;
    let restored = restored
        .head
        .context("missing canvas revision after restart")?;
    assert_eq!(restored.id, editor_revision.id);
    assert_eq!(
        serde_json::to_value(&restored.layout)?,
        serde_json::json!({ "start": { "x": 150.0, "y": -33.0 } })
    );
    assert_eq!(restored.definitions, editor_revision.definitions);
    let mine = store
        .project_workflow_graph_drafts(repo.project_id.clone())
        .await?;
    assert_eq!(
        serde_json::to_value(&mine[0])?["name"],
        "Direct editor revision"
    );
    assert!(
        store
            .project_workflow_graph_drafts("another-project".into())
            .await?
            .is_empty()
    );
    let next = store
        .request_workflow_authoring_changes(
            repo.project_id.clone(),
            draft.id.clone(),
            editor_revision.id.clone(),
            scope.clone(),
            "Use the editor revision for this feedback".into(),
        )
        .await?;
    finish_request(
        &store,
        &next,
        &scope,
        "This is not a pipeline".into(),
        &[
            "Use the editor revision for this feedback",
            "Direct editor revision",
        ],
    )
    .await?;
    let [saved]: [AuthoringDraft; 1] = store
        .conversation_workflow_authoring(conversation.id.clone())
        .await?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Expected one linked draft"))?;
    assert_eq!(saved.request.state, "invalid");
    assert_eq!(saved.head.unwrap().id, editor_revision.id);
    assert_eq!(
        store
            .workflow_definition(repo.project_id.clone(), base)
            .await?
            .unwrap()
            .name,
        "Published base title"
    );

    let from_scratch = store
        .start_workflow_authoring(
            conversation.id,
            repo.checkout_id,
            scope.clone(),
            None,
            "Build a pipeline from scratch".into(),
        )
        .await?;
    assert!(from_scratch.head.is_none());
    finish_request(
        &store,
        &from_scratch,
        &scope,
        serde_json::to_string(&graph("New pipeline"))?,
        &["Build a pipeline from scratch", "\"currentGraph\":null"],
    )
    .await?;
    let created = store
        .inspect_workflow_authoring(repo.project_id, from_scratch.id)
        .await?;
    assert!(created.base.is_none());
    assert_eq!(created.head.unwrap().graph.name, "New pipeline");
    Ok(())
}
