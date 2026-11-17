use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{
    conversations::AttemptPlan,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::AdmissionEnvironment,
    providers::{
        ProviderEvent, ProviderEventData, ProviderLocation, TurnOutcome,
        capabilities::{CapabilityAssessment, CapabilityScope, ReportedCapabilities},
        configuration,
    },
    repository::{self, ProjectCheckout},
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
    workflows::{
        WorkflowRevision, WorkflowRun,
        authoring::package::WorkflowPackagePublishOutcome,
        automation::PendingWorkflowAgent,
        templates::{WorkflowFactoryTemplate, workflow_factory_templates},
    },
};

use super::{
    approved_delivery_repository,
    backlog_saga::{DESTINATION, github_fixture::LocalGithub},
    git, seal_review,
};

mod delivery;
mod planning;

const REQUEST: &str = "Deliver three independently testable export changes. Preserve the approved behavior through integration and release audit.";

pub(super) struct FixtureScript {
    adaptive: bool,
}

impl FixtureScript {
    async fn claim(
        &self,
        store: &Store,
        repo: &ProjectCheckout,
        pending: &PendingWorkflowAgent,
    ) -> Result<Option<AttemptPlan>> {
        let (workflow, node) = pending.fixture_identity();
        let mut submission = pending.fixture_submission(store).await?;
        if self.adaptive && node.ends_with("-agent::execute") {
            submission.scope.runner = DirectWindowsRunner::IDENTITY.into();
            submission.scope.account_binding = Some("audit-fixture-account".into());
        }
        let runner = submission.scope.runner.clone();
        pending
            .enqueue(store, submission)
            .await
            .with_context(|| format!("Queue the published Audit agent {node}"))?;
        let Some(attempt) = store
            .claim_next_job(AdmissionEnvironment {
                runner,
                bridge_available: false,
            })
            .await?
        else {
            ensure!(
                self.adaptive && node.ends_with("-agent::execute"),
                "Admit the published Audit agent {node}"
            );
            return Ok(None);
        };
        let observed = store
            .workflow_attempt(repo.project_id.clone(), attempt.run.id.clone())
            .await?
            .context("The admitted Audit agent lost its workflow binding")?;
        assert_eq!(observed.pin.workflow_run_id, workflow);
        assert_eq!(observed.pin.node_instance_id, node);
        Ok(Some(attempt))
    }

    async fn complete_response(
        &self,
        store: &Store,
        repo: &ProjectCheckout,
        attempt: &AttemptPlan,
        answer: &Value,
    ) -> Result<()> {
        let run = &attempt.run.id;
        if let Some(candidate) = store
            .workflow_candidate_for_review_attempt(run.clone())
            .await?
        {
            seal_review(store, &candidate, attempt).await?;
        } else {
            store.mark_agent_running(run.clone()).await?;
            let snapshot: ContextSnapshot = if self.adaptive
                && attempt.scope.account_binding.is_some()
            {
                let checkout = repo.checkout_id.clone();
                let layout = repository::verify_checkout(
                    store,
                    checkout.clone(),
                    attempt.scope.checkout.clone(),
                )
                .await?;
                let (config, skills, instructions) = configuration::inspect(
                    &attempt.scope,
                    &layout,
                    &ReportedCapabilities::default(),
                )?;
                let capabilities = CapabilityAssessment {
                    version: 1,
                    entries: vec![],
                    scope: CapabilityScope::from_dispatch(
                        &attempt.scope,
                        checkout,
                        DirectWindowsRunner.executable_revision(&attempt.scope.executable),
                        DirectWindowsRunner.execution_platform(),
                        config,
                        attempt.scope.model.clone(),
                        None,
                    ),
                };
                serde_json::from_value(json!({
                    "version":13, "contextId":attempt.context_id, "scope":attempt.scope,
                    "baseCommit":repository::base_commit(&layout.cwd)?, "submittedMessages":attempt.input,
                    "nativeHistory":{"mode":"fresh"}, "capabilities":capabilities,
                    "instructions":instructions,"filesystemSkills":skills,"questionRoute":attempt.question_route,
                }))?
            } else {
                serde_json::from_value(json!({
                    "version": 1, "contextId": attempt.context_id, "scope": attempt.scope,
                    "submittedMessages": attempt.input, "nativeHistory": {"mode":"fresh"},
                    "questionRoute": attempt.question_route,
                }))?
            };
            let dispatch = store
                .prepare_dispatch(
                    run.clone(),
                    snapshot,
                    vec![b"audit factory fixture".to_vec()],
                )
                .await?;
            store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        }
        store
            .record_agent_observation(
                run.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some(format!("audit-fixture-session-{run}")),
                        turn_id: Some(format!("audit-fixture-turn-{run}")),
                        ..Default::default()
                    },
                    data: ProviderEventData::TurnFinished {
                        outcome: TurnOutcome::Completed,
                    },
                },
            )
            .await?;
        let response = answer
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| answer.to_string());
        store
            .finish_observed_agent_attempt(
                run.clone(),
                RunStatus::Succeeded,
                Some(response),
                None,
                true,
            )
            .await?;
        Ok(())
    }
}

