use std::{fs, process::Command};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, JobRequest, PipelineDispatch, PipelineNodeKind,
    },
    providers::{
        Provider, ProviderEvent, ProviderEventData, RequestId, SessionMode,
        questions::{QuestionAction, QuestionForm, QuestionOrigin, QuestionReply, QuestionRoute},
    },
    repository,
    runner::DirectWindowsRunner,
    store::Store,
    tasks::{TaskDetails, TaskRunScope, TaskWorkflowState},
};

#[tokio::test]
async fn task_keeps_the_originating_child_question_as_its_blocker_after_expiry_and_restart()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("tasks.sqlite");
    let root = temporary.path().join("repository");
    fs::create_dir(&root)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    let store = Store::open(database.clone())?;
    let checkout = repository::register(&store, root.clone()).await?;
    let project = checkout.project_id;
    let task = store
        .create_project_task(project.clone(), "Update the contract".into())
        .await?;
    let other = store
        .create_project_task(project.clone(), "Unrelated work".into())
        .await?;
    store
        .set_project_task_details(
            project.clone(),
            task.id.clone(),
            TaskDetails {
                workflow_state: TaskWorkflowState::InProgress,
                ..Default::default()
            },
        )
        .await?;
    store
        .register_job_pipeline(project.clone(), "delivery".into())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            "delivery".into(),
            "review".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    let request = JobRequest {
        checkout_id: Some(checkout.checkout_id),
        task_id: Some(task.id.clone()),
        mode: SessionMode::ReadOnly,
        instructions: "Review the change".into(),
        dependencies: vec![],
        requires_bridge: false,
    };
    store
        .submit_pipeline_task_agent_job(
            PipelineDispatch {
                pipeline_run_id: "delivery".into(),
                node_instance_id: "review".into(),
                plan_revision: None,
                release_id: None,
            },
            SessionScope {
                provider: Provider::Codex,
                profile_id: "fixture".into(),
                account_binding: None,
                checkout: dunce::canonicalize(root)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner::IDENTITY.into(),
                permission_revision: "read-only".into(),
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            },
            request.clone(),
            TaskRunScope {
                role: "reviewer".into(),
                checklist_item_id: None,
            },
        )
        .await?;
    let environment = AdmissionEnvironment {
        runner: DirectWindowsRunner::IDENTITY.into(),
        bridge_available: false,
    };
    let parent = store
        .claim_next_job(environment.clone())
        .await?
        .context("parent review did not start")?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    let child_job = store
        .submit_agent_job(
            parent.run.id,
            "compatibility-review".into(),
            JobRequest {
                task_id: None,
                instructions: "Check compatibility".into(),
                ..request
            },
        )
        .await?;
    assert_eq!(child_job.task_id.as_deref(), Some(task.id.as_str()));
    let inspection = store
        .inspect_job(project.clone(), child_job.id, environment.clone())
        .await?
        .unwrap();
    let [AdmissionBlocker::Approval { approval_id }] = inspection.blockers.as_slice() else {
        anyhow::bail!("expected child job approval, got {:?}", inspection.blockers);
    };
    store
        .decide_job_approval(project.clone(), approval_id.clone(), true)
        .await?;
    let child = store
        .claim_next_job(environment.clone())
        .await?
        .context("managed child did not start")?;
    assert_ne!(child.conversation_id, parent.conversation_id);
    let run = child.run.id.clone();
    let conversation = child.conversation_id;
    store.mark_agent_running(run.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 1, "contextId": child.context_id, "scope": child.scope, "baseCommit": null,
        "submittedMessages": child.input, "nativeHistory": { "mode": "fresh" },
        "taskContext": store.task_dispatch_context(run.clone()).await?,
    }))?;
    let dispatch = store
        .prepare_dispatch(
            run.clone(),
            snapshot,
            vec![b"compatibility fixture".to_vec()],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    let request_id = RequestId::Text("9".into());
    let form = json!([{"id":"coverage","question":"What coverage should be checked?","options":[{"label":"Compatibility"}]}]);
    store
        .record_agent_observation(
            run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::Question {
                    request_id: request_id.clone(),
                    normalized: Some(QuestionForm::codex(&form)?),
                    form,
                    route: QuestionRoute::Native,
                },
            },
        )
        .await?;
    let waiting = store
        .project_task_delivery_state(project.clone(), task.id.clone())
        .await?
        .unwrap();
    assert_eq!(waiting.state, TaskWorkflowState::Blocked);
    let exact = waiting
        .question_blocker
        .context("the child question should block its task")?;
    assert_eq!(
        (&exact.run_id, &exact.conversation_id, &exact.request_id),
        (&run, &conversation, &request_id)
    );
    assert_eq!(exact.state, "live");
    assert!(
        store
            .project_task_delivery_state(project.clone(), other.id)
            .await?
            .unwrap()
            .question_blocker
            .is_none()
    );

    store
        .record_agent_observation(
            run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::QuestionClosed {
                    request_id: request_id.clone(),
                },
            },
        )
        .await?;
    assert_eq!(
        store
            .project_task_delivery_state(project.clone(), task.id.clone())
            .await?
            .unwrap()
            .question_blocker
            .unwrap()
            .state,
        "saving"
    );
    drop(store);
    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let saved = store
        .project_task_delivery_state(project.clone(), task.id.clone())
        .await?
        .unwrap();
    assert_eq!(saved.state, TaskWorkflowState::Blocked);
    let saved = saved
        .question_blocker
        .context("the recovered form should retain its task link")?;
    assert_eq!(
        (&saved.run_id, &saved.conversation_id, &saved.request_id),
        (&run, &conversation, &request_id)
    );
    assert_eq!(saved.state, "saved");
    let questions = store
        .saved_conversation_questions(conversation.clone(), run.clone())
        .await?;
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].reason, "provider_expired");

    store
        .answer_saved_question(
            conversation,
            QuestionOrigin {
                run_id: run.clone(),
                request_id,
            },
            QuestionReply {
                action: QuestionAction::Accept,
                values: serde_json::from_value(json!({"coverage":"Compatibility"}))?,
            },
        )
        .await?;
    let recorded = store
        .project_task_delivery_state(project.clone(), task.id.clone())
        .await?
        .unwrap();
    assert!(recorded.question_blocker.unwrap().answer_recorded);
    let resumed = store
        .claim_next_job(environment)
        .await?
        .context("saved question did not resume")?;
    assert_eq!(resumed.previous_run_id.as_deref(), Some(run.as_str()));
    let continuing = store
        .project_task_delivery_state(project, task.id)
        .await?
        .unwrap();
    assert_eq!(continuing.state, TaskWorkflowState::InProgress);
    assert!(continuing.question_blocker.is_none());
    Ok(())
}
