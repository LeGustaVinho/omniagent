use std::{collections::BTreeSet, path::PathBuf};

use super::*;
use crate::{
    conversations::AttemptPlan,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::{AdmissionEnvironment, GoalCriterionVerification, GoalTurnAction, GoalTurnReport},
    providers::goal::NativeGoalAction,
    providers::{ProviderEvent, ProviderEventData, ProviderLocation, TurnOutcome},
    runs::RunStatus,
    workflows::{
        ArtifactValue, DeliveryRoles, NodeInputs, NodeInstance, WorkflowAgentSubmission,
        authoring::package::{PackageArtifactKind, PackageDestination},
        builtin_delivery_graph, builtin_registry,
        tests::{
            ApprovedDeliveryFixture, approve_delivery_fixture_in_run, approved_delivery_repository,
            submission,
        },
    },
};
use serde_json::json;

fn configuration(evidence: StageGoalEvidence) -> StageGoalConfiguration {
    StageGoalConfiguration {
        objective: "Implement and verify the bounded fixture".into(),
        criteria: vec![StageGoalCriterion {
            text: "Verify the recorded result".into(),
            evidence,
        }],
        max_turns: Some(3),
        max_seconds: Some(120),
        token_budget: None,
    }
}

fn stage(kind: BuiltinNode, role_revision: &str) -> PinnedNode {
    PinnedNode {
        instance: NodeInstance {
            id: "fixture-stage".into(),
            kind,
            configuration: NodeConfiguration::AgentRole {
                role: "implementation".into(),
                model: None,
                requirements: super::super::stage_requirements::StageRequirementConfiguration {
                    bound_role: Some(PackageArtifactVersion {
                        id: "role".into(),
                        revision: role_revision.into(),
                        kind: PackageArtifactKind::Role,
                        destination: PackageDestination::Project,
                    }),
                    ..Default::default()
                },
            },
        },
        revision: "stage-r2".into(),
        definition: builtin_registry()
            .into_iter()
            .find(|definition| definition.kind == kind)
            .unwrap(),
    }
}

fn scope(mode: SessionMode) -> SessionScope {
    SessionScope {
        mode,
        provider: Provider::Codex,
        profile_id: "profile".into(),
        account_binding: Some("fixture".into()),
        executable: PathBuf::from("C:/fixture/codex.exe"),
        checkout: PathBuf::from("D:/fixture/source"),
        cli_version: "fixture".into(),
        runner: "direct".into(),
        permission_revision: "bounded".into(),
        model: Some("fixture-model".into()),
        effort: None,
    }
}

#[test]
fn preserves_the_role_and_exact_goal_policy_in_the_stage_pin() -> Result<()> {
    let config = configuration(StageGoalEvidence::CandidateGate);
    let stage = stage(BuiltinNode::Implement, "role-r4");
    let pin = StageGoalPin::new(&stage, config.clone())?;
    assert_eq!(pin.role_revision.as_ref().unwrap().revision, "role-r4");
    assert_eq!(pin.stage_revision, "stage-r2");
    assert_eq!(pin.configuration, config);
    assert_eq!(
        pin.configuration.definition().criteria,
        ["Verify the recorded result"]
    );
    let decoded: StageGoalPin = serde_json::from_slice(&serde_json::to_vec(&pin)?)?;
    assert_eq!(decoded, pin);
    let changed = StageGoalPin::new(&stage, configuration(StageGoalEvidence::OperatorReview))?;
    assert_ne!(changed.configuration_revision, pin.configuration_revision);
    let invalid = StageGoalConfiguration {
        max_turns: None,
        max_seconds: None,
        ..config
    };
    assert!(StageGoalPin::new(&stage, invalid).is_err());
    Ok(())
}

