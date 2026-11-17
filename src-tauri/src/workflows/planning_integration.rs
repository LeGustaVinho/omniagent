use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    conversations::{AttemptPlan, Conversation},
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, JobContext, PipelineDispatch, PipelineNodeKind,
        ReleaseBlocker,
    },
    process_io::{OutputFrame, Stream},
    providers::{
        CliAdapter, CliSession, Provider, ProviderEvent, ProviderEventData, RequestId,
        SessionIntent, SessionMode, SessionRequest, TurnInput,
        antigravity::AntigravityAdapter,
        planning::PlanSource,
        questions::{QuestionAction, QuestionForm, QuestionOrigin, QuestionReply, QuestionRoute},
    },
    repository::{self, ManagedWorktree, ProjectCheckout, RepositoryMarkdown},
    runs::RunStatus,
    store::Store,
};

use super::{
    ArtifactValue, DeliveryRoles, NodeInputs, WorkflowAgentSubmission, WorkflowArtifact,
    WorkflowDeliveryUnit, WorkflowHostRequest, WorkflowHostSubmission, WorkflowPlanRevisionRequest,
    WorkflowRun,
    tests::{approved_delivery_repository, submission},
};

mod stage_plan;

const FIRST_PLAN: &str = r#"ORIGINAL_DELIVERY. Generate Markdown with examples.

```omniagent-plan
{"units":[{"purpose":"Add a Markdown report with examples","completionCriteria":["The report includes examples"]}]}
```"#;
const REVISED_PLAN: &str = r#"REVISED_DELIVERY. Generate the Markdown report with examples and a schema section for public fields.

```omniagent-plan
{"units":[{"purpose":"Add the report, examples and a schema section","completionCriteria":["The report includes the approved schema section"]}]}
```"#;

