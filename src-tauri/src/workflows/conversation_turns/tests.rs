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
async fn an_end_reply_settles_the_chat_while_another_branch_waits() -> Result<()> {
    use crate::workflows::{builtin_registry, compile_resolved_graph};

    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("chat.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let reply = json!({
        "type":"object", "properties":{"reply":{"type":"string"}},
        "required":["reply"], "additionalProperties":false
    });
    let mut catalog = builtin_registry();
    for node in &mut catalog {
        match node.kind {
            BuiltinNode::SetFields => {
                node.output_schemas.insert("object".into(), reply.clone());
            }
            BuiltinNode::End => {
                node.input_schemas.insert("result".into(), reply.clone());
                node.output_schemas.insert("result".into(), reply.clone());
            }
            _ => {}
        }
    }
    let graph = serde_json::from_value(json!({
        "name":"Early reply",
        "nodes":[
            {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","conversation":true,"fields":[{"name":"message","valueType":"text","required":true,"conversationSource":"message"}]}},
            {"id":"answer","kind":"set_fields","configuration":{"kind":"set_fields","mode":"build","operations":[{"op":"set","path":"/reply","value":{"source":"literal","value":"Recorded response"}}]}},
            {"id":"wait","kind":"request_input","configuration":{"kind":"request_input","message":"Review this run","fields":[{"id":"note","label":"Note","required":true,"input":{"kind":"text"}}]}},
            {"id":"end","kind":"end","configuration":{"kind":"end"}}
        ],
        "wires":[
            {"from":{"node":"start","port":"start"},"to":{"node":"answer","port":"ready"}},
            {"from":{"node":"start","port":"start"},"to":{"node":"wait","port":"ready"}},
            {"from":{"node":"answer","port":"completed"},"to":{"node":"end","port":"ready"}},
            {"from":{"node":"answer","port":"object"},"to":{"node":"end","port":"result"}}
        ]
    }))?;
    let definition = compile_resolved_graph(
        "early-reply".into(),
        graph,
        |node| catalog.iter().find(|entry| entry.kind == node.kind),
        true,
    )?;
    let workflow = definition.version.clone();
    let project = repo.project_id.clone();
    store
        .execute(move |db| {
            let version = &definition.version;
            db.execute(
                "INSERT INTO workflow_revisions VALUES (?1, ?2, ?3, ?4)",
                params![
                    project,
                    version.id,
                    version.revision,
                    protection::protect(&serde_json::to_vec(&definition)?)?
                ],
            )?;
            db.execute(
                "INSERT INTO workflow_published_heads VALUES (?1, ?2, ?3)",
                params![project, version.id, version.revision],
            )?;
            Ok(())
        })
        .await?;
    let choices = store
        .conversation_pipeline_options(repo.checkout_id.clone())
        .await?;
    assert!(choices.iter().any(|choice| choice.name == "Early reply"));
    let chat = store
        .create_conversation_with_pipeline(
            repo.checkout_id,
            "Early reply".into(),
            ConversationPipelineChoice::Published { workflow },
        )
        .await?;
    store
        .enqueue_conversation_pipeline_turn(ConversationPipelineTurnRequest {
            conversation_id: chat.id.clone(),
            request_id: "send-once".into(),
            text: "First turn".into(),
            submitted_draft: None,
            inputs: None,
            selection: None,
            effort: None,
            reference_ids: Vec::new(),
            action: None,
        })
        .await?;
    assert!(store.advance_conversation_pipeline_turns().await?);
    assert!(store.advance_conversation_pipeline_turns().await?);
    let mut turns = store.conversation_pipeline_turns(chat.id.clone()).await?;
    let turn = turns.pop().context("Missing recorded turn")?;
    assert_eq!(turn.state, "succeeded");
    let run = turn.run_id.context("Missing pinned run")?;
    let inspection = store
        .inspect_workflow_execution(repo.project_id.clone(), run.clone())
        .await?
        .context("Missing inspection")?;
    assert_eq!(inspection.pending_decisions.len(), 1);
    assert!(!store.advance_conversation_pipeline_turns().await?);
    store
        .resolve_workflow_stage_decision(WorkflowDecisionSubmission {
            project_id: repo.project_id,
            run_id: run,
            request_id: inspection.pending_decisions[0].request_id.clone(),
            decision: WorkflowStageDecision::Answer {
                values: [("note".into(), json!("Reviewed after reply"))].into(),
            },
        })
        .await?;
    assert!(store.advance_conversation_pipeline_turns().await?);
    assert!(!store.advance_conversation_pipeline_turns().await?);
    let messages = store.conversation_messages(chat.id).await?;
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["First turn", "Recorded response"]
    );
    Ok(())
}

#[tokio::test]
async fn direct_turn_pins_native_job_and_reuses_one_transcript_reply() -> Result<()> {
    use crate::{
        conversations::SessionScope,
        jobs::{AdmissionEnvironment, JobContext},
        providers::Provider,
        runs::RunStatus,
    };

    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("native-chat.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let chat = store
        .create_conversation_with_pipeline(
            repo.checkout_id,
            "Direct".into(),
            ConversationPipelineChoice::Direct,
        )
        .await?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "fixture".into(),
        account_binding: None,
        executable: std::env::current_exe()?,
        checkout: repo.checkout_root.into(),
        cli_version: "1.2.3".into(),
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
        runner: "fixture-runner".into(),
        permission_revision: "fixture-policy".into(),
    };
    store
        .save_conversation_draft(chat.id.clone(), "Native question".into())
        .await?;
    let job = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: chat.id.clone(),
                context_id: chat.context_id,
            },
            scope.clone(),
            "Native question".into(),
        )
        .await?;
    let initial = store.conversation_pipeline_turns(chat.id.clone()).await?;
    assert_eq!(initial[0].request_id, job.id);
    let pinned_run = initial[0]
        .run_id
        .clone()
        .context("Expected a pinned direct run")?;
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: scope.runner,
            bridge_available: false,
        })
        .await?
        .context("Expected the native job to be admitted")?;
    store.mark_agent_running(attempt.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            attempt.run.id,
            RunStatus::Succeeded,
            Some("Native reply".into()),
        )
        .await?;
    assert!(store.advance_conversation_pipeline_turns().await?);
    let turns = store.conversation_pipeline_turns(chat.id.clone()).await?;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].state, "succeeded");
    assert_eq!(turns[0].run_id.as_deref(), Some(pinned_run.as_str()));
    let messages = store.conversation_messages(chat.id).await?;
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["Native question", "Native reply"]
    );
    assert_eq!(
        turns[0].result_revision_id.as_deref(),
        Some(messages[1].revision_id.as_str())
    );
    let inspection = store
        .inspect_workflow_execution(repo.project_id, pinned_run)
        .await?
        .context("Expected the direct execution graph")?;
    assert!(inspection.control.iter().any(|node| node.node_id == "end"));
    assert!(inspection.jobs.iter().any(|entry| entry.job.id == job.id));
    Ok(())
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
                {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","promptOrMarkdown":true,"fields":[
                    {"name":"message","valueType":"text","required":true,"conversationSource":"message"},
                    {"name":"history","valueType":"array","required":true,"conversationSource":"history"}
                ]}},
                {"id":"text","kind":"build_text","configuration":{"kind":"build_text","template":"Result for {{request}}"}},
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
    assert!(receipt.automatic);
    assert!(receipt.source_commit.is_some());
    assert_eq!(receipt.inputs["prompt"], "second message");
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
