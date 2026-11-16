use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

use crate::{
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, JobPermission, JobRole, JobRule, JobWorkspace,
        RecoveryAction, ReleaseBlocker,
    },
    mcp_bridge::{RunBridge, invoke_test_mcp_tool},
    providers::SessionMode,
    repository,
    runs::RunStatus,
    store::Store,
    workflows::{
        ArtifactKind, ArtifactValue, DeliveryRoles, NodeInputs, WorkflowArtifact,
        WorkflowHostRequest, WorkflowHostSubmission, builtin_delivery_graph,
        tests::{approved_delivery_repository, link_reviewed_delivery_fixture, submission},
    },
};

use super::*;

#[tokio::test]
async fn automatic_approval_rechecks_typed_scope_rule_and_plan_for_release_recovery_and_children()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("workflow-rule.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id.clone();
    let version = store
        .save_workflow_revision(
            project.clone(),
            "scoped-delivery".into(),
            builtin_delivery_graph(DeliveryRoles::default()),
        )
        .await?;
    let run = store
        .start_workflow_run(project.clone(), version.clone())
        .await?;
    let (task, plan, alternate_plan) =
        link_reviewed_delivery_fixture(&store, &repo, None, &run).await?;
    let ArtifactValue::Task(task_ref) = &task.value else {
        anyhow::bail!("workflow did not link an actionable task");
    };
    let context = WorkflowApprovalSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: "approve".into(),
        inputs: NodeInputs::from([
            ("plan".into(), plan.id.clone()),
            ("task".into(), task.id.clone()),
        ]),
    };
    let initial = store.workflow_approval_preview(context.clone()).await?;
    assert_eq!(initial.outcome, ApprovalRuleOutcome::HumanRequired);
    assert_eq!(initial.settings, ApprovalRuleSettings::Human);
    assert!(initial.allows_human);
    assert_eq!(initial.facts.unit_count, 2);
    assert_eq!(initial.facts.plan_revision, plan.id);
    let settings = |maximum_units, if_unmet| ApprovalRuleSettings::Automatic {
        task_id: task_ref.task_id.clone(),
        source_checkout_id: repo.checkout_id.clone(),
        maximum_units,
        if_unmet,
    };
    let waiting = store
        .set_workflow_approval_rule(context.clone(), None, settings(1, UnmetApproval::Wait))
        .await?;
    assert_eq!(waiting.outcome, ApprovalRuleOutcome::UnitLimitExceeded);
    assert!(!waiting.allows_human);
    assert!(waiting.decision_artifact_id.is_none());
    assert!(approve(&store, &context).await.is_err());
    assert!(
        store
            .set_workflow_approval_rule(context.clone(), None, settings(2, UnmetApproval::Wait))
            .await
            .is_err()
    );
    let mut foreign = context.clone();
    foreign.project_id = "different-project".into();
    assert!(store.workflow_approval_preview(foreign).await.is_err());
    assert!(
        store
            .set_workflow_approval_rule(
                context.clone(),
                waiting.revision.clone(),
                ApprovalRuleSettings::Automatic {
                    task_id: task_ref.task_id.clone(),
                    source_checkout_id: "different-checkout".into(),
                    maximum_units: 2,
                    if_unmet: UnmetApproval::Wait,
                }
            )
            .await
            .is_err()
    );

    let allowed = store
        .set_workflow_approval_rule(
            context.clone(),
            waiting.revision,
            settings(2, UnmetApproval::Wait),
        )
        .await?;
    assert_eq!(allowed.outcome, ApprovalRuleOutcome::ConditionsMet);
    let decision = allowed
        .decision_artifact_id
        .context("automatic approval was not recorded")?;
    let pinned_rule = allowed
        .revision
        .context("automatic approval had no rule revision")?;
    let approval = store
        .resolve_workflow_artifact(
            project.clone(),
            run.id.clone(),
            decision.clone(),
            ArtifactKind::Approval,
        )
        .await?
        .context("saved automatic approval not found")?;
    let ArtifactValue::Approval(approval) = approval.value else {
        anyhow::bail!("approval artifact has wrong type");
    };
    assert_eq!(approval.source, ApprovalDecisionSource::AutomaticRule);
    assert_eq!(
        approval.rule_evidence,
        Some(ApprovalRuleEvidence {
            revision: pinned_rule.clone(),
            facts: initial.facts
        })
    );
    assert_eq!(
        store
            .evaluate_workflow_approval(context.clone(), pinned_rule.clone())
            .await?
            .decision_artifact_id
            .as_ref(),
        Some(&decision)
    );

    let (worktree, release_artifact) =
        prepare(&store, &context, &task, &decision, temp.path()).await?;
    let ArtifactValue::Release(reference) = &release_artifact.value else {
        anyhow::bail!("expected an exact writer release");
    };
    assert_eq!(reference.decision_artifact_id, decision);
    assert_eq!(reference.release.plan_revision, plan.id);
    let ArtifactValue::Worktree { checkout_id, .. } = &worktree.value else {
        anyhow::bail!("expected a writer checkout");
    };
    assert_eq!(&reference.release.checkout_id, checkout_id);
    let units = store
        .workflow_delivery_units(project.clone(), task_ref.task_id.clone(), run.id.clone())
        .await?;
    let mut writer = submission(
        &repo,
        &run,
        "implement",
        Some(plan.id.clone()),
        SessionMode::ApprovedWrite,
    );
    writer.dispatch.release_id = Some(reference.release.id.clone());
    writer.request.checkout_id = Some(checkout_id.clone());
    writer.request.task_id = Some(task_ref.task_id.clone());
    writer.scope.checkout = repository::checkout_root(&store, checkout_id.clone()).await?;
    writer.unit_id = Some(units[0].unit.id.clone());
    writer.artifacts = NodeInputs::from([
        ("plan".into(), plan.id.clone()),
        ("worktree".into(), worktree.id),
        ("release".into(), release_artifact.id.clone()),
    ]);
    let writer = store.submit_workflow_agent(writer).await?;
    store
        .set_project_job_rules(
            project.clone(),
            Vec::new(),
            vec![JobRule {
                role: JobRole::Agent,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    let env = AdmissionEnvironment {
        runner: "fixture-runner".into(),
        bridge_available: true,
    };
    let parent = store
        .claim_next_job(env.clone())
        .await?
        .context("automatic decision did not authorize the exact workflow writer")?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    assert!(repository::capture_worktree_start(&store, parent.run.id.clone()).await?);
    let bridge = RunBridge::start(&store, parent.run.id.clone()).await?;
    let child_id = submit_child(&bridge, "workflow-child").await?;
    let child = store
        .job(project.clone(), child_id)
        .await?
        .context("MCP child was not saved")?;
    assert_eq!(
        child
            .pipeline
            .as_ref()
            .and_then(|origin| origin.release.as_ref()),
        Some(&reference.release)
    );
    let before_rule_change = store
        .inspect_job(project.clone(), child.id.clone(), env.clone())
        .await?
        .context("child missing")?;
    assert!(
        !before_rule_change
            .blockers
            .iter()
            .any(|blocker| matches!(blocker, AdmissionBlocker::PipelineRelease { .. }))
    );

    let fallback = store
        .set_workflow_approval_rule(
            context.clone(),
            Some(pinned_rule.clone()),
            settings(1, UnmetApproval::Human),
        )
        .await?;
    assert!(fallback.allows_human);
    assert_eq!(fallback.outcome, ApprovalRuleOutcome::UnitLimitExceeded);
    assert!(fallback.decision_artifact_id.is_none());
    assert!(
        store
            .evaluate_workflow_approval(context.clone(), pinned_rule)
            .await
            .is_err()
    );
    let child_state = store
        .inspect_job(project.clone(), child.id.clone(), env.clone())
        .await?
        .context("child provenance was lost")?;
    assert!(
        child_state
            .blockers
            .contains(&AdmissionBlocker::PipelineRelease {
                reason: ReleaseBlocker::Revoked
            })
    );
    let bypass = submit_child(&bridge, "bypass-stale-approval").await?;
    let bypass = store
        .inspect_job(project.clone(), bypass, env.clone())
        .await?
        .context("MCP bypass was not tracked")?;
    assert!(
        bypass
            .blockers
            .contains(&AdmissionBlocker::PipelineRelease {
                reason: ReleaseBlocker::Revoked
            })
    );
    assert!(
        prepare(&store, &context, &task, &decision, temp.path())
            .await
            .is_err()
    );
    repository::save_worktree_candidate(&store, parent.run.id.clone()).await?;
    store
        .finish_agent_attempt(parent.run.id.clone(), RunStatus::Failed, None)
        .await?;
    for action in [RecoveryAction::Retry, RecoveryAction::Resume] {
        assert!(
            store
                .queue_job_recovery(
                    project.clone(),
                    writer.id.clone(),
                    parent.run.id.clone(),
                    action
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("accepted release for the current plan and worktree")
        );
    }
    drop(bridge);

    let manual = approve(&store, &context).await?;
    assert_ne!(manual.id, decision);
    let ArtifactValue::Approval(human) = &manual.value else {
        anyhow::bail!("expected human approval");
    };
    assert_eq!(human.source, ApprovalDecisionSource::Operator);
    assert_eq!(
        human
            .rule_evidence
            .as_ref()
            .map(|evidence| &evidence.revision),
        fallback.revision.as_ref()
    );
    let (_new_worktree, replacement) =
        prepare(&store, &context, &task, &manual.id, temp.path()).await?;
    assert_ne!(replacement.id, release_artifact.id);
    assert_eq!(
        store
            .workflow_delivery_units(project.clone(), task_ref.task_id.clone(), run.id.clone())
            .await?,
        units
    );
    drop(store);

    let reopened = Store::open(database)?;
    let history = reopened
        .workflow_run_artifacts(project.clone(), run.id.clone())
        .await?;
    assert!(history.iter().any(|artifact| artifact.id == decision
        && artifact.value == ArtifactValue::Approval(approval.clone())));
    assert!(history.iter().any(|artifact| artifact == &release_artifact));
    assert_eq!(
        reopened
            .workflow_approval_preview(context.clone())
            .await?
            .decision_artifact_id,
        Some(manual.id)
    );
    let resumed = reopened
        .inspect_job(project.clone(), child.id, env)
        .await?
        .context("child missing after restart")?;
    assert!(
        resumed
            .blockers
            .contains(&AdmissionBlocker::PipelineRelease {
                reason: ReleaseBlocker::Revoked
            })
    );

    reopened
        .set_workflow_approval_rule(
            context.clone(),
            fallback.revision,
            settings(2, UnmetApproval::Wait),
        )
        .await?;
    reopened
        .set_job_pipeline_plan(
            project.clone(),
            run.id.clone(),
            Some(plan.id),
            alternate_plan.id,
        )
        .await?;
    assert!(reopened.workflow_approval_preview(context).await.is_err());
    let next = reopened
        .start_workflow_run(project.clone(), version.clone())
        .await?;
    let (new_task, new_plan, _) =
        link_reviewed_delivery_fixture(&reopened, &repo, Some(task_ref.task_id.clone()), &next)
            .await?;
    let next_approval = reopened
        .workflow_approval_preview(WorkflowApprovalSubmission {
            project_id: project.clone(),
            run_id: next.id.clone(),
            node_id: "approve".into(),
            inputs: NodeInputs::from([
                ("plan".into(), new_plan.id.clone()),
                ("task".into(), new_task.id),
            ]),
        })
        .await?;
    assert_eq!(next_approval.outcome, ApprovalRuleOutcome::ConditionsMet);
    let outputs = reopened
        .current_workflow_host_outputs(project.clone(), next.id)
        .await?;
    assert_eq!(
        outputs.get("approve"),
        next_approval.decision_artifact_id.as_ref()
    );
    assert!(outputs.contains_key("approve"));
    assert_eq!(next_approval.facts.plan_revision, new_plan.id);

    let different_scope = reopened
        .start_workflow_run(project.clone(), version)
        .await?;
    let (other_task, other_plan, _) =
        link_reviewed_delivery_fixture(&reopened, &repo, None, &different_scope).await?;
    let blocked = WorkflowApprovalSubmission {
        project_id: project,
        run_id: different_scope.id,
        node_id: "approve".into(),
        inputs: NodeInputs::from([
            ("plan".into(), other_plan.id),
            ("task".into(), other_task.id),
        ]),
    };
    let mismatch = reopened.workflow_approval_preview(blocked.clone()).await?;
    assert_eq!(mismatch.outcome, ApprovalRuleOutcome::ScopeDoesNotMatch);
    assert!(!mismatch.allows_human);
    assert!(mismatch.decision_artifact_id.is_none());
    assert!(approve(&reopened, &blocked).await.is_err());
    Ok(())
}

async fn submit_child(bridge: &RunBridge, request_id: &str) -> Result<String> {
    let reply = invoke_test_mcp_tool(bridge, "submit_job", json!({
        "requestId": request_id,
        "request": { "mode": "approved_write", "instructions": "Delegate the scoped code change" },
    })).await?;
    reply["result"]["structuredContent"]["jobId"]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("MCP child request was rejected {reply}"))
}

async fn approve(store: &Store, context: &WorkflowApprovalSubmission) -> Result<WorkflowArtifact> {
    store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: context.project_id.clone(),
            run_id: context.run_id.clone(),
            node_id: context.node_id.clone(),
            inputs: context.inputs.clone(),
            request: WorkflowHostRequest::ApproveImplementation,
        })
        .await
}

async fn prepare(
    store: &Store,
    context: &WorkflowApprovalSubmission,
    task: &WorkflowArtifact,
    decision: &str,
    temp: &Path,
) -> Result<(WorkflowArtifact, WorkflowArtifact)> {
    let worktree = store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: context.project_id.clone(),
            run_id: context.run_id.clone(),
            node_id: "prepare".into(),
            inputs: NodeInputs::from([
                ("task".into(), task.id.clone()),
                ("decision".into(), decision.into()),
            ]),
            request: WorkflowHostRequest::PrepareWorktree {
                default_root: PathBuf::from(temp).join("managed"),
            },
        })
        .await?;
    let release = store
        .workflow_release_artifact(
            context.project_id.clone(),
            context.run_id.clone(),
            decision.into(),
        )
        .await?
        .context("prepared worktree release missing")?;
    Ok((worktree, release))
}
