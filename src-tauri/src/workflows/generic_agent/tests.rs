use std::path::Path;

use serde_json::json;

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::{
        AdmissionEnvironment, GoalCriterionEvidence, GoalTurnAction, GoalTurnReport,
        PipelineAgentInput, RecoveryAction,
    },
    model_catalog::ModelSelection,
    providers::{
        Provider, ProviderEvent, ProviderEventData, ProviderLocation, TurnOutcome,
        questions::QuestionRoute,
    },
    repository::{
        self, AttemptWorktreeEvidence, ProjectCheckout,
        verification::{CandidateSelection, CandidateTarget},
    },
    workflows::{
        DeliveryRoles, Endpoint, NodeInstance, Wire, WorkflowAgentSubmission, WorkflowGraph,
        builtin_delivery_graph,
        stage_goal::{StageGoalConfiguration, StageGoalCriterion, StageGoalEvidence},
        stage_requirements::StageRequirementConfiguration,
        tests::{approve_delivery_fixture_in_run, approved_delivery_repository},
    },
};

use super::*;

fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

fn wire(from: &str, out: &str, to: &str, input: &str) -> Wire {
    Wire {
        from: Endpoint {
            node: from.into(),
            port: out.into(),
        },
        to: Endpoint {
            node: to.into(),
            port: input.into(),
        },
    }
}

fn standalone_graph(
    prompt: &str,
    requirements: StageRequirementConfiguration,
) -> Result<WorkflowGraph> {
    Ok(WorkflowGraph {
        name: "Agent workflow".into(),
        nodes: vec![
            node(
                "start",
                BuiltinNode::TypedStart,
                serde_json::from_value(json!({"kind":"typed_start","fields":[]}))?,
            ),
            node(
                "prompt",
                BuiltinNode::BuildText,
                serde_json::from_value(json!({"kind":"build_text","template":prompt}))?,
            ),
            node(
                "agent",
                BuiltinNode::Agent,
                NodeConfiguration::AgentRole {
                    role: "reader".into(),
                    model: Some(ModelSelection {
                        provider: Provider::Codex,
                        profile_id: "fixture-profile".into(),
                        model_id: "fixture-model".into(),
                    }),
                    requirements,
                },
            ),
        ],
        wires: vec![
            wire("start", "start", "prompt", "ready"),
            wire("prompt", "completed", "agent", "ready"),
            wire("prompt", "text", "agent", "prompt"),
        ],
    })
}

async fn launch_standalone(
    store: &Store,
    repo: &ProjectCheckout,
    id: &str,
    graph: WorkflowGraph,
) -> Result<(WorkflowRun, WorkflowArtifact)> {
    let project = &repo.project_id;
    let version = store
        .save_workflow_revision(project.clone(), id.into(), graph)
        .await?;
    let run = store
        .launch_workflow_request(serde_json::from_value(json!({
            "projectId": project, "workflow": version, "checkoutId": repo.checkout_id,
            "requestId": id, "inputs": {},
        }))?)
        .await?;
    let inspected = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("The workflow was not launched")?;
    let prompt = inspected
        .outputs
        .into_iter()
        .find(|output| {
            output.artifact.node_instance_id == "prompt" && output.artifact.port == "text"
        })
        .context("The rendered prompt was not issued")?
        .artifact;
    Ok((run, prompt))
}

fn submit_standalone(
    repo: &ProjectCheckout,
    run: &WorkflowRun,
    prompt: &WorkflowArtifact,
    mode: SessionMode,
) -> WorkflowAgentSubmission {
    WorkflowAgentSubmission {
        project_id: repo.project_id.clone(),
        dispatch: PipelineDispatch {
            pipeline_run_id: run.id.clone(),
            node_instance_id: "agent".into(),
            plan_revision: None,
            release_id: None,
        },
        scope: SessionScope {
            provider: Provider::Codex,
            profile_id: "fixture-profile".into(),
            account_binding: None,
            executable: std::env::current_exe().unwrap(),
            checkout: repo.checkout_root.clone().into(),
            cli_version: "0.169.0".into(),
            runner: "fixture-agent-runner".into(),
            permission_revision: "fixture".into(),
            mode,
            model: Some("fixture-model".into()),
            effort: None,
        },
        request: JobRequest {
            checkout_id: Some(repo.checkout_id.clone()),
            task_id: None,
            mode,
            instructions: "The renderer cannot replace the wired prompt.".into(),
            dependencies: Vec::new(),
            requires_bridge: false,
        },
        input: PipelineAgentInput::default(),
        artifacts: NodeInputs::from([("prompt".into(), prompt.id.clone())]),
        unit_id: None,
    }
}

