use std::{path::Path, process::Command};

use crate::{
    conversations::SessionScope,
    dispatch::EventSource,
    jobs::{AdmissionEnvironment, JobRequest, PipelineDispatch, PipelineNodeKind},
    providers::{
        Provider, ProviderEvent, ProviderEventData, ProviderLocation, SessionMode,
        planning::PlanSource,
    },
    repository::{self, ProjectCheckout},
    runs::RunStatus,
    store::Store,
};

use super::*;

fn wire(from: &str, output: &str, to: &str, input: &str) -> Wire {
    Wire {
        from: Endpoint {
            node: from.into(),
            port: output.into(),
        },
        to: Endpoint {
            node: to.into(),
            port: input.into(),
        },
    }
}

fn graph(role: &str) -> WorkflowGraph {
    use BuiltinNode::*;
    WorkflowGraph {
        name: "Review repository change".into(),
        nodes: [
            ("start", Start, NodeConfiguration::None),
            ("source", LoadContext, NodeConfiguration::None),
            (
                "plan",
                Plan,
                NodeConfiguration::AgentRole {
                    role: role.into(),
                    model: None,
                    requirements: Default::default(),
                },
            ),
            (
                "review",
                ReviewPlan,
                NodeConfiguration::AgentRole {
                    role: "reviewer".into(),
                    model: None,
                    requirements: Default::default(),
                },
            ),
            ("work", LinkWork, NodeConfiguration::None),
        ]
        .into_iter()
        .map(|(id, kind, configuration)| NodeInstance {
            id: id.into(),
            kind,
            configuration,
        })
        .collect(),
        wires: vec![
            wire("start", "start", "source", "ready"),
            wire("start", "document", "source", "document"),
            wire("source", "loaded", "plan", "ready"),
            wire("source", "context", "plan", "context"),
            wire("plan", "planned", "review", "ready"),
            wire("plan", "plan", "review", "plan"),
            wire("review", "reviewed", "work", "ready"),
            wire("review", "review", "work", "review"),
            wire("plan", "plan", "work", "plan"),
        ],
    }
}

#[test]
fn registry_enforces_port_types_configuration_and_control_dependencies() -> Result<()> {
    let definitions = builtin_registry();
    let implement = definitions
        .iter()
        .find(|node| node.kind == BuiltinNode::Implement)
        .unwrap();
    assert_eq!(implement.executor, Executor::WriteAgent);
    assert_eq!(implement.effect, Effect::WriteRepository);
    assert!(
        implement
            .inputs
            .iter()
            .any(|port| port.name == "release"
                && port.value == PortType::Data(ArtifactKind::Release))
    );
    assert!(compile("work".into(), graph("planner")).is_ok());
    let mut wrong_type = graph("planner");
    wrong_type.wires[1].from.port = "start".into();
    assert!(compile("work".into(), wrong_type).is_err());
    let mut cycle = graph("planner");
    cycle.wires[2] = wire("work", "linked", "plan", "ready");
    assert!(compile("work".into(), cycle).is_err());
    assert!(compile("work".into(), graph(" ")).is_err());
    assert!(
        compile(
            "builtin".into(),
            builtin_delivery_graph(DeliveryRoles::default())
        )
        .is_ok()
    );
    Ok(())
}

#[test]
fn planner_units_require_a_closed_block_and_keep_purpose_order_and_revision() -> Result<()> {
    let body = r#"{"units":[{"purpose":"First","completionCriteria":["Build passes"]},{"purpose":"Second","completionCriteria":["Tests pass"]}]}"#;
    let unclosed = format!("Plan\n```omniagent-plan\n{body}");
    assert!(super::units::parse_plan_units("revision-one", &unclosed).is_err());
    let closed = format!("{unclosed}\n```");
    let units = super::units::parse_plan_units("revision-one", &closed)?;
    assert_eq!(units[0].purpose, "First");
    assert_eq!(units[0].completion_criteria, ["Build passes"]);
    assert_eq!(units[1].ordinal, 2);
    assert_eq!(units[1].purpose, "Second");
    assert_ne!(
        units[0].id,
        super::units::parse_plan_units("revision-two", &closed)?[0].id
    );
    Ok(())
}

