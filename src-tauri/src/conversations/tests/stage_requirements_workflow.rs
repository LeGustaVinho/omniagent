use super::*;
use crate::{
    jobs::{AdmissionEnvironment, JobRequest, JobRole, PipelineDispatch, RecoveryAction},
    model_catalog::ModelSelection,
    providers::composer_customizations::current_skill,
    providers::skills::{SkillEntry, SkillSource},
    workflows::{
        DeliveryRoles, ManualWorkflowLaunch, NodeConfiguration, NodeInputs,
        WorkflowAgentSubmission, WorkflowLaunchSource, builtin_delivery_graph,
        role_templates::{
            ConversationTemplate, SaveConversationTemplate, TemplateBase, TemplateReviewPolicy,
        },
        stage_requirements::{
            RequiredStageSource, StageRequirementConfiguration, StageRequirementDecision,
            StageRequirements, StageRoleRevision, StageSourceEvidence, StageSourcePath,
        },
    },
};

#[tokio::test]
async fn stage_requirements_recheck_skill_on_retry_and_invoke_only_the_pinned_revision()
-> Result<()> {
    let (_directory, store, _, mut scope) = setup().await?;
    scope.mode = SessionMode::Plan;
    scope.effort = None;
    std::fs::write(&scope.executable, b"fixture")?;
    let repo = crate::repository::register(&store, scope.checkout.clone()).await?;
    let project = repo.project_id.clone();
    let relative = PathBuf::from(".agents/skills/fixture/SKILL.md");
    let skill_path = scope.checkout.join(&relative);
    std::fs::create_dir_all(skill_path.parent().unwrap())?;
    let original = b"---\nname: fixture\n---\nExact skill contents\n";
    std::fs::write(&skill_path, original)?;
    let skill = SkillEntry {
        id: SkillEntry::identity(Provider::Codex, &skill_path)?,
        provider: Provider::Codex,
        source: SkillSource::Provider,
        name: "fixture".into(),
        path: skill_path.clone(),
        scope: Some("repo".into()),
        plugin_id: None,
        enabled: Some(true),
        revision: None,
    };
    let revision = current_skill(&skill)?
        .context("fixture skill unavailable")?
        .0
        .revision;
    let required = RequiredStageSource::Skill {
        provider: Provider::Codex,
        path: StageSourcePath::Checkout { path: relative },
        namespace: None,
        revision: revision.clone(),
        invoke: true,
    };
    let template = ConversationTemplate {
        name: "Qualified planner".into(),
        role: JobRole::Pipeline,
        model: ModelSelection {
            provider: scope.provider,
            profile_id: scope.profile_id.clone(),
            model_id: "fixture-model".into(),
        },
        effort: None,
        instructions: "Inspect the saved source.".into(),
        tool_scope: SessionMode::ReadOnly,
        completion_contract: "Return the review.".into(),
        review_policy: TemplateReviewPolicy::Independent,
        requirements: StageRequirements {
            capabilities: vec![],
            sources: vec![required.clone()],
        },
    };
    let SaveConversationTemplate::Saved { detail } = store
        .save_conversation_template(project.clone(), None, template.clone())
        .await?
    else {
        anyhow::bail!("The role template was not saved");
    };
    let template_id = detail.revision.template_id;
    let role_pin = StageRoleRevision {
        template_id: template_id.clone(),
        revision: detail.revision.version.revision,
    };
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let plan = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap();
    let NodeConfiguration::AgentRole { requirements, .. } = &mut plan.configuration else {
        unreachable!()
    };
    *requirements = StageRequirementConfiguration {
        role_template: Some(role_pin.clone()),
        ..Default::default()
    };
    let saved = store
        .save_workflow_revision(project.clone(), "required-skill".into(), graph)
        .await?;
    let changed = ConversationTemplate {
        requirements: StageRequirements::default(),
        ..template
    };
    let updated = store
        .save_conversation_template(
            project.clone(),
            Some(TemplateBase {
                template_id,
                expected_revision: role_pin.revision.clone(),
            }),
            changed,
        )
        .await?;
    assert!(matches!(updated, SaveConversationTemplate::Saved { .. }));
    let brief = scope.checkout.join("brief.md");
    std::fs::write(&brief, "Plan from the frozen brief")?;
    let document = store
        .capture_repository_markdown(repo.checkout_id.clone(), brief)
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: saved,
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: document.revision.id,
            },
            request_id: "required-skill-launch".into(),
            work_id: None,
        })
        .await?;
    let context = store
        .current_workflow_host_outputs(project.clone(), run.id.clone())
        .await?["context"]
        .clone();
    let job = store
        .submit_workflow_agent(WorkflowAgentSubmission {
            project_id: project.clone(),
            dispatch: PipelineDispatch {
                pipeline_run_id: run.id,
                node_instance_id: "plan".into(),
                plan_revision: None,
                release_id: None,
            },
            scope,
            request: JobRequest {
                checkout_id: Some(repo.checkout_id),
                task_id: None,
                mode: SessionMode::Plan,
                instructions: "Read the pinned brief".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
            input: Default::default(),
            artifacts: NodeInputs::from([("context".into(), context)]),
            unit_id: None,
        })
        .await?;
    let admission = AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    };
    std::fs::write(&skill_path, b"Skill revision changed before dispatch")?;
    let first = store
        .claim_next_job(admission.clone())
        .await?
        .context("Missing admitted workflow attempt")?;
    let first_id = first.run.id.clone();
    let runner_log = |conversation: String| WireLog {
        store: store.clone(),
        runtime: tokio::runtime::Handle::current(),
        conversation,
        frames: Arc::new(Mutex::new(Vec::new())),
    };
    let first_log = runner_log(first.conversation_id.clone());
    assert_eq!(
        start_managed_turn(
            store.clone(),
            Arc::new(FixtureRunner(first_log.clone())),
            first,
            None
        )
        .wait()
        .await?,
        RunStatus::Failed
    );
    assert!(first_log.frames.lock().unwrap().iter().all(|frame| {
        serde_json::from_slice::<Value>(frame).is_ok_and(|json| json["method"] != "turn/start")
    }));
    let before_retry = store
        .workflow_stage_assessment(project.clone(), first_id.clone())
        .await?
        .unwrap();
    assert_eq!(before_retry.role_template, Some(role_pin.clone()));
    assert!(
        matches!(&before_retry.decisions[..], [StageRequirementDecision::Source { source, evidence: None, blocker: Some(reason) }]
        if source == &required && reason.contains("contents changed"))
    );
    assert!(
        store
            .workflow_stage_assessment("foreign-project".into(), first_id.clone())
            .await?
            .is_none()
    );

    std::fs::write(&skill_path, original)?;
    store
        .queue_job_recovery(
            project.clone(),
            job.id,
            first_id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    let retry = store
        .claim_next_job(admission)
        .await?
        .context("Missing admitted retry")?;
    let retry_id = retry.run.id.clone();
    let retry_conversation = retry.conversation_id.clone();
    let retry_log = runner_log(retry_conversation.clone());
    start_managed_turn(
        store.clone(),
        Arc::new(FixtureRunner(retry_log.clone())),
        retry,
        None,
    )
    .wait()
    .await?;
    let dispatch = store
        .agent_dispatch(retry_conversation, retry_id.clone())
        .await?
        .context("Retry did not submit the qualified skill")?;
    assert_eq!(dispatch.delivery, Delivery::Submitted);
    let turn: Value = serde_json::from_slice(&dispatch.frames[0])?;
    let content = turn["params"]["input"]
        .as_array()
        .context("Missing Codex turn input")?;
    assert!(content.iter().any(|entry| entry["type"] == "skill"
        && entry["name"] == "fixture"
        && entry["path"].as_str().map(PathBuf::from).as_ref() == Some(&skill_path)));
    let saved = store
        .workflow_stage_assessment(project.clone(), retry_id)
        .await?
        .unwrap();
    assert_eq!(saved.role_template, Some(role_pin));
    assert!(
        matches!(&saved.decisions[..], [StageRequirementDecision::Source {
        evidence: Some(StageSourceEvidence::Skill { skill }), blocker: None, .. }] if skill.revision == revision)
    );
    assert_eq!(
        store.workflow_stage_assessment(project, first_id).await?,
        Some(before_retry)
    );
    Ok(())
}