#[tokio::test]
async fn automatic_agent_dispatch_survives_pause_restart_and_duplicate_submission() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("automatic-agent.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id.clone();
    let graph = standalone_graph("Inspect the pinned inputs.", Default::default())?;
    launch_standalone(&store, &repo, "manual-dispatch", graph.clone()).await?;
    assert!(store.next_workflow_agent().await?.is_none());

    let version = store
        .save_workflow_revision(project.clone(), "automatic-dispatch".into(), graph)
        .await?;
    let run = store
        .launch_workflow_request(serde_json::from_value(json!({
            "projectId": project, "workflow": version, "checkoutId": repo.checkout_id,
            "requestId": "automatic-dispatch", "inputs": {}, "automatic": true,
        }))?)
        .await?;
    let stage = store
        .next_workflow_agent()
        .await?
        .context("Agent was not discovered")?;
    let snapshot = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("The workflow was not launched")?;
    assert_eq!(snapshot.automatic_agent_waits.len(), 1);
    let prompt = snapshot
        .outputs
        .into_iter()
        .find(|output| {
            output.artifact.node_instance_id == "prompt" && output.artifact.port == "text"
        })
        .context("Missing pinned prompt")?
        .artifact;
    let submit = || submit_standalone(&repo, &run, &prompt, SessionMode::ReadOnly);
    store.set_job_queue_paused(true).await?;
    assert!(store.next_workflow_agent().await?.is_none());
    assert!(stage.enqueue(&store, submit()).await.is_err());
    assert!(
        store
            .workflow_operator_jobs(project.clone(), run.id.clone())
            .await?
            .is_empty()
    );
    drop(store);

    let store = Store::open(database.clone())?;
    assert!(store.next_workflow_agent().await?.is_none());
    store.set_job_queue_paused(false).await?;
    let stage = store
        .next_workflow_agent()
        .await?
        .context("Agent dispatch was lost on restart")?;
    stage.enqueue(&store, submit()).await?;
    assert!(stage.enqueue(&store, submit()).await.is_err());
    assert!(stage.reconcile(&store).await?);
    drop(store);

    let reopened = Store::open(database)?;
    assert!(reopened.next_workflow_agent().await?.is_none());
    let jobs = reopened
        .workflow_operator_jobs(project, run.id.clone())
        .await?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].node_id, "agent");
    assert_eq!(jobs[0].inputs, submit().artifacts);
    let attempt = reopened
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-agent-runner".into(),
            bridge_available: false,
        })
        .await?
        .context("Automatic job did not enter ordinary queue admission")?;
    assert_eq!(attempt.input[0].text, "Inspect the pinned inputs.");
    Ok(())
}