#[tokio::test]
async fn builtin_path_approves_an_exact_plan_and_prepares_scoped_units() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("delivery.sqlite"))?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    assert_eq!(fixture.units[0].unit.purpose, "First change");
    assert_eq!(fixture.units[1].unit.ordinal, 2);
    let ArtifactValue::Release(release) = fixture.release.value else {
        anyhow::bail!("missing prepared release");
    };
    assert_eq!(release.release.plan_revision, fixture.plan.id);
    assert_eq!(release.release.checkout_id, fixture.checkout.checkout_id);
    assert_eq!(release.workflow_run_id, fixture.run.id);
    store
        .set_job_pipeline_plan(
            fixture.project.clone(),
            fixture.run.id.clone(),
            Some(fixture.plan.id),
            fixture.alternate_plan.id,
        )
        .await?;
    let ArtifactValue::Task(task) = &fixture.task.value else {
        anyhow::bail!("expected task");
    };
    assert!(
        store
            .workflow_delivery_units(
                fixture.project.clone(),
                task.task_id.clone(),
                fixture.run.id.clone()
            )
            .await?
            .is_empty()
    );
    assert!(
        store
            .workflow_release_artifact(
                fixture.project.clone(),
                fixture.run.id.clone(),
                fixture.decision.id.clone()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .execute_workflow_host(WorkflowHostSubmission {
                project_id: fixture.project,
                run_id: fixture.run.id,
                node_id: "prepare".into(),
                inputs: NodeInputs::from([
                    ("task".into(), fixture.task.id),
                    ("decision".into(), fixture.decision.id)
                ]),
                request: WorkflowHostRequest::PrepareWorktree {
                    default_root: temp.path().join("managed")
                },
            })
            .await
            .is_err()
    );
    Ok(())
}

async fn checkout(store: &Store, path: &Path) -> Result<ProjectCheckout> {
    std::fs::create_dir(path)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(path)
            .status()?
            .success()
    );
    std::fs::write(path.join("codex.exe"), b"workflow fixture executable")?;
    repository::register(store, dunce::canonicalize(path)?).await
}

pub(super) fn submission(
    repo: &ProjectCheckout,
    run: &WorkflowRun,
    node: &str,
    plan: Option<String>,
    mode: SessionMode,
) -> WorkflowAgentSubmission {
    let checkout = std::path::PathBuf::from(&repo.checkout_root);
    WorkflowAgentSubmission {
        project_id: repo.project_id.clone(),
        dispatch: PipelineDispatch {
            pipeline_run_id: run.id.clone(),
            node_instance_id: node.into(),
            plan_revision: plan,
            release_id: None,
        },
        scope: SessionScope {
            provider: Provider::Codex,
            profile_id: "fixture".into(),
            account_binding: Some("fixture".into()),
            executable: checkout.join("codex.exe"),
            checkout,
            cli_version: "0.169.0".into(),
            runner: "fixture-runner".into(),
            permission_revision: "fixture-policy".into(),
            mode,
            model: None,
            effort: None,
        },
        request: JobRequest {
            checkout_id: Some(repo.checkout_id.clone()),
            task_id: None,
            mode,
            instructions: "Review this workflow contract".into(),
            dependencies: Vec::new(),
            requires_bridge: false,
        },
        input: Default::default(),
        artifacts: NodeInputs::new(),
        unit_id: None,
    }
}

