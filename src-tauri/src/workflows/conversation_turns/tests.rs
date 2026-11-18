use super::*;
use crate::{
    conversations::ConversationPipelineChoice,
    workflows::{WorkflowGraph, tests::approved_delivery_repository},
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
async fn direct_turn_keeps_the_revised_instruction_and_recovers_to_end() -> Result<()> {
    use crate::{
        conversations::SessionScope,
        jobs::{AdmissionEnvironment, JobContext, RecoveryAction},
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
    store
        .revise_pending_instruction(
            chat.id.clone(),
            job.id.clone(),
            initial[0]
                .message_revision_id
                .clone()
                .context("Expected an instruction")?,
            "Revised native question".into(),
            None,
        )
        .await?;
    let revised = store.conversation_pipeline_turns(chat.id.clone()).await?;
    assert_eq!(revised[0].text, "Revised native question");
    assert_ne!(
        revised[0].message_revision_id,
        initial[0].message_revision_id
    );
    let environment = AdmissionEnvironment {
        runner: scope.runner,
        bridge_available: false,
    };
    let first = store
        .claim_next_job(environment.clone())
        .await?
        .context("Expected the initial native attempt")?;
    store.mark_agent_running(first.run.id.clone()).await?;
    store
        .finish_agent_attempt(first.run.id.clone(), RunStatus::Failed, None)
        .await?;
    assert!(store.advance_conversation_pipeline_turns().await?);
    assert_eq!(
        store.conversation_pipeline_turns(chat.id.clone()).await?[0].state,
        "failed"
    );
    store
        .queue_job_recovery(
            repo.project_id.clone(),
            job.id.clone(),
            first.run.id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    let resumed = store.conversation_pipeline_turns(chat.id.clone()).await?;
    assert_eq!(resumed[0].state, "running");
    assert_eq!(resumed[0].run_id.as_deref(), Some(pinned_run.as_str()));
    let recovering = store
        .inspect_workflow_execution(repo.project_id.clone(), pinned_run.clone())
        .await?
        .context("Expected the reopened direct run")?;
    assert!(recovering.conversation_turn_active);
    assert!(!recovering.control.iter().any(|node| node.node_id == "end"));
    let attempt = store
        .claim_next_job(environment)
        .await?
        .context("Expected the native job to be admitted")?;
    assert_eq!(
        attempt.previous_run_id.as_deref(),
        Some(first.run.id.as_str())
    );
    store.mark_agent_running(attempt.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            attempt.run.id,
            RunStatus::Succeeded,
            Some("Native reply".into()),
        )
        .await?;
    let finishing = store
        .inspect_workflow_execution(repo.project_id.clone(), pinned_run.clone())
        .await?
        .context("Expected the completing direct run")?;
    assert!(finishing.conversation_turn_active);
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
        ["Revised native question", "Native reply"]
    );
    assert_eq!(
        turns[0].result_revision_id.as_deref(),
        Some(messages[1].revision_id.as_str())
    );
    assert_eq!(
        turns[0].message_revision_id.as_deref(),
        Some(messages[0].revision_id.as_str())
    );
    let inspection = store
        .inspect_workflow_execution(repo.project_id, pinned_run)
        .await?
        .context("Expected the direct execution graph")?;
    assert!(!inspection.conversation_turn_active);
    assert!(inspection.control.iter().any(|node| node.node_id == "end"
        && node.state == super::super::control::ControlStageState::Completed));
    let native = inspection
        .jobs
        .iter()
        .find(|entry| entry.job.id == job.id)
        .context("Expected the native job in the graph")?;
    assert_eq!(native.attempts.len(), 2);
    let source = inspection
        .outputs
        .iter()
        .find(|output| {
            output.artifact.node_instance_id == "start" && output.artifact.port == "inputs"
        })
        .context("Expected the Start input artifact")?;
    assert!(
        matches!(&source.artifact.value, super::super::ArtifactValue::JsonObject(value)
        if value["messageRevisionId"] == messages[0].revision_id)
    );
    assert_eq!(
        native
            .inputs
            .as_ref()
            .and_then(|inputs| inputs.get("input")),
        Some(&source.artifact.id)
    );
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