#[tokio::test]
async fn generic_agent_queues_the_wired_prompt_and_seals_one_read_only_response() -> Result<()> {
    const PROMPT: &str = "  Read the fixture and identify the exact marker.\n";
    const ANSWER: &str = "The exact marker is fixture-blue.";
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("agent.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id.clone();
    let (run, prompt) = launch_standalone(
        &store,
        &repo,
        "generic-agent",
        standalone_graph(PROMPT, Default::default())?,
    )
    .await?;
    assert_eq!(prompt.value, ArtifactValue::RenderedText(PROMPT.into()));
    let inputs = NodeInputs::from([("prompt".into(), prompt.id.clone())]);
    let target = store
        .workflow_operator_agent_target(&project, &run.id, "agent", None, &inputs)
        .await?;
    assert_eq!(target.checkout_id, repo.checkout_id);
    assert_eq!(target.mode, SessionMode::ReadOnly);
    let submission = || submit_standalone(&repo, &run, &prompt, SessionMode::ReadOnly);
    let mut wrong = submission();
    wrong.scope.model = Some("different".into());
    assert!(store.submit_workflow_agent(wrong).await.is_err());
    let mut wrong = submission();
    wrong.scope.mode = SessionMode::ApprovedWrite;
    wrong.request.mode = SessionMode::ApprovedWrite;
    assert!(store.submit_workflow_agent(wrong).await.is_err());
    let mut wrong = submission();
    wrong
        .artifacts
        .insert("prompt".into(), "forged-artifact".into());
    assert!(store.submit_workflow_agent(wrong).await.is_err());
    assert!(
        store
            .submit_workflow_agent_with_comparison(
                submission(),
                Some(ComparisonSelection {
                    group_id: "unwired".into(),
                    run_ids: vec!["unwired".into()],
                })
            )
            .await
            .is_err()
    );
    let job = store.submit_workflow_agent(submission()).await?;
    assert!(store.submit_workflow_agent(submission()).await.is_err());
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-agent-runner".into(),
            bridge_available: false,
        })
        .await?
        .context("The Agent job was not admitted")?;
    assert_eq!(attempt.input.len(), 1);
    assert_eq!(attempt.input[0].text, PROMPT);
    let source_run = attempt.run.id.clone();
    let dispatch = complete_response(&store, &attempt, ANSWER, None).await?;
    assert!(
        store
            .workflow_run_artifacts(project.clone(), run.id.clone())
            .await?
            .iter()
            .any(|artifact| artifact.node_instance_id == "agent" && artifact.port == "response"),
        "The regular terminal path should settle the stage before its inspector is opened"
    );
    drop(store);

    let store = Store::open(database)?;
    let inspection = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("The workflow did not reopen")?;
    let control = inspection
        .control
        .iter()
        .find(|receipt| receipt.node_id == "agent")
        .context("The Agent did not settle")?;
    assert_eq!(control.state, ControlStageState::Completed);
    assert_eq!(control.selected_ports, ["completed"]);
    assert_eq!(control.source_run_id.as_deref(), Some(source_run.as_str()));
    let output = inspection
        .outputs
        .iter()
        .find(|output| output.artifact.node_instance_id == "agent")
        .context("The Agent response did not survive restart")?;
    let ArtifactValue::AgentResponse(response) = &output.artifact.value else {
        anyhow::bail!("Wrong Agent output type");
    };
    assert_eq!(response.job_id, job.id);
    assert_eq!(response.run_id, source_run);
    assert_eq!(response.prompt_artifact_id, prompt.id);
    assert_eq!(response.dispatch_id, dispatch);
    assert_eq!(output.input_receipts[0].inputs, inputs);
    assert_eq!(
        store
            .workflow_agent_response(project.clone(), run.id.clone(), output.artifact.id.clone())
            .await?,
        ANSWER
    );
    assert!(
        store
            .workflow_agent_response(
                "different-project".into(),
                run.id.clone(),
                output.artifact.id.clone()
            )
            .await
            .is_err()
    );
    store
        .execute(move |db| {
            db.execute(
                "INSERT INTO history_deleted_dispatch_payloads VALUES (?1)",
                [dispatch],
            )?;
            Ok(())
        })
        .await?;
    assert!(
        store
            .workflow_agent_response(project, run.id, output.artifact.id.clone())
            .await
            .is_err()
    );
    Ok(())
}

async fn complete_response(
    store: &Store,
    attempt: &crate::conversations::AttemptPlan,
    answer: &str,
    goal_action: Option<GoalTurnAction>,
) -> Result<String> {
    let source_run = &attempt.run.id;
    store.mark_agent_running(source_run.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version":1, "contextId":attempt.context_id, "scope":attempt.scope,
        "submittedMessages":attempt.input, "nativeHistory":{"mode":"fresh"},
        "questionRoute": attempt.question_route,
    }))?;
    let dispatch = store
        .prepare_dispatch(
            source_run.clone(),
            snapshot,
            vec![b"agent fixture".to_vec()],
        )
        .await?;
    store
        .settle_dispatch(dispatch.clone(), Delivery::Submitted)
        .await?;
    store
        .record_agent_observation(
            source_run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some(format!("fixture-session-{source_run}")),
                    turn_id: Some(format!("fixture-turn-{source_run}")),
                    ..Default::default()
                },
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    if let Some(action) = goal_action {
        assert!(
            store
                .record_goal_turn_report(
                    source_run.clone(),
                    GoalTurnReport {
                        action,
                        evidence: "The claimed result is ready for operator review".into()
                    }
                )
                .await?
        );
    }
    store
        .finish_observed_agent_attempt(
            source_run.clone(),
            RunStatus::Succeeded,
            Some(answer.into()),
            None,
            true,
        )
        .await?;
    Ok(dispatch)
}