#[tokio::test]
async fn scheduler_attempts_keep_workflow_node_and_plan_pins_and_validate_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("workflows.sqlite");
    let store = Store::open(database.clone())?;
    let repo = checkout(&store, &temp.path().join("primary")).await?;
    let foreign = checkout(&store, &temp.path().join("foreign")).await?;
    let project = repo.project_id.clone();
    let saved = store
        .save_workflow_revision(project.clone(), "delivery".into(), graph("planner"))
        .await?;
    let workflow = store
        .start_workflow_run(project.clone(), saved.clone())
        .await?;
    let node = store
        .pinned_workflow_node(project.clone(), workflow.id.clone(), "plan".into())
        .await?
        .context("planner node unavailable")?;
    let file = std::path::PathBuf::from(&repo.checkout_root).join("plan.md");
    std::fs::write(&file, "Frozen requirements")?;
    let document = store
        .capture_repository_markdown(repo.checkout_id.clone(), file.clone())
        .await?;
    std::fs::write(&file, "Requirements edited after capture")?;
    let host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: workflow.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    assert!(
        store
            .execute_workflow_host(host(
                "source",
                NodeInputs::new(),
                WorkflowHostRequest::LoadContext
            ))
            .await
            .is_err()
    );
    let start = store
        .execute_workflow_host(host(
            "start",
            NodeInputs::new(),
            WorkflowHostRequest::Start {
                document: document.revision.clone(),
            },
        ))
        .await?;
    assert_eq!(
        store
            .execute_workflow_host(host(
                "start",
                NodeInputs::new(),
                WorkflowHostRequest::Start {
                    document: document.revision.clone()
                }
            ))
            .await?,
        start
    );
    let changed = store
        .capture_repository_markdown(repo.checkout_id.clone(), file)
        .await?;
    assert!(
        store
            .execute_workflow_host(host(
                "start",
                NodeInputs::new(),
                WorkflowHostRequest::Start {
                    document: changed.revision
                }
            ))
            .await
            .is_err()
    );
    let context = store
        .execute_workflow_host(host(
            "source",
            NodeInputs::from([("document".into(), start.id.clone())]),
            WorkflowHostRequest::LoadContext,
        ))
        .await?;
    let planner = |mode, artifact: &WorkflowArtifact| {
        let mut submission = submission(&repo, &workflow, "plan", None, mode);
        submission.artifacts = NodeInputs::from([("context".into(), artifact.id.clone())]);
        submission
    };
    assert!(
        store
            .submit_workflow_agent(planner(SessionMode::Plan, &start))
            .await
            .is_err()
    );
    let bypass = store
        .register_job_pipeline_node(
            project.clone(),
            workflow.id.clone(),
            "bypass".into(),
            PipelineNodeKind::Execution,
        )
        .await;
    bypass?;
    let request = submission(&repo, &workflow, "bypass", None, SessionMode::ReadOnly);
    assert!(
        store
            .submit_pipeline_agent_job(request.dispatch, request.scope, request.request)
            .await
            .is_err()
    );
    let legacy = submission(&repo, &workflow, "plan", None, SessionMode::Plan);
    assert!(
        store
            .submit_pipeline_agent_job(legacy.dispatch, legacy.scope, legacy.request)
            .await
            .is_err()
    );
    assert!(
        store
            .set_job_pipeline_plan(
                project.clone(),
                workflow.id.clone(),
                None,
                "forged-plan".into()
            )
            .await
            .is_err()
    );
    let spoofed = planner(SessionMode::ReadOnly, &context);
    assert!(store.submit_workflow_agent(spoofed).await.is_err());
    let job = store
        .submit_workflow_agent(planner(SessionMode::Plan, &context))
        .await?;
    let environment = AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    };
    let planned = store
        .claim_next_job(environment.clone())
        .await?
        .context("planner did not reach the scheduler")?;
    assert_eq!(
        store
            .dispatch_repository_markdown(planned.run.id.clone())
            .await?,
        Some(document)
    );
    let first = store
        .workflow_attempt(project.clone(), planned.run.id.clone())
        .await?
        .unwrap();
    assert_eq!(first.job_id, job.id);
    assert_eq!(first.pin.workflow, saved);
    assert_eq!(first.pin.instance_revision, node.revision);
    assert!(first.pin.plan_revision.is_none());
    store.mark_agent_running(planned.run.id.clone()).await?;
    store
        .record_agent_observation(
            planned.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::PlanProposed {
                    text: "Make one small change".into(),
                    source: PlanSource::CodexPlanItem,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            planned.run.id.clone(),
            RunStatus::Succeeded,
            Some("Plan finished".into()),
        )
        .await?;
    let event_sequence = store
        .agent_observations(planned.conversation_id, planned.run.id.clone(), 0)
        .await?
        .into_iter()
        .find(|event| matches!(event.event.data, ProviderEventData::PlanProposed { .. }))
        .context("saved plan event unavailable")?
        .seq;
    let value = ArtifactValue::Plan {
        run_id: planned.run.id,
        event_sequence,
    };
    let plan = store
        .publish_workflow_artifact(
            project.clone(),
            workflow.id.clone(),
            "plan".into(),
            "plan".into(),
            value.clone(),
        )
        .await?;
    assert!(
        store
            .publish_workflow_artifact(
                project.clone(),
                workflow.id.clone(),
                "review".into(),
                "review".into(),
                value
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .resolve_workflow_artifact(
                project.clone(),
                workflow.id.clone(),
                plan.id.clone(),
                ArtifactKind::Plan
            )
            .await?,
        Some(plan.clone())
    );
    assert!(
        store
            .resolve_workflow_artifact(
                project.clone(),
                workflow.id.clone(),
                plan.id.clone(),
                ArtifactKind::Candidate
            )
            .await?
            .is_none()
    );
    assert!(
        store
            .resolve_workflow_artifact(
                foreign.project_id.clone(),
                workflow.id.clone(),
                plan.id.clone(),
                ArtifactKind::Plan
            )
            .await?
            .is_none()
    );
    store
        .set_job_pipeline_plan(project.clone(), workflow.id.clone(), None, plan.id.clone())
        .await?;
    let mut forged = submission(
        &repo,
        &workflow,
        "review",
        Some("forged-plan".into()),
        SessionMode::ReadOnly,
    );
    forged.artifacts = NodeInputs::from([("plan".into(), plan.id.clone())]);
    assert!(store.submit_workflow_agent(forged).await.is_err());
    let mut review_submission = submission(
        &repo,
        &workflow,
        "review",
        Some(plan.id.clone()),
        SessionMode::ReadOnly,
    );
    review_submission.scope.profile_id = "fixture-independent-review".into();
    review_submission.artifacts = NodeInputs::from([("plan".into(), plan.id.clone())]);
    let review = store.submit_workflow_agent(review_submission).await?;
    let new = store
        .save_workflow_revision(project.clone(), "delivery".into(), graph("revised-planner"))
        .await?;
    assert_ne!(new, saved);

    let task = store
        .create_project_task(project.clone(), "Deliver change".into())
        .await?;
    let counterfeit = store
        .create_project_task(foreign.project_id.clone(), "Foreign task".into())
        .await?;
    assert!(
        store
            .publish_workflow_artifact(
                project.clone(),
                workflow.id.clone(),
                "work".into(),
                "task".into(),
                ArtifactValue::Task(TaskReference {
                    project_id: project.clone(),
                    task_id: task.id.clone()
                })
            )
            .await
            .is_err()
    );
    drop(store);

    let store = Store::open(database)?;
    let claimed = store
        .claim_next_job(environment)
        .await?
        .context("review did not reach the scheduler")?;
    store.mark_agent_running(claimed.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            claimed.run.id.clone(),
            RunStatus::Succeeded,
            Some("Reviewed plan".into()),
        )
        .await?;
    let review_evidence = store
        .publish_workflow_artifact(
            project.clone(),
            workflow.id.clone(),
            "review".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: claimed.run.id.clone(),
            },
        )
        .await?;
    let work_inputs = NodeInputs::from([
        ("plan".into(), plan.id.clone()),
        ("review".into(), review_evidence.id),
    ]);
    assert!(
        store
            .execute_workflow_host(host(
                "work",
                work_inputs.clone(),
                WorkflowHostRequest::LinkExistingTask {
                    task_id: counterfeit.id
                }
            ))
            .await
            .is_err()
    );
    let work = store
        .execute_workflow_host(host(
            "work",
            work_inputs.clone(),
            WorkflowHostRequest::LinkExistingTask { task_id: task.id },
        ))
        .await?;
    let attempt = store
        .workflow_attempt(project.clone(), claimed.run.id)
        .await?
        .unwrap();
    assert_eq!(attempt.job_id, review.id);
    assert_eq!(attempt.pin.workflow, saved);
    assert_eq!(attempt.pin.plan_revision.as_ref(), Some(&plan.id));
    assert_eq!(attempt.pin.node_instance_id, "review");
    let recorded = store
        .inspect_workflow_execution(project.clone(), workflow.id.clone())
        .await?
        .context("missing run graph")?;
    assert_eq!(recorded.definition.version, saved);
    assert_ne!(recorded.definition.version, new);
    assert_eq!(
        recorded
            .definition
            .nodes
            .iter()
            .find(|entry| entry.instance.id == "plan"),
        Some(&node)
    );
    let start_output = recorded
        .outputs
        .iter()
        .find(|entry| entry.artifact.id == start.id)
        .context("start output missing")?;
    assert!(start_output.input_receipts[0].inputs.is_empty());
    let work_output = recorded
        .outputs
        .iter()
        .find(|entry| entry.artifact.id == work.id)
        .context("linked work missing")?;
    assert_eq!(work_output.input_receipts[0].inputs, work_inputs);
    assert_eq!(work_output.input_receipts[0].execution_key, plan.id);
    let review_job = recorded
        .jobs
        .iter()
        .find(|entry| entry.job.id == review.id)
        .context("review job missing")?;
    assert_eq!(review_job.inputs.as_ref().unwrap()["plan"], plan.id);
    assert_eq!(review_job.attempts[0].run_id, attempt.run_id);
    assert_eq!(review_job.attempts[0].status, RunStatus::Succeeded);
    assert!(
        store
            .inspect_workflow_execution(foreign.project_id, workflow.id.clone())
            .await?
            .is_none()
    );
    assert_eq!(
        store
            .resolve_workflow_artifact(project, workflow.id, work.id.clone(), ArtifactKind::Task)
            .await?,
        Some(work)
    );
    Ok(())
}

