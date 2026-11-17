use serde_json::json;

use super::*;
use crate::{
    conversations::ConversationPipelineChoice,
    workflows::{
        WorkflowGraph,
        decision_nodes::{WorkflowDecisionSubmission, WorkflowStageDecision},
        tests::approved_delivery_repository,
    },
};

async fn publish_fixture(
    store: &Store,
    project: &str,
    id: &str,
    graph: WorkflowGraph,
) -> Result<WorkflowRevision> {
    let project = project.to_owned();
    let workflow = store
        .save_workflow_revision(project.clone(), id.to_owned(), graph)
        .await?;
    let revision = workflow.clone();
    store
        .execute(move |db| {
            db.execute(
                "INSERT INTO workflow_published_heads VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id, workflow_id) DO UPDATE SET revision = excluded.revision",
                params![project, revision.id, revision.revision],
            )?;
            Ok(())
        })
        .await?;
    Ok(workflow)
}

#[tokio::test]
async fn conversation_pipeline_turns_pin_previous_output_and_resume_in_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let graph = |name| {
        serde_json::from_value(json!({
            "name": name,
            "nodes": [
                {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[
                    {"name":"message","valueType":"text","required":true,"conversationSource":"message"},
                    {"name":"history","valueType":"array","required":true,"conversationSource":"history"}
                ]}},
                {"id":"text","kind":"build_text","configuration":{"kind":"build_text","template":"Result for {{message}}"}},
                {"id":"result","kind":"summarize_result","configuration":{"kind":"none"}}
            ],
            "wires":[
                {"from":{"node":"start","port":"start"},"to":{"node":"text","port":"ready"}},
                {"from":{"node":"start","port":"inputs"},"to":{"node":"text","port":"values"}},
                {"from":{"node":"text","port":"completed"},"to":{"node":"result","port":"ready"}},
                {"from":{"node":"text","port":"text"},"to":{"node":"result","port":"summary"}}
            ]
        }))
    };
    let workflow =
        publish_fixture(&store, &repo.project_id, "typed-chat", graph("Original")?).await?;
    let chat = store
        .create_conversation_with_pipeline(
            repo.checkout_id,
            "Workflow chat".into(),
            ConversationPipelineChoice::Published {
                workflow: workflow.clone(),
            },
        )
        .await?;
    let request = |id: &str, text: &str| ConversationPipelineTurnRequest {
        conversation_id: chat.id.clone(),
        request_id: id.into(),
        text: text.into(),
        submitted_draft: None,
        inputs: None,
        selection: None,
        effort: None,
        reference_ids: Vec::new(),
        action: None,
    };
    let first = request("once", "first message");
    let (left, right) = tokio::join!(
        store.enqueue_conversation_pipeline_turn(first.clone()),
        store.enqueue_conversation_pipeline_turn(first.clone()),
    );
    assert_eq!(left?.state, "queued");
    assert_eq!(right?.state, "queued");
    assert!(
        store
            .enqueue_conversation_pipeline_turn(request("once", "different"))
            .await
            .is_err()
    );
    assert_eq!(
        store
            .enqueue_conversation_pipeline_turn(request("next", "second message"))
            .await?
            .state,
        "queued"
    );
    assert!(store.advance_conversation_pipeline_turns().await?);
    assert_eq!(
        store.conversation_pipeline_turns(chat.id.clone()).await?[1].state,
        "queued"
    );
    let next = publish_fixture(
        &store,
        &repo.project_id,
        &workflow.id,
        graph("New publication")?,
    )
    .await?;
    assert_ne!(workflow, next);
    drop(store);

    let store = Store::open(database)?;
    assert!(store.advance_conversation_pipeline_turns().await?);
    assert!(store.advance_conversation_pipeline_turns().await?);
    let turns = store.conversation_pipeline_turns(chat.id.clone()).await?;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].state, "succeeded");
    assert_eq!(turns[1].state, "succeeded");
    assert!(turns.iter().all(|turn| turn.result_revision_id.is_some()));
    let second = turns[1].run_id.clone().context("second workflow run")?;
    let receipt = store
        .workflow_launch(repo.project_id, second)
        .await?
        .context("second launch receipt")?;
    assert_eq!(receipt.version, workflow);
    assert_eq!(receipt.inputs["message"], "second message");
    assert_eq!(
        receipt.inputs["history"],
        json!([
            {"role":"user", "text":"first message"},
            {"role":"assistant", "text":"Result for first message"}
        ])
    );
    store
        .save_conversation_draft(chat.id.clone(), "new draft".into())
        .await?;
    store
        .enqueue_conversation_pipeline_turn(first.clone())
        .await?;
    assert_eq!(
        store.conversation_draft(chat.id.clone()).await?,
        "new draft"
    );
    store
        .save_conversation_draft(chat.id.clone(), first.text.clone())
        .await?;
    assert_eq!(
        store
            .enqueue_conversation_pipeline_turn(first)
            .await?
            .run_id,
        turns[0].run_id
    );
    assert!(store.conversation_draft(chat.id).await?.is_empty());
    Ok(())
}

