use super::*;
use crate::{
    jobs::{JobPermission, JobRole, JobRule, JobWorkspace, SubagentLimitMode},
    providers::{Capability, capabilities::CapabilitySubject},
    workflows::{
        BuiltinNode, NodeConfiguration, builtin_delivery_graph, stage_plan::StagePlanConfiguration,
    },
};

fn planning_graph(route: QuestionRoute, maximum: u32) -> Result<crate::workflows::WorkflowGraph> {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let stage = graph
        .nodes
        .iter_mut()
        .find(|stage| stage.kind == BuiltinNode::Plan)
        .context("The graph needs a Plan stage")?;
    let NodeConfiguration::AgentRole { requirements, .. } = &mut stage.configuration else {
        anyhow::bail!("The Plan stage needs an agent role");
    };
    requirements.plan = Some(StagePlanConfiguration {
        question_route: route,
        max_subagents: maximum,
    });
    Ok(graph)
}

async fn planning_context(
    store: &Store,
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
) -> Result<(RepositoryMarkdown, WorkflowArtifact)> {
    let document = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let start = store
        .execute_workflow_host(host(
            repo,
            workflow,
            "start",
            NodeInputs::new(),
            WorkflowHostRequest::Start {
                document: document.revision.clone(),
            },
        ))
        .await?;
    let context = store
        .execute_workflow_host(host(
            repo,
            workflow,
            "context",
            NodeInputs::from([("document".into(), start.id)]),
            WorkflowHostRequest::LoadContext,
        ))
        .await?;
    Ok((document, context))
}

#[tokio::test]
async fn configured_plan_preserves_question_route_quota_and_exact_release_after_restart()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("stage-plan.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let project = &repo.project_id;
    let graph = planning_graph(QuestionRoute::StructuredTurn, 1)?;
    let primary = store
        .save_workflow_revision(project.clone(), "bounded-plan".into(), graph)
        .await?;
    let workflow = store
        .start_workflow_run(project.clone(), primary.clone())
        .await?;
    let (document, context) = planning_context(&store, &repo, &workflow).await?;
    let inputs = NodeInputs::from([("context".into(), context.id.clone())]);
    let target = store
        .workflow_operator_agent_target(project, &workflow.id, "plan", None, &inputs)
        .await?;
    assert_eq!(target.mode, SessionMode::ReadOnly);
    assert!(target.release_id.is_none());

    let mut wrong = submission(&repo, &workflow, "plan", None, SessionMode::Plan);
    wrong.artifacts = inputs.clone();
    assert!(
        store
            .submit_workflow_agent(wrong)
            .await
            .unwrap_err()
            .to_string()
            .contains("pinned agent executor")
    );
    let mut selected = submission(&repo, &workflow, "plan", None, target.mode);
    selected.artifacts = inputs.clone();
    selected.scope.provider = Provider::Claude;
    selected.scope.profile_id = "structured-plan".into();
    let job = store.submit_workflow_agent(selected).await?;
    let conversation = job
        .context
        .as_ref()
        .context("The stage did not bind a planning conversation")?
        .conversation_id
        .clone();
    let widened = store
        .set_conversation_subagent_quota(
            conversation.clone(),
            Some(SubagentLimitMode::Managed),
            12,
            Some(9),
        )
        .await?;
    assert_eq!(widened.authorized, 1);
    let env = AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    };
    let first = claim(&store, &env, "configured planner").await?;
    assert_eq!(first.conversation_id, conversation);
    assert_eq!(first.scope.mode, SessionMode::ReadOnly);
    assert_eq!(first.question_route, QuestionRoute::StructuredTurn);
    seal_dispatch(&store, &first, Some(&document)).await?;
    let question = record_question(
        &store,
        &first,
        "Which output format?",
        "format",
        json!({"type":"string"}),
    )
    .await?;
    let new_head = store
        .save_workflow_revision(
            project.clone(),
            primary.id,
            planning_graph(QuestionRoute::Native, 6)?,
        )
        .await?;
    assert_ne!(workflow.version.revision, new_head.revision);
    drop(store);

    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    assert_eq!(
        store
            .saved_conversation_questions(conversation.clone(), first.run.id.clone())
            .await?[0]
            .state,
        "saved"
    );
    answer(
        &store,
        &conversation,
        &question,
        "format",
        json!("markdown"),
    )
    .await?;
    let continued = claim(&store, &env, "configured planner continuation").await?;
    assert_eq!(continued.question_route, QuestionRoute::StructuredTurn);
    assert_eq!(continued.previous_run_id.as_ref(), Some(&first.run.id));
    assert_eq!(
        store
            .conversation_run_subagent_quota(conversation, continued.run.id.clone())
            .await?
            .authorized,
        1
    );
    seal_dispatch(&store, &continued, Some(&document)).await?;
    let plan = publish_plan(&store, &repo, &workflow, &continued, FIRST_PLAN).await?;
    assert_eq!(
        store
            .workflow_attempt(project.clone(), continued.run.id)
            .await?
            .context("Missing attempt pin")?
            .pin
            .workflow,
        workflow.version
    );
    store
        .set_job_pipeline_plan(project.clone(), workflow.id.clone(), None, plan.id.clone())
        .await?;
    let mut missing_release = submission(
        &repo,
        &workflow,
        "implement",
        Some(plan.id.clone()),
        SessionMode::ApprovedWrite,
    );
    missing_release.artifacts = NodeInputs::from([("plan".into(), plan.id.clone())]);
    assert!(store.submit_workflow_agent(missing_release).await.is_err());

    let work = store
        .create_project_task(project.clone(), "Implement the approved plan".into())
        .await?;
    let approved = release_plan(
        &store,
        &repo,
        &workflow,
        &env,
        &temporary.path().join("worktrees"),
        &work.id,
        &plan,
    )
    .await?;
    let writer = store
        .submit_workflow_agent(implementation(&repo, &workflow, &plan, &approved)?)
        .await?;
    let previous = store.project_job_rules(project.clone()).await?;
    let deny = store
        .set_project_job_rules(
            project.clone(),
            previous.clone(),
            vec![JobRule {
                role: JobRole::Pipeline,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Task { task_id: work.id },
                permission: JobPermission::Deny,
                requirements: Default::default(),
            }],
        )
        .await?;
    let blocked = store
        .inspect_job(project.clone(), writer.id.clone(), env.clone())
        .await?
        .context("The approved writer is unavailable for policy inspection")?;
    assert!(blocked.blockers.iter().any(|blocker| matches!(blocker,
        AdmissionBlocker::Policy { reason } if reason.contains("denies this role"))));
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    store
        .set_project_job_rules(project.clone(), deny, previous)
        .await?;
    let attempt = claim(&store, &env, "released implementation").await?;
    let pin = store
        .workflow_attempt(project.clone(), attempt.run.id)
        .await?
        .context("Missing writer pin")?;
    assert_eq!(pin.job_id, writer.id);
    assert_eq!(pin.pin.plan_revision.as_deref(), Some(plan.id.as_str()));
    assert_eq!(attempt.scope.mode, SessionMode::ApprovedWrite);
    assert!(pin.pin.release_id.is_some());
    Ok(())
}