pub(super) struct ApprovedDeliveryFixture {
    pub project: String,
    pub run: WorkflowRun,
    pub task: WorkflowArtifact,
    pub plan: WorkflowArtifact,
    pub alternate_plan: WorkflowArtifact,
    pub decision: WorkflowArtifact,
    pub worktree: WorkflowArtifact,
    pub release: WorkflowArtifact,
    pub checkout: ProjectCheckout,
    pub units: Vec<WorkflowDeliveryUnit>,
}

pub(super) async fn approved_delivery_fixture(
    store: &Store,
    temp_path: &Path,
) -> Result<ApprovedDeliveryFixture> {
    let repo = approved_delivery_repository(store, temp_path).await?;
    approve_delivery_fixture_for_work(store, temp_path, repo, None).await
}

pub(super) async fn approved_delivery_repository(
    store: &Store,
    temp_path: &Path,
) -> Result<ProjectCheckout> {
    let repo = checkout(store, &temp_path.join("builtin-source")).await?;
    let root = Path::new(&repo.checkout_root);
    std::fs::write(root.join("brief.md"), "Two independent changes")?;
    std::fs::write(root.join("first.txt"), "original first\n")?;
    std::fs::write(root.join("second.txt"), "original second\n")?;
    for args in [
        vec!["add", "brief.md", "first.txt", "second.txt"],
        vec![
            "-c",
            "user.name=Workflow Test",
            "-c",
            "user.email=workflow@example.test",
            "commit",
            "-qm",
            "Base",
        ],
    ] {
        ensure!(
            Command::new("git.exe")
                .args(args)
                .current_dir(root)
                .status()?
                .success()
        );
    }
    Ok(repo)
}