#[tokio::test]
async fn generic_agent_retains_only_the_selected_logical_unit_inputs_and_response() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("units.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.nodes.extend([
        node(
            "unit-prompt",
            BuiltinNode::BuildText,
            serde_json::from_value(json!({
                "kind":"build_text", "template":"Review this unit's selection."
            }))?,
        ),
        node(
            "unit-agent",
            BuiltinNode::Agent,
            NodeConfiguration::AgentRole {
                role: "unit reader".into(),
                model: None,
                requirements: StageRequirementConfiguration {
                    plan: Some(StagePlanConfiguration {
                        question_route: QuestionRoute::Native,
                        max_subagents: 0,
                    }),
                    ..Default::default()
                },
            },
        ),
    ]);
    graph.wires.extend([
        wire("implement", "implemented", "unit-prompt", "ready"),
        wire("unit-prompt", "completed", "unit-agent", "ready"),
        wire("unit-prompt", "text", "unit-agent", "prompt"),
    ]);
    let project = repo.project_id.clone();
    let version = store
        .save_workflow_revision(project.clone(), "generic-agent-per-unit".into(), graph)
        .await?;
    let run = store.start_workflow_run(project.clone(), version).await?;
    let fixture =
        approve_delivery_fixture_in_run(&store, dir.path(), repo.clone(), None, run).await?;
    let unit = &fixture.units[0];
    let unit_id = &unit.unit.id;
    let checkout = &fixture.checkout;
    let run = &fixture.run.id;
    let ArtifactValue::Release(release) = &fixture.release.value else {
        anyhow::bail!("The approval fixture did not issue a write release");
    };
    let submission = |node: &str, mode, artifacts: NodeInputs| WorkflowAgentSubmission {
        project_id: project.clone(),
        dispatch: PipelineDispatch {
            pipeline_run_id: run.clone(),
            node_instance_id: node.into(),
            plan_revision: Some(unit.plan_revision.clone()),
            release_id: (mode == SessionMode::ApprovedWrite).then(|| release.release.id.clone()),
        },
        scope: SessionScope {
            provider: Provider::Codex,
            profile_id: "fixture-profile".into(),
            account_binding: None,
            executable: std::env::current_exe().unwrap(),
            checkout: checkout.checkout_root.clone().into(),
            cli_version: "0.169.0".into(),
            runner: "fixture-unit-agent".into(),
            permission_revision: "fixture".into(),
            mode,
            model: Some("fixture-model".into()),
            effort: None,
        },
        request: JobRequest {
            checkout_id: Some(checkout.checkout_id.clone()),
            task_id: Some(unit.task_id.clone()),
            mode,
            instructions: "Fixture unit instruction".into(),
            dependencies: Vec::new(),
            requires_bridge: false,
        },
        artifacts,
        input: PipelineAgentInput::default(),
        unit_id: Some(unit_id.clone()),
    };
    store
        .submit_workflow_agent(submission(
            "implement",
            SessionMode::ApprovedWrite,
            NodeInputs::from([
                ("plan".into(), fixture.plan.id.clone()),
                ("release".into(), fixture.release.id.clone()),
                ("worktree".into(), fixture.worktree.id.clone()),
            ]),
        ))
        .await?;
    let admission = AdmissionEnvironment {
        runner: "fixture-unit-agent".into(),
        bridge_available: false,
    };
    let writer = store
        .claim_next_job(admission.clone())
        .await?
        .context("The fixture writer did not start")?;
    let writer_run = writer.run.id;
    store.mark_agent_running(writer_run.clone()).await?;
    assert!(repository::capture_worktree_start(&store, writer_run.clone()).await?);
    std::fs::write(
        Path::new(&checkout.checkout_root).join("first.txt"),
        "Selected test candidate\n",
    )?;
    repository::save_worktree_candidate(&store, writer_run.clone()).await?;
    store
        .finish_agent_attempt(writer_run.clone(), RunStatus::Succeeded, None)
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.clone(), writer_run.clone())
        .await?
    else {
        anyhow::bail!("The implementation fixture did not capture a candidate");
    };
    let candidate = store
        .select_worktree_candidate(
            CandidateTarget {
                project_id: project.clone(),
                task_id: unit.task_id.clone(),
                unit_id: unit_id.clone(),
                capture_revision: artifact.revision,
                run_id: writer_run,
            },
            CandidateSelection {
                tracked_paths: Some(vec!["first.txt".into()]),
                untracked_paths: Vec::new(),
            },
        )
        .await?;
    store
        .publish_workflow_candidate(
            project.clone(),
            run.clone(),
            "implement".into(),
            unit.task_id.clone(),
            candidate.id,
        )
        .await?;
    let observed = store
        .inspect_workflow_execution(project.clone(), run.clone())
        .await?
        .context("The run is unavailable")?;
    let prompt = observed
        .outputs
        .into_iter()
        .find(|output| {
            output.artifact.node_instance_id == "unit-prompt"
                && output.artifact.unit_id.as_deref() == Some(unit_id.as_str())
        })
        .context("The unit prompt did not settle")?
        .artifact;
    let artifacts = NodeInputs::from([("prompt".into(), prompt.id.clone())]);
    let target = store
        .workflow_operator_agent_target(&project, run, "unit-agent", Some(unit_id), &artifacts)
        .await?;
    assert_eq!(target.checkout_id, checkout.checkout_id);
    assert_eq!(target.mode, SessionMode::Plan);
    assert_eq!(target.plan_revision.as_ref(), Some(&unit.plan_revision));
    assert_eq!(target.task_id.as_ref(), Some(&unit.task_id));
    assert!(
        store
            .workflow_operator_agent_target(
                &project,
                run,
                "unit-agent",
                Some(&fixture.units[1].unit.id),
                &artifacts
            )
            .await
            .is_err()
    );
    let mut wrong_checkout = submission("unit-agent", SessionMode::Plan, artifacts.clone());
    wrong_checkout.scope.checkout = repo.checkout_root.into();
    wrong_checkout.request.checkout_id = Some(repo.checkout_id);
    assert!(store.submit_workflow_agent(wrong_checkout).await.is_err());
    let job = store
        .submit_workflow_agent(submission(
            "unit-agent",
            SessionMode::Plan,
            artifacts.clone(),
        ))
        .await?;
    let admitted = store
        .claim_next_job(admission)
        .await?
        .context("The unit Agent did not start")?;
    assert_eq!(admitted.run.checkout_id, checkout.checkout_id);
    assert!(admitted.input[0].text.contains(&unit.unit.purpose));
    assert!(
        admitted.input[0]
            .text
            .starts_with("Review this unit's selection.")
    );
    complete_response(&store, &admitted, "Unit-specific response", None).await?;
    let observed = store
        .inspect_workflow_execution(project.clone(), run.clone())
        .await?
        .context("The settled unit run is unavailable")?;
    let output = observed
        .outputs
        .iter()
        .find(|output| output.artifact.node_instance_id == "unit-agent")
        .context("The response was not retained")?;
    assert_eq!(output.artifact.unit_id.as_deref(), Some(unit_id.as_str()));
    assert_eq!(output.input_receipts[0].execution_key, unit.plan_revision);
    assert_eq!(output.input_receipts[0].inputs, artifacts);
    let ArtifactValue::AgentResponse(response) = &output.artifact.value else {
        anyhow::bail!("The unit Agent returned a different artifact kind");
    };
    assert_eq!(response.job_id, job.id);
    assert_eq!(
        store
            .workflow_agent_response(project, run.clone(), output.artifact.id.clone())
            .await?,
        "Unit-specific response"
    );
    Ok(())
}

