use super::*;
use crate::{
    jobs::{AdmissionEnvironment, JobRequest, JobRole, PipelineDispatch},
    model_catalog::ModelSelection,
    providers::{
        capabilities::{CapabilityDescriptor, CapabilityStatus, EvidenceOrigin},
        generated_image::TINY_PNG_BASE64,
        profiles::DESKTOP_PERMISSION_REVISION,
    },
    workflows::{
        DeliveryRoles, ManualWorkflowLaunch, NodeConfiguration, NodeInputs,
        WorkflowAgentSubmission, WorkflowLaunchSource, builtin_delivery_graph,
        role_templates::{ConversationTemplate, SaveConversationTemplate, TemplateReviewPolicy},
        stage_requirements::{StageRequirementDecision, StageRequirements, StageRoleRevision},
    },
};

#[tokio::test]
async fn saved_native_image_qualifies_the_exact_profile_for_two_pinned_roles() -> Result<()> {
    let (directory, store, chat, mut scope) = setup().await?;
    scope.profile_id = "desktop-direct:codex".into();
    scope.runner = DirectWindowsRunner.identity().into();
    scope.permission_revision = DESKTOP_PERMISSION_REVISION.into();
    scope.mode = SessionMode::Plan;
    scope.effort = None;
    scope.model = Some("fixture-image-model".into());
    std::fs::write(&scope.executable, b"disposable-cli-build")?;
    let repository = crate::repository::register(&store, scope.checkout.clone()).await?;
    let project = repository.project_id;
    let checkout = repository.checkout_id;
    let baseline = prepare(
        &store,
        &chat.id,
        scope.clone(),
        "Generate the bounded fixture image",
    )
    .await?;
    let baseline_id = baseline.run.id.clone();
    let wire = runner(&store, &baseline, true);
    assert_eq!(
        start_managed_turn(store.clone(), Arc::new(wire), baseline, None)
            .wait()
            .await?,
        RunStatus::Succeeded
    );
    let source = store
        .dispatch_profile_evidence(project.clone(), chat.id.clone(), baseline_id.clone())
        .await?
        .context("Missing image-qualification source run")?;
    let image = source
        .generated_images
        .first()
        .context("No captured native image")?;
    let sequence: i64 = image.event_sequence.parse()?;
    let assessed = store
        .agent_dispatch(chat.id.clone(), baseline_id.clone())
        .await?
        .context("Missing submitted image scope")?
        .snapshot
        .capabilities
        .context("Missing saved capability scope")?
        .scope;
    assert!(
        store
            .qualify_conversation_generated_image(
                "other-project".into(),
                chat.id.clone(),
                baseline_id.clone(),
                sequence
            )
            .await
            .is_err()
    );
    assert!(
        store
            .qualify_conversation_generated_image(
                project.clone(),
                chat.id.clone(),
                baseline_id.clone(),
                sequence + 1
            )
            .await
            .is_err()
    );
    let qualification = store
        .qualify_conversation_generated_image(
            project.clone(),
            chat.id.clone(),
            baseline_id.clone(),
            sequence,
        )
        .await?;
    assert_eq!(
        qualification.origin,
        EvidenceOrigin::AuthenticatedProviderRun
    );
    assert_eq!(
        qualification.artifact_id,
        format!("generated-image:{sequence}")
    );
    assert_eq!(
        qualification,
        store
            .qualify_conversation_generated_image(project.clone(), chat.id, baseline_id, sequence)
            .await?
    );
    let descriptor = CapabilityDescriptor::native_image_generation(Provider::Codex);
    let mut mismatches = [assessed.clone(), assessed.clone(), assessed];
    mismatches[0].mode = SessionMode::ReadOnly;
    mismatches[1].checkout.id = "other-checkout".into();
    mismatches[2].configuration.effective_revision = Some("updated-settings".into());
    for mismatch in mismatches {
        let resolution = store
            .resolve_capability_assessment(mismatch, vec![descriptor.clone()])
            .await?;
        assert_eq!(resolution.entries[0].status(), CapabilityStatus::Unverified);
    }

    let document = scope.checkout.join("brief.md");
    std::fs::write(&document, "Prepare a figure for the release notes.")?;
    let source = WorkflowLaunchSource {
        checkout_id: checkout.clone(),
        revision_id: store
            .capture_repository_markdown(checkout.clone(), document)
            .await?
            .revision
            .id,
    };
    let available = planner(&store, &project, &scope, "Illustration planner").await?;
    let changed = SessionScope {
        model: Some("fixture-other-model".into()),
        ..scope.clone()
    };
    let stale = planner(&store, &project, &changed, "Second illustration role").await?;
    let accepted = run_role(&store, &project, &source, scope, available.clone()).await?;
    let blocked = run_role(&store, &project, &source, changed, stale.clone()).await?;
    assert_eq!(accepted.status, RunStatus::Succeeded);
    assert!(accepted.sent);
    assert_eq!(blocked.status, RunStatus::Failed);
    assert!(!blocked.sent);
    assert!(
        store
            .agent_dispatch(blocked.conversation, blocked.id.clone())
            .await?
            .is_none()
    );

    drop(store);
    let reopened = Store::open(directory.path().join("store.sqlite"))?;
    let saved = reopened
        .workflow_stage_assessment(project.clone(), accepted.id)
        .await?
        .unwrap();
    assert_eq!(saved.role_template, Some(available));
    assert!(
        matches!(&saved.decisions[..], [StageRequirementDecision::Capability {
        status: Some(CapabilityStatus::Verified), evidence_id: Some(id), blocker: None, ..
    }] if id == &qualification.id)
    );
    let denied = reopened
        .workflow_stage_assessment(project, blocked.id)
        .await?
        .unwrap();
    assert_eq!(denied.role_template, Some(stale));
    assert!(
        matches!(&denied.decisions[..], [StageRequirementDecision::Capability {
        status: Some(CapabilityStatus::Unverified), evidence_id: None, blocker: Some(reason), ..
    }] if reason.contains("No qualified Codex image matches the effective profile, model, mode, settings and worktree"))
    );
    Ok(())
}