pub(super) async fn approve_delivery_fixture_for_work(
    store: &Store,
    temp_path: &Path,
    repo: ProjectCheckout,
    task_id: Option<String>,
) -> Result<ApprovedDeliveryFixture> {
    let run = store
        .start_builtin_delivery(repo.project_id.clone(), DeliveryRoles::default())
        .await?;
    approve_delivery_fixture_in_run(store, temp_path, repo, task_id, run).await
}

pub(super) async fn link_reviewed_delivery_fixture(
    store: &Store,
    repo: &ProjectCheckout,
    task_id: Option<String>,
    run: &WorkflowRun,
) -> Result<(WorkflowArtifact, WorkflowArtifact, WorkflowArtifact)> {
    let planned_text = if task_id.is_some() {
        "Deliver the change for this Sub-Task.\n\n```omniagent-plan\n{\"units\":[{\"purpose\":\"Deliver the scoped change\",\"completionCriteria\":[\"Scoped file updated\"]}]}\n```"
    } else {
        "Do each change separately.\n\n```omniagent-plan\n{\"units\":[{\"purpose\":\"First change\",\"completionCriteria\":[\"First file updated\"]},{\"purpose\":\"Second change\",\"completionCriteria\":[\"Second file updated\"]}]}\n```"
    };
    link_reviewed_plan_fixture(
        store,
        repo,
        task_id,
        run,
        planned_text,
        "Two sequential changes",
    )
    .await
}

pub(super) async fn link_reviewed_plan_fixture(
    store: &Store,
    repo: &ProjectCheckout,
    task_id: Option<String>,
    run: &WorkflowRun,
    planned_text: &str,
    task_title: &str,
) -> Result<(WorkflowArtifact, WorkflowArtifact, WorkflowArtifact)> {
    let alternative = "Alternative: deliver only the first change.\n\n```omniagent-plan\n{\"units\":[{\"purpose\":\"First change\",\"completionCriteria\":[\"First file updated\"]}]}\n```";
    let (plans, _) =
        workflow_plan_candidates_fixture(store, repo, run, [planned_text, alternative]).await?;
    link_reviewed_plan_candidates_fixture(store, repo, task_id, run, plans, task_title).await
}