#[tokio::test]
async fn planning_goal_publishes_the_pinned_plan_only_after_evidence() -> Result<()> {
    use crate::{
        providers::planning::PlanSource,
        workflows::{WorkflowHostRequest, WorkflowHostSubmission},
    };

    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("planner-goal.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id.clone();
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let plan = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap();
    let NodeConfiguration::AgentRole { requirements, .. } = &mut plan.configuration else {
        anyhow::bail!("planner has no role");
    };
    requirements.goal = Some(configuration(StageGoalEvidence::OperatorReview));
    let version = store
        .save_workflow_revision(project.clone(), "planner-goal".into(), graph)
        .await?;
    let workflow = store.start_workflow_run(project.clone(), version).await?;
    let host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: workflow.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let document = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            PathBuf::from(&repo.checkout_root).join("brief.md"),
        )
        .await?
        .revision;
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
    let mut request = submission(&repo, &workflow, "plan", None, SessionMode::Plan);
    request.artifacts = NodeInputs::from([("context".into(), context.id)]);
    let environment = AdmissionEnvironment {
        runner: request.scope.runner.clone(),
        bridge_available: true,
    };
    let job = store.submit_workflow_agent(request).await?;
    let attempt = store
        .claim_next_job(environment)
        .await?
        .context("Goal planner blocked before any Plan exists")?;
    assert_eq!(attempt.scope.mode, SessionMode::Plan);
    assert!(
        job.pipeline
            .as_ref()
            .is_some_and(|origin| origin.release.is_none())
    );
    let markdown = attempt
        .input
        .last()
        .context("pinned planning prompt missing")?;
    assert!(markdown.text.contains("omniagent-plan"));
    begin(&store, &attempt).await?;
    store.record_agent_observation(attempt.run.id.clone(), EventSource::Provider, ProviderEvent {
        location: ProviderLocation::default(),
        data: ProviderEventData::PlanProposed {
            text: "A bounded plan.\n\n```omniagent-plan\n{\"units\":[{\"purpose\":\"Plan scope\",\"completionCriteria\":[\"Evidence accepted\"]}]}\n```".into(),
            source: PlanSource::CodexPlanItem,
        },
    }).await?;
    finish(&store, &attempt, GoalTurnAction::Review).await?;
    let proposal = store
        .agent_observations(attempt.conversation_id.clone(), attempt.run.id.clone(), 0)
        .await?
        .into_iter()
        .find(|event| matches!(event.event.data, ProviderEventData::PlanProposed { .. }))
        .context("protected Plan event missing")?;
    let publish = || {
        store.publish_workflow_artifact(
            project.clone(),
            workflow.id.clone(),
            "plan".into(),
            "plan".into(),
            ArtifactValue::Plan {
                run_id: attempt.run.id.clone(),
                event_sequence: proposal.seq,
            },
        )
    };
    let before_review = publish().await.unwrap_err();
    assert!(
        before_review
            .to_string()
            .contains("waiting for verified result evidence")
    );
    let verification = store
        .record_goal_criterion_evidence(
            project.clone(),
            attempt.run.id.clone(),
            1,
            GoalCriterionEvidence::OperatorReview {
                approved: true,
                note: Some("The exact proposal covers the requested scope".into()),
            },
        )
        .await?;
    assert_eq!(verification.status, GoalVerificationStatus::Completed);
    assert!(
        matches!(publish().await?.value, ArtifactValue::Plan { run_id, .. } if run_id == attempt.run.id)
    );
    Ok(())
}

#[test]
fn refuses_unsupported_stage_modes_and_preserves_planning_scope() -> Result<()> {
    let gate = configuration(StageGoalEvidence::CandidateGate);
    let operator = configuration(StageGoalEvidence::OperatorReview);
    assert!(StageGoalPin::new(&stage(BuiltinNode::Plan, "role-r3"), gate).is_err());
    assert!(
        StageGoalPin::new(
            &stage(BuiltinNode::ReviewCandidate, "role-r3"),
            operator.clone()
        )
        .is_err()
    );
    assert!(StageGoalPin::new(&stage(BuiltinNode::Command, "role-r3"), operator.clone()).is_err());
    let plan = StageGoalPin::new(&stage(BuiltinNode::Plan, "role-r3"), operator)?;
    assert_eq!(plan.configuration.max_turns, Some(3));
    let mut time_bounded = plan.configuration;
    time_bounded.max_turns = None;
    assert!(matches!(
        assess_route(
            &time_bounded,
            &scope(SessionMode::Plan),
            true,
            Some(&proof(true))
        ),
        StageGoalRoute::Managed { .. }
    ));
    Ok(())
}

fn proof(token_budget_verified: bool) -> NativeGoalEvidence {
    use NativeGoalAction::*;
    NativeGoalEvidence {
        scope_key: "fixture-qualified-scope".into(),
        evidence_id: "native-lifecycle-evidence".into(),
        controls: BTreeSet::from([SeedContext, Start, Status, Pause, Resume, Cancel]),
        token_budget_verified,
    }
}