fn prompt_response(answer: Value) -> Value {
    json!({
        "route":"concluido",
        "reason":"Use the fixture's pinned evidence.",
        "outputs":{"object":answer},
        "delegations": [],
    })
}

fn authored_stage(node: &str) -> &str {
    node.strip_suffix("::execute")
        .unwrap_or(node)
        .rsplit("::")
        .next()
        .unwrap_or(node)
}

async fn launch_audit(
    store: &Store,
    repo: &ProjectCheckout,
    template_id: WorkflowFactoryTemplate,
) -> Result<WorkflowRun> {
    let template = workflow_factory_templates()
        .into_iter()
        .find(|entry| entry.id == template_id)
        .context("The Audit factory is unavailable")?;
    let draft = store
        .create_workflow_factory_draft(repo.project_id.clone(), template.id, template.revision)
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(repo.project_id.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("The complete Audit factory did not publish");
    };
    let member = publication
        .members
        .iter()
        .find(|member| member.key == "planning")
        .context("The Audit root was not published")?;
    let workflow = WorkflowRevision {
        id: member.artifact.version.id.clone(),
        revision: member.artifact.version.revision.clone(),
    };
    let run = store
        .launch_workflow_request(serde_json::from_value(json!({
            "projectId":repo.project_id, "workflow":workflow, "checkoutId":repo.checkout_id,
            "requestId":"audit-factory-native", "inputs":{"prompt":REQUEST}, "automatic":true,
        }))?)
        .await?;
    ensure!(
        run.version == workflow,
        "The launch changed the published Audit version"
    );
    let launch = store
        .workflow_launch(repo.project_id.clone(), run.id.clone())
        .await?
        .context("The Audit launch lost its stored input receipt")?;
    assert_eq!(launch.inputs["prompt"], REQUEST);
    assert!(launch.automatic);
    Ok(run)
}

#[tokio::test]
async fn published_audit_factory_executes_native_planning_delivery_and_release() -> Result<()> {
    run_audit_factory(WorkflowFactoryTemplate::SuperAiDevelopmentAudit).await
}

#[tokio::test]
async fn published_adaptive_audit_runs_selected_reviews_and_verifies_the_final_release()
-> Result<()> {
    run_audit_factory(WorkflowFactoryTemplate::SuperAiDevelopmentAuditAdaptive).await
}

async fn run_audit_factory(template: WorkflowFactoryTemplate) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("native-published-audit.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let checkout = Path::new(&repo.checkout_root);
    git(checkout, &["branch", DESTINATION])?;
    let initial = git(checkout, &["rev-parse", "HEAD"])?;
    let github = LocalGithub::new(checkout, initial);
    let run = launch_audit(&store, &repo, template).await?;
    let script = FixtureScript {
        adaptive: template == WorkflowFactoryTemplate::SuperAiDevelopmentAuditAdaptive,
    };
    planning::plan_and_materialize(&store, &repo, &run, &script, &github).await?;
    drop(store);
    let mut store = Store::open(database.clone())?;
    delivery::run_delivery_and_audit(
        &mut store,
        &database,
        temporary.path(),
        &repo,
        &run,
        &script,
        &github,
    )
    .await?;
    Ok(())
}