#[tokio::test]
async fn generic_agent_plan_modes_use_pinned_question_routes_without_a_write_release() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("plan.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = &repo.project_id;
    for (id, route, mode) in [
        ("native-plan", QuestionRoute::Native, SessionMode::Plan),
        (
            "structured-plan",
            QuestionRoute::StructuredTurn,
            SessionMode::ReadOnly,
        ),
    ] {
        let plan = StagePlanConfiguration {
            question_route: route,
            max_subagents: 0,
        };
        let graph = standalone_graph(
            "Plan without changing files.",
            StageRequirementConfiguration {
                plan: Some(plan),
                ..Default::default()
            },
        )?;
        let (run, prompt) = launch_standalone(&store, &repo, id, graph).await?;
        let proposed = submit_standalone(&repo, &run, &prompt, mode);
        let target = store
            .workflow_operator_agent_target(project, &run.id, "agent", None, &proposed.artifacts)
            .await?;
        assert_eq!(target.mode, mode);
        assert!(target.plan_revision.is_none() && target.release_id.is_none());
        let wrong_mode = if mode == SessionMode::Plan {
            SessionMode::ReadOnly
        } else {
            SessionMode::Plan
        };
        assert!(
            store
                .submit_workflow_agent(submit_standalone(&repo, &run, &prompt, wrong_mode))
                .await
                .is_err()
        );
        let job = store.submit_workflow_agent(proposed).await?;
        let attempt = store
            .claim_next_job(AdmissionEnvironment {
                runner: "fixture-agent-runner".into(),
                bridge_available: false,
            })
            .await?
            .context("The configured Plan Mode did not reach the scheduler")?;
        assert_eq!(attempt.scope.mode, mode);
        assert_eq!(attempt.question_route, route);
        assert_eq!(attempt.input[0].text, "Plan without changing files.");
        assert!(!attempt.requires_bridge);
        assert!(
            job.pipeline
                .as_ref()
                .is_some_and(|origin| origin.release.is_none())
        );
        complete_response(&store, &attempt, "A read-only proposal", None).await?;
        let result = store
            .workflow_run_artifacts(project.clone(), run.id.clone())
            .await?;
        assert!(
            result
                .iter()
                .any(|artifact| matches!(artifact.value, ArtifactValue::AgentResponse(_)))
        );
        assert!(result.iter().all(|artifact| !matches!(
            artifact.value,
            ArtifactValue::Plan { .. } | ArtifactValue::Release(_)
        )));
    }
    Ok(())
}