#[test]
fn exposes_native_only_for_registered_controls_compatible_with_the_goal_limits() {
    let scope = scope(SessionMode::ApprovedWrite);
    let goal = configuration(StageGoalEvidence::CandidateGate);
    assert!(matches!(
        assess_route(&goal, &scope, true, Some(&proof(true))),
        StageGoalRoute::Managed { .. }
    ));
    assert!(matches!(
        assess_route(&goal, &scope, false, None),
        StageGoalRoute::Blocked { .. }
    ));
    let token_goal = StageGoalConfiguration {
        max_turns: None,
        token_budget: Some(500),
        ..goal
    };
    assert!(matches!(
        assess_route(&token_goal, &scope, true, None),
        StageGoalRoute::Blocked { .. }
    ));
    assert!(matches!(
        assess_route(&token_goal, &scope, true, Some(&proof(false))),
        StageGoalRoute::Blocked { .. }
    ));
    let mut incomplete = proof(true);
    incomplete.controls.remove(&NativeGoalAction::Resume);
    assert!(matches!(
        assess_route(&token_goal, &scope, true, Some(&incomplete)),
        StageGoalRoute::Blocked { .. }
    ));
    assert_eq!(
        assess_route(&token_goal, &scope, true, Some(&proof(true))),
        StageGoalRoute::NativeCandidate {
            evidence_id: "native-lifecycle-evidence".into()
        }
    );
}

#[test]
fn checks_evidence_kind_and_candidate_scope_before_honoring_a_completed_verdict() -> Result<()> {
    let policy = configuration(StageGoalEvidence::CandidateGate);
    let pin = StageGoalPin::new(&stage(BuiltinNode::Implement, "role-r3"), policy)?;
    let gate = GoalCriterionEvidence::CandidateGate {
        verification_id: "verified-check".into(),
        gate_index: 0,
    };
    let manual = GoalCriterionEvidence::OperatorReview {
        approved: true,
        note: None,
    };
    pin.require_criterion_evidence(1, &gate)?;
    assert!(pin.require_criterion_evidence(1, &manual).is_err());
    assert!(pin.require_criterion_evidence(2, &gate).is_err());
    let mut completed = GoalVerification {
        revision_id: "goal-r2".into(),
        run_id: "terminal-attempt".into(),
        context_id: "context".into(),
        provider_result: Some("completed".into()),
        candidate_id: Some("exact-candidate".into()),
        status: GoalVerificationStatus::Completed,
        criteria: vec![GoalCriterionVerification {
            ordinal: 1,
            text: "Verify the recorded result".into(),
            status: GoalCriterionStatus::Passed,
            evidence: Some(gate),
        }],
    };
    assert!(
        pin.require_verified_result(
            "goal-r2",
            "intermediate-attempt",
            Some("exact-candidate"),
            &completed
        )
        .is_err()
    );
    assert!(
        pin.require_verified_result(
            "goal-r1",
            "terminal-attempt",
            Some("exact-candidate"),
            &completed
        )
        .is_err()
    );
    assert!(
        pin.require_verified_result(
            "goal-r2",
            "terminal-attempt",
            Some("other-candidate"),
            &completed
        )
        .is_err()
    );
    pin.require_verified_result(
        "goal-r2",
        "terminal-attempt",
        Some("exact-candidate"),
        &completed,
    )?;
    completed.criteria[0].evidence = Some(manual);
    assert!(
        pin.require_verified_result(
            "goal-r2",
            "terminal-attempt",
            Some("exact-candidate"),
            &completed
        )
        .is_err()
    );
    Ok(())
}

async fn begin(store: &Store, attempt: &AttemptPlan) -> Result<()> {
    store.mark_agent_running(attempt.run.id.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 1,
        "contextId": attempt.context_id,
        "scope": attempt.scope,
        "submittedMessages": attempt.input,
        "nativeHistory": { "mode": "fresh" },
        "questionRoute": attempt.question_route,
    }))?;
    let dispatch = store
        .prepare_dispatch(
            attempt.run.id.clone(),
            snapshot,
            vec![b"workflow goal fixture".to_vec()],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await
}