pub(super) async fn workflow_plan_candidates_fixture(
    store: &Store,
    repo: &ProjectCheckout,
    run: &WorkflowRun,
    [planned_text, alternative]: [&str; 2],
) -> Result<(
    [WorkflowArtifact; 2],
    Vec<crate::conversations::MessageRevision>,
)> {
    let project = repo.project_id.clone();
    let root = Path::new(&repo.checkout_root);
    let host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let document = match store
        .workflow_launch(project.clone(), run.id.clone())
        .await?
    {
        Some(launch) => launch.source,
        None => {
            store
                .capture_repository_markdown(repo.checkout_id.clone(), root.join("brief.md"))
                .await?
                .revision
        }
    };
    let start = store
        .execute_workflow_host(host(
            "start",
            NodeInputs::new(),
            WorkflowHostRequest::Start { document },
        ))
        .await?;
    let context = store
        .execute_workflow_host(host(
            "context",
            NodeInputs::from([("document".into(), start.id)]),
            WorkflowHostRequest::LoadContext,
        ))
        .await?;
    let mut planner = submission(repo, run, "plan", None, SessionMode::Plan);
    planner.artifacts = NodeInputs::from([("context".into(), context.id)]);
    store.submit_workflow_agent(planner).await?;
    let env = || AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    };
    let planned = store
        .claim_next_job(env())
        .await?
        .context("planner was not admitted")?;
    store.mark_agent_running(planned.run.id.clone()).await?;
    store
        .record_agent_observation(
            planned.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::PlanProposed {
                    text: planned_text.into(),
                    source: PlanSource::CodexPlanItem,
                },
            },
        )
        .await?;
    store
        .record_agent_observation(
            planned.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::PlanProposed {
                    text: alternative.into(),
                    source: PlanSource::CodexPlanItem,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            planned.run.id.clone(),
            RunStatus::Succeeded,
            Some("Plan ready".into()),
        )
        .await?;
    let mut proposals = store
        .agent_observations(planned.conversation_id, planned.run.id.clone(), 0)
        .await?
        .into_iter()
        .filter(|event| matches!(event.event.data, ProviderEventData::PlanProposed { .. }));
    let original = proposals.next().context("saved plan proposal missing")?.seq;
    let alternative = proposals.next().context("alternate proposal missing")?.seq;
    let plan = store
        .publish_workflow_artifact(
            project.clone(),
            run.id.clone(),
            "plan".into(),
            "plan".into(),
            ArtifactValue::Plan {
                run_id: planned.run.id.clone(),
                event_sequence: original,
            },
        )
        .await?;
    let alternate_plan = store
        .publish_workflow_artifact(
            project.clone(),
            run.id.clone(),
            "plan".into(),
            "plan".into(),
            ArtifactValue::Plan {
                run_id: planned.run.id,
                event_sequence: alternative,
            },
        )
        .await?;
    Ok(([plan, alternate_plan], planned.input))
}

pub(super) async fn link_reviewed_plan_candidates_fixture(
    store: &Store,
    repo: &ProjectCheckout,
    task_id: Option<String>,
    run: &WorkflowRun,
    [plan, alternate_plan]: [WorkflowArtifact; 2],
    task_title: &str,
) -> Result<(WorkflowArtifact, WorkflowArtifact, WorkflowArtifact)> {
    let project = repo.project_id.clone();
    store
        .set_job_pipeline_plan(project.clone(), run.id.clone(), None, plan.id.clone())
        .await?;
    let mut reviewer = submission(
        repo,
        run,
        "review-plan",
        Some(plan.id.clone()),
        SessionMode::ReadOnly,
    );
    reviewer.artifacts = NodeInputs::from([("plan".into(), plan.id.clone())]);
    reviewer.scope.provider = Provider::Claude;
    reviewer.scope.profile_id = "fixture-independent-review".into();
    store.submit_workflow_agent(reviewer).await?;
    let reviewed = store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-runner".into(),
            bridge_available: false,
        })
        .await?
        .context("plan review was not admitted")?;
    store.mark_agent_running(reviewed.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            reviewed.run.id.clone(),
            RunStatus::Succeeded,
            Some("Plan reviewed".into()),
        )
        .await?;
    let review = store
        .publish_workflow_artifact(
            project.clone(),
            run.id.clone(),
            "review-plan".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: reviewed.run.id,
            },
        )
        .await?;
    let links = NodeInputs::from([
        ("plan".into(), plan.id.clone()),
        ("review".into(), review.id),
    ]);
    let work = match task_id {
        Some(task_id) => WorkflowHostRequest::LinkExistingTask { task_id },
        None => WorkflowHostRequest::CreateTask {
            title: task_title.into(),
        },
    };
    let host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let task = store
        .execute_workflow_host(host("work", links.clone(), work.clone()))
        .await?;
    assert_eq!(
        store
            .execute_workflow_host(host("work", links, work))
            .await?,
        task
    );
    Ok((task, plan, alternate_plan))
}

