use std::{fs, process::Command, time::Duration};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio::time::timeout;

use super::JobNoticeKind;
use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::{
        AdmissionEnvironment, JobRequest, PipelineAgentInput, PipelineDispatch, QuestionTimer,
        now_ms,
    },
    process_io::{OutputFrame, Stream},
    providers::{
        AdapterUpdate, CliAdapter, CliSession, Provider, ProviderEvent, ProviderEventData,
        ProviderLocation, RequestId, SessionIntent, SessionMode, SessionRequest, TurnInput,
        claude::ClaudeAdapter,
    },
    repository,
    runner::DirectWindowsRunner,
    runs::RunStatus,
    store::Store,
    workflows::{
        DeliveryRoles, NodeInputs, WorkflowAgentSubmission, WorkflowHostRequest,
        WorkflowHostSubmission,
    },
};

#[tokio::test]
async fn subagent_question_from_first_planning_stage_reaches_desktop_notices_after_restart()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    fs::create_dir(&root)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    let brief = root.join("brief.md");
    fs::write(&brief, "# Release brief")?;
    let database = temp.path().join("planner.sqlite");
    let store = Store::open(database.clone())?;
    let repo = repository::register(&store, root.clone()).await?;
    let project = repo.project_id;
    let document = store
        .capture_repository_markdown(repo.checkout_id.clone(), brief)
        .await?;
    let workflow = store
        .start_builtin_delivery(project.clone(), DeliveryRoles::default())
        .await?;
    let start = store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: project.clone(),
            run_id: workflow.id.clone(),
            node_id: "start".into(),
            inputs: NodeInputs::new(),
            request: WorkflowHostRequest::Start {
                document: document.revision.clone(),
            },
        })
        .await?;
    let loaded = store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: project.clone(),
            run_id: workflow.id.clone(),
            node_id: "context".into(),
            inputs: NodeInputs::from([("document".into(), start.id)]),
            request: WorkflowHostRequest::LoadContext,
        })
        .await?;
    let artifacts = NodeInputs::from([("context".into(), loaded.id)]);
    let target = store
        .workflow_operator_agent_target(&project, &workflow.id, "plan", None, &artifacts)
        .await?;
    assert_eq!(target.mode, SessionMode::Plan);
    let job = store
        .submit_workflow_agent(WorkflowAgentSubmission {
            project_id: project.clone(),
            dispatch: PipelineDispatch {
                pipeline_run_id: workflow.id,
                node_instance_id: "plan".into(),
                plan_revision: target.plan_revision,
                release_id: target.release_id,
            },
            scope: SessionScope {
                provider: Provider::Claude,
                profile_id: "fixture".into(),
                account_binding: None,
                checkout: dunce::canonicalize(root)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner::IDENTITY.into(),
                permission_revision: "fixture".into(),
                mode: target.mode,
                model: None,
                effort: None,
            },
            request: JobRequest {
                checkout_id: Some(target.checkout_id),
                task_id: target.task_id,
                mode: target.mode,
                instructions: "Plan the release".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
            input: PipelineAgentInput::default(),
            artifacts,
            unit_id: None,
        })
        .await?;
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: DirectWindowsRunner::IDENTITY.into(),
            bridge_available: false,
        })
        .await?
        .context("first planning stage was not admitted")?;
    assert_eq!(attempt.scope.mode, SessionMode::Plan);
    let run = attempt.run.id.clone();
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 13, "contextId": attempt.context_id, "scope": attempt.scope,
        "submittedMessages": attempt.input, "nativeHistory": {"mode":"fresh"},
        "repositoryMarkdown": document, "questionRoute": attempt.question_route,
    }))?;
    store.mark_agent_running(run.clone()).await?;
    let mut adapter = ClaudeAdapter.create_session(SessionRequest {
        checkout: attempt.scope.checkout,
        intent: SessionIntent::Start,
        mode: attempt.scope.mode,
        model: None,
        effort: None,
    })?;
    adapter.start()?;
    receive(
        &mut *adapter,
        json!({
            "type":"control_response", "response":{"request_id":"omniagent-initialize", "subtype":"success",
                "response":{"current_permission_mode":"plan", "models":[]}}
        }),
    )?;
    let turn = adapter.send(TurnInput {
        attachments: Vec::new(),
        message_id: run.clone(),
        text: "Plan the release".into(),
    })?;
    receive(
        &mut *adapter,
        json!({
            "type":"system", "subtype":"init", "session_id":"session", "permissionMode":"plan"
        }),
    )?;
    let frames = turn
        .outbound
        .iter()
        .map(|frame| frame.as_bytes().to_vec())
        .collect();
    let dispatch = store
        .prepare_dispatch(run.clone(), snapshot, frames)
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;

    let mut notices = store.job_notices();
    let opening = receive(&mut *adapter, json!({
        "type":"control_request", "request_id":"42", "session_id":"session",
        "request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"form",
            "parent_tool_use_id":"planning-subagent", "input":{"questions":[
                {"question":"What format should the release use?","options":[{"label":"Markdown"},{"label":"JSON"}]},
                {"question":"What should the release note cover?"}
            ]}}
    }))?.events.pop().context("native form was not decoded")?;
    let conversation = attempt.conversation_id;
    let request_id = RequestId::Text("42".into());
    store
        .record_agent_observation(run.clone(), EventSource::Provider, opening.clone())
        .await?;
    let current = store
        .latest_conversation_run(conversation.clone())
        .await?
        .context("planning run was not available in its conversation")?;
    assert_eq!(current.id, run);
    assert_eq!(current.status, RunStatus::WaitingForInput);
    let first_notice = timeout(Duration::from_secs(3), notices.next()).await??;
    assert_eq!(first_notice.kind, JobNoticeKind::Question);
    assert_eq!(first_notice.project_id, project);
    assert_eq!(first_notice.job_id, job.id);
    assert_eq!(first_notice.run_id.as_deref(), Some(run.as_str()));
    assert!(first_notice.request_event_seq.is_some());
    let timeline = store
        .conversation_question_timeline(conversation.clone(), run.clone())
        .await?;
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].provider, Provider::Claude);
    assert_eq!(
        timeline[0].parent_item_id.as_deref(),
        Some("planning-subagent")
    );
    assert_eq!(timeline[0].origin.request_id, request_id);

    let mismatched_cancel = receive(
        &mut *adapter,
        json!({
            "type":"control_cancel_request", "request_id":"42", "session_id":"session", "parent_tool_use_id":"other-subagent"
        }),
    )?;
    assert!(mismatched_cancel.events.is_empty());
    store
        .record_agent_observation(
            run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::QuestionClosed {
                    request_id: request_id.clone(),
                },
            },
        )
        .await?;
    assert_eq!(
        store
            .pending_conversation_inputs(conversation.clone(), run.clone())
            .await?,
        vec![opening]
    );
    assert!(matches!(
        store.poll_question_timer(run.clone(), now_ms()).await?,
        QuestionTimer::WakeAt(_)
    ));

    let closing = receive(
        &mut *adapter,
        json!({
            "type":"control_cancel_request", "request_id":"42", "session_id":"session"
        }),
    )?
    .events
    .pop()
    .context("Claude did not resolve the outstanding subagent form")?;
    assert_eq!(
        closing.location.parent_item_id.as_deref(),
        Some("planning-subagent")
    );
    store
        .record_agent_observation(run.clone(), EventSource::Provider, closing)
        .await?;
    assert!(matches!(
        store.poll_question_timer(run.clone(), now_ms()).await?,
        QuestionTimer::Stop
    ));
    drop(notices);
    drop(store);
    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let saved = store
        .saved_conversation_questions(conversation, run)
        .await?;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].reason, "provider_expired");
    let mut notices = store.job_notices();
    let recovered = timeout(Duration::from_secs(3), notices.next()).await??;
    assert_eq!(recovered, first_notice);
    notices.acknowledge(&recovered).await?;
    Ok(())
}

fn receive(adapter: &mut dyn CliSession, value: Value) -> Result<AdapterUpdate> {
    adapter.receive(OutputFrame {
        stream: Stream::Stdout,
        data: value.to_string(),
    })
}