async fn planner(
    store: &Store,
    project: &str,
    scope: &SessionScope,
    name: &str,
) -> Result<StageRoleRevision> {
    let SaveConversationTemplate::Saved { detail } = store
        .save_conversation_template(
            project.into(),
            None,
            ConversationTemplate {
                name: name.into(),
                role: JobRole::Pipeline,
                model: ModelSelection {
                    provider: scope.provider,
                    profile_id: scope.profile_id.clone(),
                    model_id: scope.model.clone().unwrap(),
                },
                effort: None,
                instructions: "Use the selected image capability.".into(),
                tool_scope: SessionMode::ReadOnly,
                completion_contract: "Return the illustration plan.".into(),
                review_policy: TemplateReviewPolicy::Independent,
                requirements: StageRequirements {
                    capabilities: vec![CapabilitySubject::codex_image_generation()],
                    sources: vec![],
                },
            },
        )
        .await?
    else {
        anyhow::bail!("Could not save the role")
    };
    Ok(StageRoleRevision {
        template_id: detail.revision.template_id,
        revision: detail.revision.version.revision,
    })
}

struct StageOutcome {
    id: String,
    conversation: String,
    sent: bool,
    status: RunStatus,
}

async fn run_role(
    store: &Store,
    project: &str,
    source: &WorkflowLaunchSource,
    scope: SessionScope,
    role: StageRoleRevision,
) -> Result<StageOutcome> {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let plan = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap();
    let NodeConfiguration::AgentRole {
        requirements,
        model,
        ..
    } = &mut plan.configuration
    else {
        unreachable!()
    };
    requirements.role_template = Some(role.clone());
    *model = Some(ModelSelection {
        provider: scope.provider,
        profile_id: scope.profile_id.clone(),
        model_id: scope.model.clone().unwrap(),
    });
    let version = store
        .save_workflow_revision(
            project.into(),
            format!("image-role-{}", role.template_id),
            graph,
        )
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.into(),
            workflow: version,
            source: source.clone(),
            request_id: role.template_id,
            work_id: None,
        })
        .await?;
    let context = store
        .current_workflow_host_outputs(project.into(), run.id.clone())
        .await?["context"]
        .clone();
    store
        .submit_workflow_agent(WorkflowAgentSubmission {
            project_id: project.into(),
            dispatch: PipelineDispatch {
                pipeline_run_id: run.id,
                node_instance_id: "plan".into(),
                plan_revision: None,
                release_id: None,
            },
            scope,
            request: JobRequest {
                checkout_id: Some(source.checkout_id.clone()),
                task_id: None,
                mode: SessionMode::Plan,
                instructions: "Read the frozen brief".into(),
                dependencies: vec![],
                requires_bridge: false,
            },
            input: Default::default(),
            artifacts: NodeInputs::from([("context".into(), context)]),
            unit_id: None,
        })
        .await?;
    let job = store
        .claim_next_job(AdmissionEnvironment {
            runner: DirectWindowsRunner.identity().into(),
            bridge_available: false,
        })
        .await?
        .context("The role did not reach scheduler admission")?;
    let id = job.run.id.clone();
    let conversation = job.conversation_id.clone();
    let protocol = runner(store, &job, false);
    let log = protocol.log.clone();
    let status = start_managed_turn(store.clone(), Arc::new(protocol), job, None)
        .wait()
        .await?;
    let sent = log.frames.lock().unwrap().iter().any(|frame| {
        serde_json::from_slice::<Value>(frame).is_ok_and(|value| value["method"] == "turn/start")
    });
    Ok(StageOutcome {
        id,
        conversation,
        sent,
        status,
    })
}