pub(super) async fn approve_delivery_fixture_in_run(
    store: &Store,
    temp_path: &Path,
    repo: ProjectCheckout,
    task_id: Option<String>,
    run: WorkflowRun,
) -> Result<ApprovedDeliveryFixture> {
    let unit_count = if task_id.is_some() { 1 } else { 2 };
    let linked = link_reviewed_delivery_fixture(store, &repo, task_id, &run).await?;
    let fixture = approve_reviewed_plan_fixture(store, temp_path, repo, run, linked).await?;
    assert_eq!(fixture.units.len(), unit_count);
    Ok(fixture)
}

pub(super) async fn approve_reviewed_plan_fixture(
    store: &Store,
    temp_path: &Path,
    repo: ProjectCheckout,
    run: WorkflowRun,
    linked: (WorkflowArtifact, WorkflowArtifact, WorkflowArtifact),
) -> Result<ApprovedDeliveryFixture> {
    let (task, plan, alternate_plan) = linked;
    let project = repo.project_id.clone();
    let host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let decision = store
        .execute_workflow_host(host(
            "approve",
            NodeInputs::from([
                ("plan".into(), plan.id.clone()),
                ("task".into(), task.id.clone()),
            ]),
            WorkflowHostRequest::ApproveImplementation,
        ))
        .await?;
    let ArtifactValue::Task(task_ref) = &task.value else {
        anyhow::bail!("expected a task");
    };
    let ArtifactValue::Approval(approved) = &decision.value else {
        anyhow::bail!("expected an approval decision");
    };
    assert_eq!(approved.plan_revision, plan.id);
    assert_eq!(approved.source_checkout_id, repo.checkout_id);
    assert_eq!(approved.source, ApprovalDecisionSource::Operator);
    assert!(
        store
            .workflow_release_artifact(project.clone(), run.id.clone(), decision.id.clone())
            .await?
            .is_none()
    );
    let prepare = WorkflowHostRequest::PrepareWorktree {
        default_root: temp_path.join("managed"),
    };
    let inputs = NodeInputs::from([
        ("task".into(), task.id.clone()),
        ("decision".into(), decision.id.clone()),
    ]);
    let worktree = store
        .execute_workflow_host(host("prepare", inputs.clone(), prepare.clone()))
        .await?;
    assert_eq!(
        store
            .execute_workflow_host(host("prepare", inputs, prepare))
            .await?,
        worktree
    );
    let release = store
        .workflow_release_artifact(project.clone(), run.id.clone(), decision.id.clone())
        .await?
        .context("prepared release missing")?;
    let ArtifactValue::Worktree { checkout_id, .. } = &worktree.value else {
        anyhow::bail!("expected a worktree");
    };
    let checkout = ProjectCheckout {
        checkout_id: checkout_id.clone(),
        project_id: project.clone(),
        repository_root: repo.repository_root,
        checkout_root: repository::checkout_root(store, checkout_id.clone())
            .await?
            .to_string_lossy()
            .into_owned(),
    };
    let units = store
        .workflow_delivery_units(project.clone(), task_ref.task_id.clone(), run.id.clone())
        .await?;
    assert!(
        units
            .iter()
            .all(|unit| unit.checkout_id == *checkout_id && unit.commit_evidence_id.is_none())
    );
    Ok(ApprovedDeliveryFixture {
        project,
        run,
        task,
        plan,
        alternate_plan,
        decision,
        worktree,
        release,
        checkout,
        units,
    })
}