#[tokio::test]
async fn planning_questions_and_document_revision_survive_restart_and_bind_the_release()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("planning.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let workflow = store
        .start_builtin_delivery(project.clone(), DeliveryRoles::default())
        .await?;
    let env = AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    };
    let working_root = temp.path().join("managed");
    let markdown_file = Path::new(&repo.checkout_root).join("brief.md");
    let frozen = store
        .capture_repository_markdown(repo.checkout_id.clone(), markdown_file.clone())
        .await?;
    fs::write(
        &markdown_file,
        "Do not substitute this working copy revision",
    )?;
    let edited = store
        .capture_repository_markdown(repo.checkout_id.clone(), markdown_file)
        .await?;
    assert_ne!(frozen.revision, edited.revision);
    let start = store
        .execute_workflow_host(host(
            &repo,
            &workflow,
            "start",
            NodeInputs::new(),
            WorkflowHostRequest::Start {
                document: frozen.revision.clone(),
            },
        ))
        .await?;
    let context = store
        .execute_workflow_host(host(
            &repo,
            &workflow,
            "context",
            NodeInputs::from([("document".into(), start.id)]),
            WorkflowHostRequest::LoadContext,
        ))
        .await?;
    let conversation = store
        .create_conversation(repo.checkout_id.clone(), "Plan from the saved brief".into())
        .await?;
    let planning = planner(
        &repo,
        &workflow,
        &conversation,
        &context,
        "Plan the report from the versioned brief",
    );
    let generic_pipeline = "generic-read-only".to_owned();
    store
        .register_job_pipeline(project.clone(), generic_pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            generic_pipeline.clone(),
            "read".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    let generic = store
        .submit_pinned_pipeline_agent_job(
            PipelineDispatch {
                pipeline_run_id: generic_pipeline,
                node_instance_id: "read".into(),
                plan_revision: None,
                release_id: None,
            },
            planning.scope.clone(),
            planning.request.clone(),
            JobContext {
                conversation_id: conversation.id.clone(),
                context_id: conversation.context_id.clone(),
            },
            QuestionRoute::StructuredTurn,
        )
        .await?;
    let inspected = store
        .inspect_job(project.clone(), generic.id, env.clone())
        .await?
        .context("generic pinned job disappeared")?;
    assert!(
        inspected
            .blockers
            .contains(&AdmissionBlocker::PipelineRelease {
                reason: ReleaseBlocker::StalePlan
            })
    );
    let job = store.submit_workflow_agent(planning).await?;
    let inspected = store
        .inspect_job(project.clone(), job.id.clone(), env.clone())
        .await?
        .context("built-in planner disappeared")?;
    assert!(inspected.blockers.is_empty(), "{:?}", inspected.blockers);

    let first = claim(&store, &env, "first planning round").await?;
    let first_dispatch = seal_dispatch(&store, &first, Some(&frozen)).await?;
    let format = record_question(
        &store,
        &first,
        "Which output format?",
        "format",
        json!({"type":"string","enum":["markdown","json"]}),
    )
    .await?;
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    answer(
        &store,
        &first.conversation_id,
        &format,
        "format",
        json!("markdown"),
    )
    .await?;
    let second = claim(&store, &env, "second planning round").await?;
    assert_eq!(second.previous_run_id.as_ref(), Some(&first.run.id));
    assert_eq!(second.answered_question.as_ref(), Some(&format));
    assert!(
        second
            .input
            .iter()
            .any(|message| message.text.contains(r#""format":"markdown""#))
    );
    let second_dispatch = seal_dispatch(&store, &second, Some(&frozen)).await?;
    let examples = record_question(
        &store,
        &second,
        "Include worked examples?",
        "include_examples",
        json!({"type":"boolean"}),
    )
    .await?;
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    answer(
        &store,
        &first.conversation_id,
        &examples,
        "include_examples",
        json!(true),
    )
    .await?;
    drop(store);

    let store = Store::open(database.clone())?;
    store.recover_agent_attempts_after_restart().await?;
    let complete = claim(&store, &env, "answered planning round").await?;
    assert_eq!(complete.previous_run_id.as_ref(), Some(&second.run.id));
    assert_eq!(complete.answered_question.as_ref(), Some(&examples));
    assert!(
        complete
            .input
            .iter()
            .any(|message| message.text.contains(r#""format":"markdown""#))
    );
    assert!(
        complete
            .input
            .iter()
            .any(|message| message.text.contains(r#""include_examples":true"#))
    );
    for (attempt, ordinal) in [(&first, 1), (&second, 2), (&complete, 3)] {
        let pinned = store
            .workflow_attempt(project.clone(), attempt.run.id.clone())
            .await?
            .context("planner attempt lost its built-in node")?;
        assert_eq!(pinned.job_id, job.id);
        assert_eq!(pinned.ordinal, ordinal);
        assert_eq!(pinned.pin.workflow_run_id, workflow.id);
        assert_eq!(pinned.pin.node_instance_id, "plan");
    }
    let final_dispatch = seal_dispatch(&store, &complete, Some(&frozen)).await?;
    let proposal = publish_plan(&store, &repo, &workflow, &complete, FIRST_PLAN).await?;
    store
        .set_job_pipeline_plan(
            project.clone(),
            workflow.id.clone(),
            None,
            proposal.id.clone(),
        )
        .await?;
    let task = store
        .create_project_task(project.clone(), "Deliver the report".into())
        .await?;
    let approved = release_plan(
        &store,
        &repo,
        &workflow,
        &env,
        &working_root,
        &task.id,
        &proposal,
    )
    .await?;
    let mut bypass = implementation(&repo, &workflow, &proposal, &approved)?;
    bypass.dispatch.release_id = None;
    bypass.artifacts.remove("release");
    let missing_release = store.submit_workflow_agent(bypass).await.unwrap_err();
    assert!(
        missing_release
            .to_string()
            .contains("workflow input release needs an artifact")
    );
    let mut shortcut = implementation(&repo, &workflow, &proposal, &approved)?;
    shortcut.dispatch.release_id = None;
    let untyped = store
        .submit_pipeline_agent_job(shortcut.dispatch, shortcut.scope, shortcut.request)
        .await
        .unwrap_err();
    assert!(
        untyped
            .to_string()
            .contains("typed workflow jobs require verified inputs")
    );
    assert!(store.claim_next_job(env.clone()).await?.is_none());

    let mut writer = implementation(&repo, &workflow, &proposal, &approved)?;
    writer.scope.profile_id = "original-implementation-profile".into();
    let original_job = store.submit_workflow_agent(writer).await?;
    let original = claim(&store, &env, "first released implementation").await?;
    assert_eq!(original.scope.profile_id, "original-implementation-profile");
    let original_dispatch = observe_implementation(
        &store,
        &original,
        &original_job.id,
        &approved,
        "ORIGINAL_DELIVERY",
        "REVISED_DELIVERY",
    )
    .await?;

    let mut revision_submission = submission(&repo, &workflow, "plan", None, SessionMode::Plan);
    revision_submission.scope.provider = Provider::Antigravity;
    revision_submission.scope.profile_id = "selected-revision-profile".into();
    revision_submission.artifacts = NodeInputs::from([("context".into(), context.id.clone())]);
    let requested = WorkflowPlanRevisionRequest {
        base_revision_id: proposal.id.clone(),
        expected_plan_revision: Some(proposal.id.clone()),
        feedback: "Keep the Markdown output and examples the operator selected. Add a schema section and clarify its scope with the operator.".into(),
    };
    let revision_job = store
        .request_workflow_plan_revision(revision_submission, requested.clone())
        .await?;
    let revision = claim(&store, &env, "revision after approval").await?;
    assert_eq!(revision.scope.profile_id, "selected-revision-profile");
    assert_eq!(revision.scope.provider, Provider::Antigravity);
    assert_eq!(revision.scope.mode, SessionMode::Plan);
    let revision_pin = store
        .workflow_attempt(project.clone(), revision.run.id.clone())
        .await?
        .context("revision lost its pinned planning stage")?;
    assert_eq!(revision_pin.job_id, revision_job.id);
    assert_eq!(revision_pin.pin.release_id, None);
    let origin =
        native_revision_question(&store, &env, &revision, &frozen, &requested.feedback).await?;
    let revised = persist_proposed_plan(&store, &repo, &workflow, &revision, REVISED_PLAN).await?;
    assert_ne!(proposal.id, revised.id);
    let ArtifactValue::Release(old_release) = &approved.release.value else {
        anyhow::bail!("original release is unavailable");
    };
    assert_eq!(
        store
            .workflow_approval_reference(
                project.clone(),
                workflow.id.clone(),
                old_release.release.id.clone()
            )
            .await?,
        *old_release
    );
    let stale_writer = store
        .submit_workflow_agent(implementation(&repo, &workflow, &proposal, &approved)?)
        .await?;
    store
        .set_job_pipeline_plan(
            project.clone(),
            workflow.id.clone(),
            Some(proposal.id.clone()),
            revised.id.clone(),
        )
        .await?;
    let stale = store
        .inspect_job(project.clone(), stale_writer.id, env.clone())
        .await?
        .context("revoked writer disappeared from queue inspection")?;
    assert!(stale.blockers.contains(&AdmissionBlocker::PipelineRelease {
        reason: ReleaseBlocker::StalePlan
    }));
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    assert!(
        store
            .execute_workflow_host(host(
                &repo,
                &workflow,
                "prepare",
                NodeInputs::from([
                    ("task".into(), approved.task.id.clone()),
                    ("decision".into(), approved.decision.id.clone())
                ]),
                WorkflowHostRequest::PrepareWorktree {
                    default_root: working_root.clone()
                }
            ))
            .await
            .is_err()
    );
    assert!(
        store
            .submit_workflow_agent(implementation(&repo, &workflow, &revised, &approved)?)
            .await
            .is_err()
    );

    let reapproved = release_plan(
        &store,
        &repo,
        &workflow,
        &env,
        &working_root,
        &task.id,
        &revised,
    )
    .await?;
    assert_ne!(reapproved.decision.id, approved.decision.id);
    assert_ne!(reapproved.release.id, approved.release.id);
    assert_eq!(
        reapproved.checkout.checkout_id,
        approved.checkout.checkout_id
    );
    assert_eq!(reapproved.unit.plan_revision, revised.id);
    let mut writer = implementation(&repo, &workflow, &revised, &reapproved)?;
    writer.scope.profile_id = "selected-revised-implementation-profile".into();
    writer.scope.model = Some("reviewed-write-model".into());
    writer.scope.effort = Some("high".into());
    let revised_job = store.submit_workflow_agent(writer).await?;
    let changed = claim(&store, &env, "newly released implementation").await?;
    assert_eq!(
        changed.scope.profile_id,
        "selected-revised-implementation-profile"
    );
    assert_eq!(changed.scope.model.as_deref(), Some("reviewed-write-model"));
    assert_eq!(changed.scope.effort.as_deref(), Some("high"));
    assert_eq!(changed.scope.mode, SessionMode::ApprovedWrite);
    let revised_dispatch = observe_implementation(
        &store,
        &changed,
        &revised_job.id,
        &reapproved,
        "REVISED_DELIVERY",
        "ORIGINAL_DELIVERY",
    )
    .await?;

    drop(store);
    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let revision_question = serde_json::to_value(
        store
            .conversation_question_timeline(
                revision.conversation_id.clone(),
                revision.run.id.clone(),
            )
            .await?,
    )?;
    assert_eq!(
        revision_question[0]["origin"],
        serde_json::to_value(origin)?
    );
    assert_eq!(revision_question[0]["route"], "native");
    assert_eq!(revision_question[0]["provider"], "antigravity");
    assert_eq!(revision_question[0]["state"], "submitted");
    assert_eq!(
        revision_question[0]["reply"]["fields"][0]["id"],
        "schema_scope"
    );
    assert_eq!(
        revision_question[0]["reply"]["fields"][0]["value"],
        "public_fields"
    );
    let history = store
        .workflow_plan_history(project.clone(), revision.run.id.clone())
        .await?
        .context("revision history did not survive restart")?;
    assert_eq!(history.request, Some(requested));
    let prior = history
        .versions
        .iter()
        .find(|version| version.revision_id == proposal.id)
        .context("the first reviewed plan was lost")?;
    assert_eq!(prior.release_revoked, Some(true));
    assert_eq!(
        prior.approval_artifact_id.as_ref(),
        Some(&approved.decision.id)
    );
    let current = history
        .versions
        .iter()
        .find(|version| version.revision_id == revised.id)
        .context("the revised plan was lost")?;
    assert!(current.current);
    assert_eq!(current.release_revoked, Some(false));
    assert_eq!(
        current.approval_artifact_id.as_ref(),
        Some(&reapproved.decision.id)
    );
    assert_question_history(
        &store,
        &first,
        &format,
        &second,
        "Which output format?",
        "format",
        json!("markdown"),
    )
    .await?;
    assert_question_history(
        &store,
        &second,
        &examples,
        &complete,
        "Include worked examples?",
        "include_examples",
        json!(true),
    )
    .await?;
    for (attempt, id, question) in [
        (&first, first_dispatch, None),
        (&second, second_dispatch, Some(&format)),
        (&complete, final_dispatch, Some(&examples)),
    ] {
        let evidence = store
            .agent_dispatch(attempt.conversation_id.clone(), attempt.run.id.clone())
            .await?
            .context("saved planning dispatch is missing after restart")?;
        assert_eq!(evidence.id, id);
        assert_eq!(
            evidence.snapshot.repository_markdown.as_ref(),
            Some(&frozen)
        );
        assert_eq!(evidence.snapshot.answered_question.as_ref(), question);
        let frame: Value = serde_json::from_slice(&evidence.frames[0])?;
        assert!(frame["text"].as_str().is_some_and(
            |text| text.contains(&frozen.markdown) && !text.contains(&edited.markdown)
        ));
    }
    for (attempt, id, token) in [
        (&original, original_dispatch, "ORIGINAL_DELIVERY"),
        (&changed, revised_dispatch, "REVISED_DELIVERY"),
    ] {
        let evidence = store
            .agent_dispatch(attempt.conversation_id.clone(), attempt.run.id.clone())
            .await?
            .context("saved implementation input is missing after restart")?;
        assert_eq!(evidence.id, id);
        assert!(evidence.snapshot.scope == attempt.scope);
        let frame: Value = serde_json::from_slice(&evidence.frames[0])?;
        assert!(
            frame["text"]
                .as_str()
                .is_some_and(|text| text.contains(token))
        );
    }
    let revoked = store
        .attempt_pipeline_origin(project.clone(), original.run.id)
        .await?
        .context("original implementation provenance is missing")?;
    assert_eq!(revoked.plan_revision.as_ref(), Some(&proposal.id));
    assert!(
        revoked
            .release
            .is_some_and(|release| release.revoked && release.plan_revision == proposal.id)
    );
    Ok(())
}

fn host(
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
    node: &str,
    inputs: NodeInputs,
    request: WorkflowHostRequest,
) -> WorkflowHostSubmission {
    WorkflowHostSubmission {
        project_id: repo.project_id.clone(),
        run_id: workflow.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    }
}

fn planner(
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
    conversation: &Conversation,
    context: &WorkflowArtifact,
    instructions: &str,
) -> WorkflowAgentSubmission {
    let mut plan = submission(repo, workflow, "plan", None, SessionMode::ReadOnly);
    plan.scope.provider = Provider::Claude;
    plan.scope.profile_id = "structured-planner".into();
    plan.request.instructions = instructions.into();
    plan.input.pinned = Some((
        JobContext {
            conversation_id: conversation.id.clone(),
            context_id: conversation.context_id.clone(),
        },
        QuestionRoute::StructuredTurn,
    ));
    plan.artifacts = NodeInputs::from([("context".into(), context.id.clone())]);
    plan
}

async fn claim(store: &Store, env: &AdmissionEnvironment, name: &str) -> Result<AttemptPlan> {
    store
        .claim_next_job(env.clone())
        .await?
        .with_context(|| format!("{name} was not admitted"))
}

async fn seal_dispatch(
    store: &Store,
    attempt: &AttemptPlan,
    document: Option<&RepositoryMarkdown>,
) -> Result<String> {
    let source = store
        .dispatch_repository_markdown(attempt.run.id.clone())
        .await?;
    assert_eq!(source.as_ref(), document);
    let mut input = attempt
        .input
        .last()
        .context("attempt has no input")?
        .text
        .clone();
    if let Some(document) = &source {
        document.append_to_prompt(&mut input)?;
    }
    store.mark_agent_running(attempt.run.id.clone()).await?;
    let snapshot = planning_snapshot(attempt, source.as_ref())?;
    let frame = serde_json::to_vec(&json!({"text": input}))?;
    let dispatch = store
        .prepare_dispatch(attempt.run.id.clone(), snapshot, vec![frame])
        .await?;
    store
        .settle_dispatch(dispatch.clone(), Delivery::Submitted)
        .await?;
    Ok(dispatch)
}

fn planning_snapshot(
    attempt: &AttemptPlan,
    document: Option<&RepositoryMarkdown>,
) -> Result<ContextSnapshot> {
    Ok(serde_json::from_value(json!({
        "version": 13,
        "contextId": attempt.context_id,
        "scope": attempt.scope,
        "submittedMessages": attempt.input,
        "nativeHistory": {"mode":"fresh"},
        "repositoryMarkdown": document,
        "questionRoute": attempt.question_route,
        "answeredQuestion": attempt.answered_question,
    }))?)
}

fn acp_reply(
    session: &mut dyn CliSession,
    mut frame: Value,
) -> Result<crate::providers::AdapterUpdate> {
    frame["jsonrpc"] = json!("2.0");
    session.receive(OutputFrame {
        stream: Stream::Stdout,
        data: frame.to_string(),
    })
}

async fn native_revision_question(
    store: &Store,
    env: &AdmissionEnvironment,
    attempt: &AttemptPlan,
    document: &RepositoryMarkdown,
    feedback: &str,
) -> Result<QuestionOrigin> {
    assert!(attempt.question_route.is_native());
    let run = &attempt.run.id;
    assert_eq!(
        store
            .dispatch_repository_markdown(run.clone())
            .await?
            .as_ref(),
        Some(document)
    );
    let mut session = AntigravityAdapter.create_session(SessionRequest {
        checkout: attempt.scope.checkout.clone(),
        mode: attempt.scope.mode,
        intent: SessionIntent::Start,
        model: attempt.scope.model.clone(),
        effort: attempt.scope.effort.clone(),
    })?;
    session.start()?;
    acp_reply(
        &mut *session,
        json!({"id":1,"result":{"protocolVersion":1}}),
    )?;
    let ready = acp_reply(
        &mut *session,
        json!({"id":2,"result":{"sessionId":"acp-revision","configOptions":[
            {"id":"mode","category":"mode","type":"select","currentValue":"plan","options":[{"value":"default"},{"value":"plan"}]}
        ]}}),
    )?;
    assert!(
        ready
            .events
            .iter()
            .any(|event| matches!(&event.data, ProviderEventData::PlanningModeConfirmed { .. }))
    );
    acp_reply(
        &mut *session,
        json!({"method":"session/update","params":{"sessionId":"acp-revision","update":{
            "sessionUpdate":"available_commands_update","availableCommands":[{"name":"plan","description":"Revise the plan"}]
        }}}),
    )?;

    let mut input = attempt
        .input
        .last()
        .context("revision instructions are missing")?
        .text
        .clone();
    document.append_to_prompt(&mut input)?;
    let turn = session.send(TurnInput {
        attachments: vec![],
        message_id: attempt.run.turn_id.clone(),
        text: input,
    })?;
    let prompt: Value = serde_json::from_slice(turn.outbound[0].as_bytes())?;
    let prompt_text = prompt["params"]["prompt"][0]["text"]
        .as_str()
        .context("ACP revision prompt is missing")?;
    assert!(
        prompt_text.contains(FIRST_PLAN)
            && prompt_text.contains(feedback)
            && prompt_text.contains(&document.markdown)
    );
    store.mark_agent_running(run.clone()).await?;
    let dispatch = store
        .prepare_dispatch(
            run.clone(),
            planning_snapshot(attempt, Some(document))?,
            turn.outbound
                .iter()
                .map(|frame| frame.as_bytes().to_vec())
                .collect(),
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    let request_id = RequestId::Number(73);
    let question = acp_reply(&mut *session, json!({"id":73,"method":"elicitation/create","params":{
        "sessionId":"acp-revision","mode":"form","message":"Which fields should the schema describe?",
        "requestedSchema":{"type":"object","required":["schema_scope"],"properties":{
            "schema_scope":{"type":"string","enum":["public_fields","all_fields"]}
        }}
    }}))?.events.into_iter().find(|event| matches!(&event.data, ProviderEventData::Question { .. }))
        .context("provider adapter did not emit the revision question")?;
    store
        .record_agent_observation(run.clone(), EventSource::Provider, question)
        .await?;
    assert_eq!(
        store
            .workflow_attempt(attempt.run.project_id.clone(), run.clone())
            .await?
            .context("revision attempt missing during the live question")?
            .status,
        RunStatus::WaitingForInput
    );
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    let values = json!({"schema_scope":"public_fields"});
    let reply =
        session.respond_question(&request_id, json!({"action":"accept","values":values}))?;
    let response: Value = serde_json::from_slice(reply.outbound[0].as_bytes())?;
    assert_eq!(
        response,
        json!({"jsonrpc":"2.0","id":73,"result":{"action":"accept","content":values}})
    );
    let prepared = reply
        .events
        .into_iter()
        .find(|event| matches!(&event.data, ProviderEventData::QuestionReplyPrepared { .. }))
        .context("native reply evidence is missing")?;
    store
        .record_agent_observation(run.clone(), EventSource::Application, prepared)
        .await?;
    store
        .record_agent_observation(
            run.clone(),
            EventSource::Application,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::QuestionReplySettled {
                    request_id: request_id.clone(),
                    action: QuestionAction::Accept,
                    delivery: Delivery::Submitted,
                },
            },
        )
        .await?;
    assert_eq!(
        store
            .workflow_attempt(attempt.run.project_id.clone(), run.clone())
            .await?
            .context("revision attempt missing after native reply")?
            .status,
        RunStatus::Running
    );

    acp_reply(
        &mut *session,
        json!({"method":"session/update","params":{"sessionId":"acp-revision","update":{
            "sessionUpdate":"agent_message_chunk","messageId":"revised-plan","content":{"type":"text","text":format!("<proposed_plan>{REVISED_PLAN}</proposed_plan>")}
        }}}),
    )?;
    let finished = acp_reply(
        &mut *session,
        json!({"id":prompt["id"],"result":{"stopReason":"end_turn"}}),
    )?;
    assert!(finished.events.iter().any(|event| matches!(&event.data,
        ProviderEventData::PlanProposed { text, source: PlanSource::AntigravityPlanCommand } if text == REVISED_PLAN)));
    for event in finished.events {
        store
            .record_agent_observation(run.clone(), EventSource::Provider, event)
            .await?;
    }
    Ok(QuestionOrigin {
        run_id: run.clone(),
        request_id,
    })
}

async fn record_question(
    store: &Store,
    attempt: &AttemptPlan,
    message: &str,
    field: &str,
    definition: Value,
) -> Result<QuestionOrigin> {
    let origin = QuestionOrigin {
        run_id: attempt.run.id.clone(),
        request_id: RequestId::Text(format!("structured:{}", attempt.run.turn_id)),
    };
    let schema = json!({"type":"object","properties":{(field):definition},"required":[field]});
    store
        .record_agent_observation(
            attempt.run.id.clone(),
            EventSource::Application,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::Question {
                    request_id: origin.request_id.clone(),
                    form: json!({"message": message, "schema": schema}),
                    normalized: Some(QuestionForm::schema(message.into(), &schema)?),
                    route: QuestionRoute::StructuredTurn,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            attempt.run.id.clone(),
            RunStatus::Succeeded,
            Some(message.into()),
        )
        .await?;
    let timeline = serde_json::to_value(
        store
            .conversation_question_timeline(attempt.conversation_id.clone(), attempt.run.id.clone())
            .await?,
    )?;
    assert_eq!(timeline[0]["state"], "saved");
    Ok(origin)
}

async fn answer(
    store: &Store,
    conversation: &str,
    origin: &QuestionOrigin,
    field: &str,
    value: Value,
) -> Result<()> {
    store
        .answer_saved_question(
            conversation.into(),
            origin.clone(),
            QuestionReply {
                action: QuestionAction::Accept,
                values: [(field.into(), value)].into_iter().collect(),
            },
        )
        .await
}

async fn publish_plan(
    store: &Store,
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
    attempt: &AttemptPlan,
    text: &str,
) -> Result<WorkflowArtifact> {
    store
        .record_agent_observation(
            attempt.run.id.clone(),
            EventSource::Application,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::PlanProposed {
                    text: text.into(),
                    source: PlanSource::StructuredTurn,
                },
            },
        )
        .await?;
    persist_proposed_plan(store, repo, workflow, attempt, text).await
}

async fn persist_proposed_plan(
    store: &Store,
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
    attempt: &AttemptPlan,
    text: &str,
) -> Result<WorkflowArtifact> {
    store
        .finish_agent_attempt(
            attempt.run.id.clone(),
            RunStatus::Succeeded,
            Some(text.into()),
        )
        .await?;
    let proposal = store
        .workflow_plan_proposals(
            repo.project_id.clone(),
            workflow.id.clone(),
            attempt.run.id.clone(),
        )
        .await?
        .into_iter()
        .find(|proposal| proposal.text == text)
        .context("planner output did not persist")?;
    store
        .publish_workflow_artifact(
            repo.project_id.clone(),
            workflow.id.clone(),
            "plan".into(),
            "plan".into(),
            ArtifactValue::Plan {
                run_id: attempt.run.id.clone(),
                event_sequence: proposal.sequence.parse()?,
            },
        )
        .await
}

struct ApprovedPlan {
    task: WorkflowArtifact,
    decision: WorkflowArtifact,
    prepared: WorkflowArtifact,
    release: WorkflowArtifact,
    checkout: ManagedWorktree,
    unit: WorkflowDeliveryUnit,
}

async fn release_plan(
    store: &Store,
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
    env: &AdmissionEnvironment,
    working_root: &Path,
    task: &str,
    plan: &WorkflowArtifact,
) -> Result<ApprovedPlan> {
    let mut review = submission(
        repo,
        workflow,
        "review-plan",
        Some(plan.id.clone()),
        SessionMode::ReadOnly,
    );
    review.scope.profile_id = "independent-codex-reviewer".into();
    review.artifacts = NodeInputs::from([("plan".into(), plan.id.clone())]);
    store.submit_workflow_agent(review).await?;
    let reviewed = claim(store, env, "cross-review").await?;
    assert!(
        reviewed
            .input
            .iter()
            .any(|message| message.text.contains("Workflow plan evidence"))
    );
    store.mark_agent_running(reviewed.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            reviewed.run.id.clone(),
            RunStatus::Succeeded,
            Some("Reviewed the exact plan".into()),
        )
        .await?;
    let review = store
        .publish_workflow_artifact(
            repo.project_id.clone(),
            workflow.id.clone(),
            "review-plan".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: reviewed.run.id,
            },
        )
        .await?;
    let linked = store
        .execute_workflow_host(host(
            repo,
            workflow,
            "work",
            NodeInputs::from([
                ("review".into(), review.id),
                ("plan".into(), plan.id.clone()),
            ]),
            WorkflowHostRequest::LinkExistingTask {
                task_id: task.into(),
            },
        ))
        .await?;
    let decision = store
        .execute_workflow_host(host(
            repo,
            workflow,
            "approve",
            NodeInputs::from([
                ("task".into(), linked.id.clone()),
                ("plan".into(), plan.id.clone()),
            ]),
            WorkflowHostRequest::ApproveImplementation,
        ))
        .await?;
    let prepared = store
        .execute_workflow_host(host(
            repo,
            workflow,
            "prepare",
            NodeInputs::from([
                ("task".into(), linked.id.clone()),
                ("decision".into(), decision.id.clone()),
            ]),
            WorkflowHostRequest::PrepareWorktree {
                default_root: working_root.to_owned(),
            },
        ))
        .await?;
    let release = store
        .workflow_release_artifact(
            repo.project_id.clone(),
            workflow.id.clone(),
            decision.id.clone(),
        )
        .await?
        .context("prepared plan has no release")?;
    let ArtifactValue::Release(reference) = &release.value else {
        anyhow::bail!("release has wrong type");
    };
    assert_eq!(reference.release.plan_revision, plan.id);
    assert_eq!(reference.decision_artifact_id, decision.id);
    assert_eq!(reference.release.task_id, task);
    let checkout = store
        .live_managed_worktree(repo.project_id.clone(), task.into())
        .await?
        .context("the prepared worktree is unavailable")?
        .worktree;
    assert_eq!(reference.release.checkout_id, checkout.checkout_id);
    let units = store
        .workflow_delivery_units(repo.project_id.clone(), task.into(), workflow.id.clone())
        .await?;
    let [unit] = units.as_slice() else {
        anyhow::bail!("release did not pin one approved unit");
    };
    Ok(ApprovedPlan {
        task: linked,
        decision,
        prepared,
        release,
        checkout,
        unit: unit.clone(),
    })
}

fn implementation(
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
    plan: &WorkflowArtifact,
    approved: &ApprovedPlan,
) -> Result<WorkflowAgentSubmission> {
    let ArtifactValue::Release(reference) = &approved.release.value else {
        anyhow::bail!("expected a release");
    };
    let mut writer = submission(
        repo,
        workflow,
        "implement",
        Some(plan.id.clone()),
        SessionMode::ApprovedWrite,
    );
    writer.scope.checkout = approved.checkout.root.clone();
    writer.request.checkout_id = Some(approved.checkout.checkout_id.clone());
    writer.request.task_id = Some(approved.unit.task_id.clone());
    writer.dispatch.release_id = Some(reference.release.id.clone());
    writer.unit_id = Some(approved.unit.unit.id.clone());
    writer.artifacts = NodeInputs::from([
        ("plan".into(), plan.id.clone()),
        ("worktree".into(), approved.prepared.id.clone()),
        ("release".into(), approved.release.id.clone()),
    ]);
    Ok(writer)
}

async fn observe_implementation(
    store: &Store,
    attempt: &AttemptPlan,
    job: &str,
    approved: &ApprovedPlan,
    expected: &str,
    excluded: &str,
) -> Result<String> {
    let pin = store
        .workflow_attempt(approved.unit.project_id.clone(), attempt.run.id.clone())
        .await?
        .context("implementation attempt has no workflow pin")?;
    assert_eq!(pin.job_id, job);
    assert_eq!(pin.pin.workflow_run_id, approved.unit.workflow_run_id);
    assert_eq!(
        pin.pin.plan_revision.as_ref(),
        Some(&approved.unit.plan_revision)
    );
    let ArtifactValue::Release(reference) = &approved.release.value else {
        anyhow::bail!("expected release");
    };
    assert_eq!(pin.pin.release_id.as_ref(), Some(&reference.release.id));
    let input = &attempt
        .input
        .last()
        .context("implementation prompt is missing")?
        .text;
    assert!(input.contains(expected) && !input.contains(excluded));
    assert!(repository::capture_worktree_start(store, attempt.run.id.clone()).await?);
    let dispatch = seal_dispatch(store, attempt, None).await?;
    repository::save_worktree_candidate(store, attempt.run.id.clone()).await?;
    store
        .finish_agent_attempt(attempt.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    Ok(dispatch)
}

async fn assert_question_history(
    store: &Store,
    question: &AttemptPlan,
    origin: &QuestionOrigin,
    next: &AttemptPlan,
    message: &str,
    field: &str,
    value: Value,
) -> Result<()> {
    let timeline = store
        .conversation_question_timeline(question.conversation_id.clone(), question.run.id.clone())
        .await?;
    assert_eq!(timeline.len(), 1);
    let record = &timeline[0];
    assert_eq!(&record.origin, origin);
    assert_eq!(record.provider, Provider::Claude);
    assert_eq!(record.route, QuestionRoute::StructuredTurn);
    assert_eq!(record.message.as_deref(), Some(message));
    assert_eq!(serde_json::to_value(record.state)?, "continued");
    assert_eq!(record.continuation_run_id.as_ref(), Some(&next.run.id));
    let reply = record
        .reply
        .as_ref()
        .context("persisted question is missing its answer")?;
    assert_eq!(reply.action, QuestionAction::Accept);
    assert_eq!(reply.fields.len(), 1);
    assert_eq!(reply.fields[0].id, field);
    assert_eq!(reply.fields[0].value.as_ref(), Some(&value));
    Ok(())
}