#[tokio::test]
async fn configured_native_plan_requires_exact_capabilities_and_recovers_saved_questions()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("native-stage-plan.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let project = &repo.project_id;
    let version = store
        .save_workflow_revision(
            project.clone(),
            "native-plan".into(),
            planning_graph(QuestionRoute::Native, 0)?,
        )
        .await?;
    let workflow = store.start_workflow_run(project.clone(), version).await?;
    let (document, context) = planning_context(&store, &repo, &workflow).await?;
    let mut selected = submission(&repo, &workflow, "plan", None, SessionMode::Plan);
    let runner = crate::runner::DirectWindowsRunner::IDENTITY;
    selected.scope.runner = runner.into();
    selected.artifacts = NodeInputs::from([("context".into(), context.id)]);
    store.submit_workflow_agent(selected).await?;
    let env = AdmissionEnvironment {
        runner: runner.into(),
        bridge_available: false,
    };
    let first = claim(&store, &env, "native planning stage").await?;
    assert_eq!(first.question_route, QuestionRoute::Native);
    assert_eq!(first.scope.mode, SessionMode::Plan);
    let pin = store
        .stage_requirements_for_attempt(first.run.id.clone())
        .await?
        .context("Native requirements were not pinned")?;
    for capability in [
        Capability::NativePlanMode,
        Capability::PlanningQuestions,
        Capability::PlanningAnswers,
        Capability::NativePlanOutput,
    ] {
        assert!(
            pin.requirements
                .capabilities
                .contains(&CapabilitySubject::Protocol { capability })
        );
    }
    seal_dispatch(&store, &first, Some(&document)).await?;
    let request_id = RequestId::Number(41);
    let schema = json!({"type":"object","properties":{"audience":{"type":"string"}},"required":["audience"]});
    store
        .record_agent_observation(
            first.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::Question {
                    request_id: request_id.clone(),
                    form: json!({"message": "Who will read this?", "schema": schema}),
                    normalized: Some(QuestionForm::schema("Who will read this?".into(), &schema)?),
                    route: QuestionRoute::Native,
                },
            },
        )
        .await?;
    drop(store);
    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let questions = store
        .saved_conversation_questions(first.conversation_id.clone(), first.run.id.clone())
        .await?;
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].state, "saved");
    let origin = QuestionOrigin {
        run_id: first.run.id.clone(),
        request_id,
    };
    answer(
        &store,
        &first.conversation_id,
        &origin,
        "audience",
        json!("maintainers"),
    )
    .await?;
    let continued = claim(&store, &env, "native planning stage after its saved answer").await?;
    assert_eq!(continued.scope.mode, SessionMode::Plan);
    assert_eq!(continued.question_route, QuestionRoute::Native);
    assert_eq!(continued.answered_question.as_ref(), Some(&origin));
    assert_eq!(
        store
            .conversation_run_subagent_quota(first.conversation_id, continued.run.id)
            .await?
            .authorized,
        0
    );
    Ok(())
}