async fn finish(store: &Store, attempt: &AttemptPlan, action: GoalTurnAction) -> Result<RunStatus> {
    assert!(
        store
            .record_goal_turn_report(
                attempt.run.id.clone(),
                GoalTurnReport {
                    evidence: "Recorded evidence for the current goal checkpoint".into(),
                    action,
                }
            )
            .await?
    );
    store
        .record_agent_observation(
            attempt.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some("stage-goal-fixture".into()),
                    ..Default::default()
                },
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_observed_agent_attempt(
            attempt.run.id.clone(),
            RunStatus::Succeeded,
            Some("Checkpoint".into()),
            None,
            true,
        )
        .await
}

async fn goal_delivery(
    store: &Store,
    root: &std::path::Path,
    evidence: StageGoalEvidence,
    checks: Vec<String>,
) -> Result<(
    ApprovedDeliveryFixture,
    WorkflowAgentSubmission,
    NodeInstance,
)> {
    let repository = approved_delivery_repository(store, root).await?;
    let project = repository.project_id.clone();
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let implement = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "implement")
        .unwrap();
    let NodeConfiguration::AgentRole { requirements, .. } = &mut implement.configuration else {
        anyhow::bail!("implementation stage has no role");
    };
    requirements.goal = Some(configuration(evidence));
    let draft = implement.clone();
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "verify-candidate")
        .unwrap()
        .configuration = NodeConfiguration::RequiredChecks { names: checks };
    let version = store
        .save_workflow_revision(project.clone(), "workflow-goal".into(), graph)
        .await?;
    let workflow = store.start_workflow_run(project.clone(), version).await?;
    let fixture = approve_delivery_fixture_in_run(store, root, repository, None, workflow).await?;
    let unit = &fixture.units[0];
    let ArtifactValue::Release(release) = &fixture.release.value else {
        anyhow::bail!("no approved release");
    };
    let mut request = submission(
        &fixture.checkout,
        &fixture.run,
        "implement",
        Some(unit.plan_revision.clone()),
        SessionMode::ApprovedWrite,
    );
    request.scope.executable = PathBuf::from(&fixture.checkout.repository_root).join("codex.exe");
    request.request.task_id = Some(unit.task_id.clone());
    request.dispatch.release_id = Some(release.release.id.clone());
    request.unit_id = Some(unit.unit.id.clone());
    request.artifacts = NodeInputs::from([
        ("plan".into(), fixture.plan.id.clone()),
        ("worktree".into(), fixture.worktree.id.clone()),
        ("release".into(), fixture.release.id.clone()),
    ]);
    Ok((fixture, request, draft))
}

