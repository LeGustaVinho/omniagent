use crate::{
    providers::Provider,
    workflows::{
        BuiltinNode, ConditionPredicate, Endpoint, NodeConfiguration, NodeInstance, Wire,
        WorkflowGraph,
    },
};

use super::*;

#[tokio::test]
async fn late_selected_evidence_automatically_approves_only_its_branch_and_releases_its_writer()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("branch-approval.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id.clone();
    let workflow = "approval-branches";
    let original = store
        .save_workflow_revision(project.clone(), workflow.into(), graph(false))
        .await?;
    let initial = store.start_workflow_run(project.clone(), original).await?;
    let (linked, plan, _) = link_reviewed_delivery_fixture(&store, &repo, None, &initial).await?;
    let ArtifactValue::Task(task) = &linked.value else {
        anyhow::bail!("The fixture did not link a Task");
    };
    let rule = ApprovalRuleSettings::Automatic {
        task_id: task.task_id.clone(),
        source_checkout_id: repo.checkout_id.clone(),
        maximum_units: 1,
        if_unmet: UnmetApproval::Wait,
    };
    let mut rules = std::collections::BTreeMap::new();
    for node in ["approve", "alternate-approval"] {
        let preview = store
            .set_workflow_approval_rule(
                approval(&project, &initial.id, node, &plan, &linked),
                None,
                rule.clone(),
            )
            .await?;
        assert_eq!(preview.outcome, ApprovalRuleOutcome::UnitLimitExceeded);
        assert!(preview.decision_artifact_id.is_none());
        rules.insert(node, preview.revision.context("The rule did not save")?);
    }

    let version = store
        .save_workflow_revision(project.clone(), workflow.into(), graph(true))
        .await?;
    let run = store.start_workflow_run(project.clone(), version).await?;
    let (linked, plan, _) =
        link_reviewed_delivery_fixture(&store, &repo, Some(task.task_id.clone()), &run).await?;
    let selected = approval(&project, &run.id, "approve", &plan, &linked);
    let alternate = approval(&project, &run.id, "alternate-approval", &plan, &linked);
    for context in [selected.clone(), alternate.clone()] {
        assert!(matches!(
            store
                .workflow_approval_stage(project.clone(), run.id.clone(), context.node_id.clone())
                .await?,
            WorkflowApprovalStage::Waiting { .. }
        ));
        let outcome = store
            .workflow_approval_preview(context.clone())
            .await
            .unwrap_err();
        assert!(
            outcome
                .to_string()
                .contains("ready is waiting for its source"),
            "{outcome:#}"
        );
        assert!(
            store
                .workflow_approval_decision(
                    project.clone(),
                    run.id.clone(),
                    context.node_id,
                    plan.id.clone()
                )
                .await?
                .is_none()
        );
    }

    let environment = AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: false,
    };
    let mut reviewer = submission(
        &repo,
        &run,
        "late-review",
        Some(plan.id.clone()),
        SessionMode::ReadOnly,
    );
    reviewer.artifacts = NodeInputs::from([("plan".into(), plan.id.clone())]);
    reviewer.scope.provider = Provider::Claude;
    reviewer.scope.profile_id = "separate-branch-reviewer".into();
    store.submit_workflow_agent(reviewer).await?;
    let evidence = store
        .claim_next_job(environment.clone())
        .await?
        .context("Late review was not admitted")?;
    store.mark_agent_running(evidence.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            evidence.run.id.clone(),
            RunStatus::Succeeded,
            Some("Reviewed the scoped plan".into()),
        )
        .await?;
    store
        .publish_workflow_artifact(
            project.clone(),
            run.id.clone(),
            "late-review".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: evidence.run.id,
            },
        )
        .await?;

    // Reading the decision directly, without refreshing controls or running a human evaluation,
    // proves that publishing the late evidence advanced the approval in the scheduler.
    let receipt = store
        .workflow_approval_decision(
            project.clone(),
            run.id.clone(),
            "approve".into(),
            plan.id.clone(),
        )
        .await?
        .context("The selected approval did not advance")?;
    assert_eq!(receipt.source, ApprovalDecisionSource::AutomaticRule);
    let rule_evidence = receipt
        .rule_evidence
        .context("Missing automatic rule evidence")?;
    assert_eq!(rule_evidence.revision, rules["approve"]);
    assert_eq!(rule_evidence.facts.plan_revision, plan.id);
    assert_eq!(rule_evidence.facts.task_artifact_id, linked.id);
    assert_eq!(rule_evidence.facts.source_checkout_id, repo.checkout_id);
    assert!(matches!(
        store.workflow_approval_stage(project.clone(), run.id.clone(), "approve".into()).await?,
        WorkflowApprovalStage::Ready { plan_id, task_artifact_id, source: Some(ApprovalDecisionSource::AutomaticRule), .. }
        if plan_id == plan.id && task_artifact_id == linked.id
    ));
    assert!(matches!(
        store
            .workflow_approval_stage(project.clone(), run.id.clone(), "alternate-approval".into())
            .await?,
        WorkflowApprovalStage::Skipped { .. }
    ));
    assert!(
        store
            .workflow_approval_decision(
                project.clone(),
                run.id.clone(),
                "alternate-approval".into(),
                plan.id.clone()
            )
            .await?
            .is_none()
    );
    let rejected = approve(&store, &alternate).await.unwrap_err();
    assert!(rejected.to_string().contains("skipped"), "{rejected:#}");

    let preview = store.workflow_approval_preview(selected.clone()).await?;
    let decision = preview
        .decision_artifact_id
        .context("Missing active approval")?;
    let (worktree, release_artifact) =
        prepare(&store, &selected, &linked, &decision, directory.path()).await?;
    let ArtifactValue::Release(reference) = &release_artifact.value else {
        anyhow::bail!("Missing prepared release")
    };
    let release = reference.release.clone();
    let unit = store
        .workflow_delivery_units(project.clone(), task.task_id.clone(), run.id.clone())
        .await?
        .pop()
        .context("The release did not bind a logical unit")?;
    let mut writer = submission(
        &repo,
        &run,
        "implement",
        Some(plan.id.clone()),
        SessionMode::ApprovedWrite,
    );
    writer.dispatch.release_id = Some(release.id.clone());
    writer.scope.checkout = repository::checkout_root(&store, release.checkout_id.clone()).await?;
    writer.request.checkout_id = Some(release.checkout_id.clone());
    writer.request.task_id = Some(task.task_id.clone());
    writer.artifacts = NodeInputs::from([
        ("plan".into(), plan.id.clone()),
        ("release".into(), release_artifact.id.clone()),
        ("worktree".into(), worktree.id),
    ]);
    writer.unit_id = Some(unit.unit.id);
    store
        .set_project_job_rules(
            project.clone(),
            Vec::new(),
            vec![JobRule {
                role: JobRole::Pipeline,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    let queued = store.submit_workflow_agent(writer).await?;
    assert!(
        !store
            .inspect_job(project.clone(), queued.id.clone(), environment.clone())
            .await?
            .context("The branch writer did not queue")?
            .blockers
            .iter()
            .any(|blocker| matches!(blocker, AdmissionBlocker::PipelineRelease { .. }))
    );

    let changed = store
        .set_workflow_approval_rule(
            selected.clone(),
            Some(rules["approve"].clone()),
            ApprovalRuleSettings::Human,
        )
        .await?;
    assert!(changed.allows_human);
    assert!(changed.decision_artifact_id.is_none());
    assert!(
        prepare(&store, &selected, &linked, &decision, directory.path())
            .await
            .is_err()
    );
    drop(store);

    let store = Store::open(database)?;
    let writer = store
        .inspect_job(project.clone(), queued.id, environment)
        .await?
        .context("The branch writer did not survive restart")?;
    assert!(
        writer
            .blockers
            .contains(&AdmissionBlocker::PipelineRelease {
                reason: ReleaseBlocker::Revoked
            })
    );
    let graph = store
        .inspect_workflow_execution(project, run.id.clone())
        .await?
        .context("Missing saved graph")?;
    assert!(graph.control.iter().any(|entry| entry.node_id == "selected-approval" && entry.selected_ports == ["joined"]));
    assert!(
        graph
            .control
            .iter()
            .any(|entry| entry.node_id == "alternate-approval"
                && entry.state == crate::workflows::control::ControlStageState::Skipped)
    );
    Ok(())
}

fn approval(
    project: &str,
    run: &str,
    node: &str,
    plan: &WorkflowArtifact,
    task: &WorkflowArtifact,
) -> WorkflowApprovalSubmission {
    WorkflowApprovalSubmission {
        project_id: project.into(),
        run_id: run.into(),
        node_id: node.into(),
        inputs: NodeInputs::from([
            ("plan".into(), plan.id.clone()),
            ("task".into(), task.id.clone()),
        ]),
    }
}

fn graph(late: bool) -> WorkflowGraph {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let wire = |source: &str, from: &str, target: &str, to: &str| Wire {
        from: Endpoint {
            node: source.into(),
            port: from.into(),
        },
        to: Endpoint {
            node: target.into(),
            port: to.into(),
        },
    };
    graph.nodes.push(NodeInstance {
        id: "alternate-approval".into(),
        kind: BuiltinNode::ApproveImplementation,
        configuration: NodeConfiguration::None,
    });
    graph.wires.extend([
        wire("work", "task", "alternate-approval", "task"),
        wire("plan", "plan", "alternate-approval", "plan"),
    ]);
    if !late {
        graph
            .wires
            .push(wire("work", "linked", "alternate-approval", "ready"));
        return graph;
    }
    graph
        .wires
        .retain(|edge| !(edge.to.node == "approve" && edge.to.port == "ready"));
    let mut reviewer = graph
        .nodes
        .iter()
        .find(|node| node.id == "review-plan")
        .unwrap()
        .clone();
    reviewer.id = "late-review".into();
    graph.nodes.extend([
        reviewer,
        NodeInstance {
            id: "route".into(),
            kind: BuiltinNode::Condition,
            configuration: NodeConfiguration::Condition {
                predicate: ConditionPredicate::Exists {
                    pointer: "/evidence/run_id".into(),
                },
            },
        },
        NodeInstance {
            id: "selected-approval".into(),
            kind: BuiltinNode::Join,
            configuration: NodeConfiguration::None,
        },
    ]);
    graph.wires.extend([
        wire("plan", "planned", "late-review", "ready"),
        wire("plan", "plan", "late-review", "plan"),
        wire("work", "linked", "route", "ready"),
        wire("late-review", "review", "route", "value"),
        wire("route", "matched", "selected-approval", "branches"),
        wire("selected-approval", "joined", "approve", "ready"),
        wire("route", "unmatched", "alternate-approval", "ready"),
    ]);
    graph
}