#[test]
fn conversation_context_preserves_literal_actions_without_activating_them() -> Result<()> {
    let marker = format!("@command:{}:{}", "a".repeat(64), "b".repeat(64));
    let payload = stage_context_json(&marker)?;
    assert!(!payload.contains("@command:"));
    assert_eq!(serde_json::from_str::<String>(&payload)?, marker);
    Ok(())
}

#[tokio::test]
async fn chat_turn_pins_an_agile_event_and_returns_approval_and_artifact_links() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("store.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id;
    let task = store
        .create_project_task(project.clone(), "Approval request".into())
        .await?;
    let event = store
        .project_work_events(project.clone(), None, 10)
        .await?
        .into_iter()
        .find(|event| event.task_id == task.id)
        .context("Missing saved Agile event")?;
    let graph = serde_json::from_value(json!({
        "name": "Approve the work event",
        "nodes": [
            {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[]}},
            {"id":"approve","kind":"approval","configuration":{"kind":"approval","message":"Review this Agile event."}},
            {"id":"text","kind":"build_text","configuration":{"kind":"build_text","template":"Recorded approval"}}
        ],
        "wires": [
            {"from":{"node":"start","port":"start"},"to":{"node":"approve","port":"ready"}},
            {"from":{"node":"start","port":"event"},"to":{"node":"approve","port":"evidence"}},
            {"from":{"node":"approve","port":"approved"},"to":{"node":"text","port":"ready"}}
        ]
    }))?;
    let workflow = publish_fixture(&store, &project, "approve-event", graph).await?;
    let chat = store
        .create_conversation_with_pipeline(
            repo.checkout_id,
            "Event review".into(),
            ConversationPipelineChoice::Published {
                workflow: workflow.clone(),
            },
        )
        .await?;
    let schema = store.conversation_pipeline_schema(chat.id.clone()).await?;
    assert!(schema.requires_event && schema.requires_work);
    assert!(
        schema
            .events
            .iter()
            .any(|candidate| candidate.id == event.id)
    );
    let text = "Review the saved event".to_owned();
    store
        .save_conversation_draft(chat.id.clone(), text.clone())
        .await?;
    let mut request = ConversationPipelineTurnRequest {
        conversation_id: chat.id.clone(),
        request_id: "event-request".into(),
        text: text.clone(),
        submitted_draft: Some(text.clone()),
        inputs: Some(ConversationPipelineInputs {
            work_id: Some(task.id),
            agile_event_id: Some("missing-event".into()),
            ..Default::default()
        }),
        selection: None,
        effort: None,
        reference_ids: Vec::new(),
        action: None,
    };
    assert!(
        store
            .enqueue_conversation_pipeline_turn(request.clone())
            .await
            .is_err()
    );
    assert_eq!(store.conversation_draft(chat.id.clone()).await?, text);
    assert!(
        store
            .conversation_pipeline_turns(chat.id.clone())
            .await?
            .is_empty()
    );
    request.inputs.as_mut().unwrap().agile_event_id = Some(event.id.clone());
    store.enqueue_conversation_pipeline_turn(request).await?;
    assert!(store.advance_conversation_pipeline_turns().await?);
    let mut turns = store.conversation_pipeline_turns(chat.id.clone()).await?;
    let turn = turns.pop().context("Missing waiting turn")?;
    let run = turn.run_id.context("Missing linked execution")?;
    let wait = serde_json::to_value(turn.waiting)?;
    assert_eq!(wait["nodeId"], "approve");
    assert_eq!(wait["kind"], "approval");
    let execution = store
        .inspect_workflow_execution(project.clone(), run.clone())
        .await?
        .context("Missing execution")?;
    assert_eq!(execution.run.version, workflow);
    assert_eq!(
        execution
            .launch
            .context("Missing receipt")?
            .event
            .context("Missing pinned event")?
            .id,
        event.id
    );
    let decision = execution
        .pending_decisions
        .first()
        .context("Missing approval request")?;
    store
        .resolve_workflow_stage_decision(WorkflowDecisionSubmission {
            project_id: project.clone(),
            run_id: run.clone(),
            request_id: decision.request_id.clone(),
            decision: WorkflowStageDecision::Approval { approved: true },
        })
        .await?;
    assert!(store.advance_conversation_pipeline_turns().await?);
    let turn = store
        .conversation_pipeline_turns(chat.id)
        .await?
        .pop()
        .context("Missing completed turn")?;
    assert_eq!(turn.state, "succeeded");
    assert_eq!(turn.run_id.as_deref(), Some(run.as_str()));
    assert!(turn.result_revision_id.is_none());
    let artifacts = serde_json::to_value(turn.artifacts)?;
    let execution = store
        .inspect_workflow_execution(project, run)
        .await?
        .context("Missing completed execution")?;
    let result = execution
        .outputs
        .iter()
        .find(|output| output.current && output.artifact.node_instance_id == "text")
        .context("Missing rendered artifact")?;
    assert!(
        artifacts
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| artifact["nodeId"] == "text" && artifact["id"] == result.artifact.id)
    );
    Ok(())
}