#[tokio::test]
async fn managed_stage_keeps_its_typed_inputs_and_stops_when_release_is_revoked() -> Result<()> {
    use crate::{
        jobs::GoalContinuationOwner,
        repository::{self, WorktreeCheckpointMismatch},
    };

    let temp = tempfile::tempdir()?;
    let database = temp.path().join("workflow-goal.sqlite");
    let store = Store::open(database.clone())?;
    let (fixture, request, draft) = goal_delivery(
        &store,
        temp.path(),
        StageGoalEvidence::OperatorReview,
        Vec::new(),
    )
    .await?;
    let project = fixture.project.clone();
    let workflow = fixture.run.id.clone();
    let unit = &fixture.units[0];
    let release = request
        .dispatch
        .release_id
        .clone()
        .context("no approved release")?;
    let environment = AdmissionEnvironment {
        runner: request.scope.runner.clone(),
        bridge_available: true,
    };
    let assessment = store
        .workflow_stage_goal_assessment(
            project.clone(),
            workflow.clone(),
            "implement".into(),
            fixture.checkout.checkout_id.clone(),
            request.scope.clone(),
            true,
        )
        .await?
        .context("missing workflow goal config")?;
    assert!(matches!(assessment.route, StageGoalRoute::Managed { .. }));
    let preview = store
        .preview_workflow_stage_goal(
            project.clone(),
            fixture.checkout.checkout_id.clone(),
            draft.clone(),
            request.scope.clone(),
            true,
        )
        .await?
        .context("missing draft goal config")?;
    assert_eq!(preview.pin, assessment.pin);
    assert_eq!(preview.route, assessment.route);
    let mut read_only = request.scope.clone();
    read_only.mode = SessionMode::ReadOnly;
    let mismatch = store
        .preview_workflow_stage_goal(
            project.clone(),
            fixture.checkout.checkout_id.clone(),
            draft,
            read_only,
            true,
        )
        .await?
        .context("mismatched profile was not assessed")?;
    assert!(matches!(mismatch.route, StageGoalRoute::Blocked { .. }));
    let job = store.submit_workflow_agent(request).await?;
    let initial = match store.claim_next_job(environment.clone()).await? {
        Some(attempt) => attempt,
        None => anyhow::bail!(
            "initial stage blocked: {:?}",
            store
                .inspect_job(project.clone(), job.id.clone(), environment.clone())
                .await?
                .map(|job| job.blockers)
        ),
    };
    assert!(initial.requires_bridge);
    let prompt = &initial
        .input
        .last()
        .context("missing dispatched objective")?
        .text;
    assert!(prompt.contains(&assessment.pin.configuration.objective));
    assert!(prompt.contains(&unit.unit.purpose));
    let input = store
        .workflow_attempt_input(initial.run.id.clone())
        .await?
        .context("initial typed input lost")?;
    let origin = store
        .attempt_pipeline_origin(project.clone(), initial.run.id.clone())
        .await?
        .context("initial release lost")?;
    assert!(repository::capture_worktree_start(&store, initial.run.id.clone()).await?);
    begin(&store, &initial).await?;
    let changed = initial.scope.checkout.join("first.txt");
    std::fs::write(&changed, "first turn's selected work\n")?;
    repository::save_worktree_candidate(&store, initial.run.id.clone()).await?;
    assert_eq!(
        finish(&store, &initial, GoalTurnAction::Continue).await?,
        RunStatus::Succeeded
    );
    drop(store);

    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let active = store
        .active_pipeline_goal(project.clone(), workflow.clone(), "implement".into())
        .await?
        .context("workflow goal lost on restart")?;
    assert_ne!(active.job.id, job.id);
    let continuation_id = active.job.id.clone();
    assert_eq!(active.continuation_owner, GoalContinuationOwner::Managed);
    let checkpoint = active.checkpoint.context("first turn checkpoint lost")?;
    assert_eq!(checkpoint.run_id, initial.run.id);
    assert_eq!(checkpoint.next_action, "continue");
    let replay = (checkpoint.run_id, checkpoint.context_id);
    store
        .execute(move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            crate::jobs::accept_goal_checkpoint(&tx, &replay.0, &replay.1)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    let next = match store.claim_next_job(environment.clone()).await? {
        Some(attempt) => attempt,
        None => anyhow::bail!(
            "managed continuation not admitted: {:?}",
            store
                .inspect_job(project.clone(), continuation_id, environment.clone())
                .await?
                .map(|job| job.blockers)
        ),
    };
    assert_eq!(
        next.previous_run_id.as_deref(),
        Some(initial.run.id.as_str())
    );
    assert!(next.scope == initial.scope);
    assert_eq!(
        store.workflow_attempt_input(next.run.id.clone()).await?,
        Some(input)
    );
    assert_eq!(
        store
            .attempt_pipeline_origin(project.clone(), next.run.id.clone())
            .await?,
        Some(origin)
    );
    std::fs::write(&changed, "another operation changed this worktree\n")?;
    let drift = repository::capture_worktree_start(&store, next.run.id.clone())
        .await
        .unwrap_err();
    assert!(drift.is::<WorktreeCheckpointMismatch>());
    std::fs::write(&changed, "first turn's selected work\n")?;
    assert!(repository::capture_worktree_start(&store, next.run.id.clone()).await?);
    begin(&store, &next).await?;
    store
        .revoke_job_pipeline_release(project.clone(), release)
        .await?;
    repository::save_worktree_candidate(&store, next.run.id.clone()).await?;
    assert_eq!(
        finish(&store, &next, GoalTurnAction::Continue).await?,
        RunStatus::Interrupted
    );
    let stopped = store
        .active_pipeline_goal(project.clone(), workflow.clone(), "implement".into())
        .await?
        .context("stopped goal revision disappeared")?;
    assert_eq!(stopped.control_state, crate::jobs::GoalControlState::Paused);
    assert_eq!(
        stopped.checkpoint.context("prior checkpoint lost")?.run_id,
        initial.run.id
    );
    assert!(
        store
            .resume_goal(project.clone(), stopped.id)
            .await
            .is_err()
    );
    assert!(store.claim_next_job(environment).await?.is_none());
    store
        .execute(move |db| {
            assert!(
                require_stage_goal_result(db, &project, &workflow, "implement", &next.run.id, None)
                    .is_err()
            );
            Ok(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn candidate_checks_use_the_pinned_gate_and_publish_only_the_verified_selection() -> Result<()>
{
    use crate::repository::{
        self, AttemptWorktreeEvidence,
        verification::{
            CandidateGate, CandidateGateAction, CandidateGateOutcome, CandidateSelection,
            CandidateTarget,
        },
    };
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("workflow-goal-gate.sqlite"))?;
    let name = "fixture selected build";
    let (fixture, request, _) = goal_delivery(
        &store,
        temp.path(),
        StageGoalEvidence::CandidateGate,
        vec![name.into()],
    )
    .await?;
    let project = fixture.project.clone();
    let workflow = fixture.run.id.clone();
    let unit = &fixture.units[0];
    let task = unit.task_id.clone();
    let executable =
        PathBuf::from(std::env::var_os("SystemRoot").context("missing Windows directory")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    store.set_candidate_gate_policy(project.clone(), vec![CandidateGate {
        name: name.into(), required: false,
        action: CandidateGateAction::Command { executable,
            arguments: vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
                "if ((Get-Content -LiteralPath 'first.txt' -Raw).Trim() -eq 'selected goal result') { exit 0 } else { exit 17 }".into()],
            timeout_seconds: Some(30),
        },
    }]).await?;
    let environment = AdmissionEnvironment {
        runner: request.scope.runner.clone(),
        bridge_available: true,
    };
    store.submit_workflow_agent(request).await?;
    let attempt = store
        .claim_next_job(environment)
        .await?
        .context("goal writer was not admitted")?;
    assert!(repository::capture_worktree_start(&store, attempt.run.id.clone()).await?);
    begin(&store, &attempt).await?;
    std::fs::write(
        attempt.scope.checkout.join("first.txt"),
        "selected goal result\n",
    )?;
    std::fs::write(
        attempt.scope.checkout.join("second.txt"),
        "another edit excluded from the selection\n",
    )?;
    repository::save_worktree_candidate(&store, attempt.run.id.clone()).await?;
    assert_eq!(
        finish(&store, &attempt, GoalTurnAction::Review).await?,
        RunStatus::Succeeded
    );
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.clone(), attempt.run.id.clone())
        .await?
    else {
        anyhow::bail!("goal writer candidate was not captured");
    };
    let target = CandidateTarget {
        project_id: project.clone(),
        task_id: task.clone(),
        unit_id: unit.unit.id.clone(),
        run_id: attempt.run.id.clone(),
        capture_revision: artifact.revision,
    };
    let select = |path: &str| {
        store.select_worktree_candidate(
            target.clone(),
            CandidateSelection {
                tracked_paths: Some(vec![path.into()]),
                untracked_paths: Vec::new(),
            },
        )
    };
    let publish = |selection: String| {
        store.publish_workflow_candidate(
            project.clone(),
            workflow.clone(),
            "implement".into(),
            task.clone(),
            selection,
        )
    };
    let candidate = select("first.txt").await?;
    assert!(publish(candidate.id.clone()).await.is_err());
    assert!(
        store
            .record_goal_criterion_evidence(
                project.clone(),
                attempt.run.id.clone(),
                1,
                GoalCriterionEvidence::OperatorReview {
                    approved: true,
                    note: None
                }
            )
            .await
            .is_err()
    );
    let verification = store
        .verify_workflow_goal_candidate(
            project.clone(),
            workflow.clone(),
            "implement".into(),
            task.clone(),
            candidate.id.clone(),
        )
        .await?;
    assert_eq!(verification.candidate_id, candidate.id);
    let [gate] = verification.gates.as_slice() else {
        anyhow::bail!("expected exactly one check");
    };
    assert!(gate.gate.required);
    assert!(matches!(
        gate.outcome,
        Some(CandidateGateOutcome::Passed { .. })
    ));
    let evidence = store
        .record_goal_criterion_evidence(
            project.clone(),
            attempt.run.id.clone(),
            1,
            GoalCriterionEvidence::CandidateGate {
                verification_id: verification.id,
                gate_index: 0,
            },
        )
        .await?;
    assert_eq!(evidence.status, GoalVerificationStatus::Completed);
    let published = publish(candidate.id.clone()).await?;
    assert!(
        matches!(published.value, ArtifactValue::Candidate(selected) if selected.id == candidate.id)
    );
    let other = select("second.txt").await?;
    assert!(publish(other.id).await.is_err());
    Ok(())
}