#[tokio::test]
async fn generic_agent_goal_waits_for_human_evidence_before_publishing() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("goal.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = &repo.project_id;
    let goal = StageGoalConfiguration {
        objective: "Read the selected scope".into(),
        criteria: vec![StageGoalCriterion {
            text: "An operator verified the response.".into(),
            evidence: StageGoalEvidence::OperatorReview,
        }],
        max_turns: Some(2),
        max_seconds: None,
        token_budget: None,
    };
    let invalid = standalone_graph(
        "Inspect the fixture.",
        StageRequirementConfiguration {
            goal: Some(goal.clone()),
            plan: Some(StagePlanConfiguration {
                question_route: QuestionRoute::Native,
                max_subagents: 0,
            }),
            ..Default::default()
        },
    )?;
    assert!(
        store
            .save_workflow_revision(project.clone(), "mixed-modes".into(), invalid)
            .await
            .is_err()
    );
    let graph = standalone_graph(
        "Inspect the fixture.",
        StageRequirementConfiguration {
            goal: Some(goal),
            ..Default::default()
        },
    )?;
    let (run, prompt) = launch_standalone(&store, &repo, "generic-goal", graph).await?;
    let job = store
        .submit_workflow_agent(submit_standalone(
            &repo,
            &run,
            &prompt,
            SessionMode::ReadOnly,
        ))
        .await?;
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-agent-runner".into(),
            bridge_available: true,
        })
        .await?
        .context("The managed Goal did not reach the scheduler")?;
    assert_eq!(job.mode, SessionMode::ReadOnly);
    assert!(attempt.requires_bridge && attempt.native_goal_seed.is_none());
    assert!(attempt.input[0].text.contains("Read the selected scope"));
    let run_id = &attempt.run.id;
    complete_response(
        &store,
        &attempt,
        "Exact Goal evidence",
        Some(GoalTurnAction::Review),
    )
    .await?;
    assert!(
        store
            .workflow_run_artifacts(project.clone(), run.id.clone())
            .await?
            .iter()
            .all(|artifact| artifact.port != "response")
    );
    store
        .record_goal_criterion_evidence(
            project.clone(),
            run_id.clone(),
            1,
            GoalCriterionEvidence::OperatorReview {
                approved: true,
                note: Some("Confirmed the retained response".into()),
            },
        )
        .await?;
    let outputs = store
        .workflow_run_artifacts(project.clone(), run.id.clone())
        .await?;
    let output = outputs
        .iter()
        .find(|artifact| artifact.node_instance_id == "agent" && artifact.port == "response")
        .context("Completed verification did not publish the original Goal response")?;
    assert_eq!(
        store
            .workflow_agent_response(project.clone(), run.id, output.id.clone())
            .await?,
        "Exact Goal evidence"
    );
    Ok(())
}