fn runner(store: &Store, plan: &AttemptPlan, image: bool) -> ImageProtocol {
    ImageProtocol {
        log: WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: plan.conversation_id.clone(),
            frames: Arc::new(Mutex::new(Vec::new())),
        },
        model: plan.scope.model.clone().unwrap(),
        image,
    }
}

struct ImageProtocol {
    log: WireLog,
    model: String,
    image: bool,
}

impl Runner for ImageProtocol {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }
    fn execution_platform(&self) -> Option<ExecutionPlatform> {
        DirectWindowsRunner.execution_platform()
    }
    fn executable_revision(&self, path: &Path) -> Option<String> {
        DirectWindowsRunner.executable_revision(path)
    }
    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        let process = FixtureRunner(self.log.clone()).launch(request)?;
        let (reply, output) = mpsc::channel();
        let send = |value: Value| {
            reply
                .send(Ok(OutputFrame {
                    stream: Stream::Stdout,
                    data: value.to_string(),
                }))
                .map_err(io::Error::other)
        };
        for frame in process.output {
            let mut value: Value = serde_json::from_str(&frame?.data).map_err(io::Error::other)?;
            match value["method"].as_str() {
                Some("thread/settings/updated") => continue,
                Some("turn/completed") if self.image => {
                    send(json!({"method":"item/completed","params":{
                        "threadId":"native-session","turnId":"native-turn","item":{
                            "type":"imageGeneration","id":"fixture-image","status":"completed","result":TINY_PNG_BASE64
                        }
                    }}))?
                }
                _ => {}
            }
            if value["id"] == 2 {
                value["result"]["model"] = self.model.clone().into();
            }
            if value["id"] == 5 {
                value["result"]["data"][0]["skills"] = json!([]);
            }
            send(value)?;
        }
        Ok(CapturedProcess { output, ..process })
    }
}