#[tokio::test]
async fn generic_agent_terminal_failure_selects_one_branch_and_rejects_same_run_retry() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("failure.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = &repo.project_id;
    let mut graph = standalone_graph("Read a fixture.", Default::default())?;
    graph.nodes.push(node(
        "failure-note",
        BuiltinNode::BuildText,
        serde_json::from_value(
            json!({"kind":"build_text","template":"Record that the agent failed."}),
        )?,
    ));
    graph
        .wires
        .push(wire("agent", "failed", "failure-note", "ready"));
    let (run, prompt) = launch_standalone(&store, &repo, "failure", graph).await?;
    let job = store
        .submit_workflow_agent(submit_standalone(
            &repo,
            &run,
            &prompt,
            SessionMode::ReadOnly,
        ))
        .await?;
    let environment = AdmissionEnvironment {
        runner: "fixture-agent-runner".into(),
        bridge_available: false,
    };
    let attempt = store
        .claim_next_job(environment.clone())
        .await?
        .context("The test Agent did not start")?;
    let original_run = attempt.run.id;
    let original_prompt = attempt.input[0].text.clone();
    store
        .finish_agent_attempt(original_run.clone(), RunStatus::Interrupted, None)
        .await?;
    let requeued = store
        .job(project.clone(), job.id.clone())
        .await?
        .context("The unlaunched Agent job is unavailable")?;
    assert_eq!(requeued.queued_recovery, Some(RecoveryAction::Retry));
    let attempt = store
        .claim_next_job(environment.clone())
        .await?
        .context("The recoverable unlaunched attempt did not reuse the Agent job")?;
    assert_eq!(
        attempt.previous_run_id.as_deref(),
        Some(original_run.as_str())
    );
    assert_eq!(attempt.input[0].text, original_prompt);
    store.mark_agent_running(attempt.run.id.clone()).await?;
    store
        .finish_agent_attempt(attempt.run.id.clone(), RunStatus::Failed, None)
        .await?;
    let inspected = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("The workflow disappeared")?;
    let receipt = inspected
        .control
        .iter()
        .find(|receipt| receipt.node_id == "agent")
        .context("The failure branch did not settle")?;
    assert_eq!(receipt.selected_ports, ["failed"]);
    assert_eq!(
        receipt.source_run_id.as_deref(),
        Some(attempt.run.id.as_str())
    );
    assert!(
        inspected
            .outputs
            .iter()
            .any(|output| output.artifact.node_instance_id == "failure-note")
    );
    assert!(
        inspected
            .outputs
            .iter()
            .all(|output| output.artifact.node_instance_id != "agent")
    );
    let retry = store
        .queue_job_recovery(
            project.clone(),
            job.id,
            attempt.run.id,
            RecoveryAction::Retry,
        )
        .await
        .unwrap_err();
    assert!(retry.to_string().contains("already selected an outcome"));
    assert!(store.claim_next_job(environment).await?.is_none());
    Ok(())
}
