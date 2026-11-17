use serde_json::json;

use crate::{
    dispatch::{Delivery, EventSource},
    jobs::{AdmissionEnvironment, JobRequest, PipelineDispatch},
    providers::{
        Provider, ProviderLocation, TurnOutcome,
        capabilities::{CapabilityAssessment, CapabilityScope},
    },
    repository::ProjectCheckout,
    workflows::{
        WorkflowRevision,
        authoring::package::{WorkflowPackage, WorkflowPackagePublishOutcome},
        tests::approved_delivery_repository,
    },
};

use super::*;

fn package() -> Result<WorkflowPackage> {
    let pin = |node, port| json!({"node":node, "port":port});
    let wire = |source, output, target, input| {
        json!({
            "from":pin(source, output), "to":pin(target, input)
        })
    };
    let node =
        |id, kind, configuration| json!({"id":id, "kind":kind, "configuration":configuration});
    let body = json!({
        "key":"body", "name":"Review unchanged subject",
        "body":{
            "kind":"subflow",
            "graph":{"name":"Reviewer", "nodes":[
                node("start","start",json!({"kind":"subflow_entry","outputs":[{"name":"ready","value":{"kind":"control"}}]})),
                node("material","build_text",json!({"kind":"build_text","template":"Review this stable artifact."})),
                node("evidence","collect_evidence",json!({"kind":"none"})),
                node("review","agent",json!({
                    "kind":"agent_role","role":"Independent reviewer",
                    "model":{"provider":"codex","profileId":"fixture-profile","modelId":"fixture-model"},
                    "requirements":{"reviewReuse":true}
                })),
                node("validate","validate_output",json!({
                    "kind":"validate_output","schema":{"type":"object","additionalProperties":false,"required":["approved"],"properties":{"approved":{"type":"boolean"}}}
                }))
            ],"wires":[
                wire("start","ready","material","ready"),
                wire("material","completed","evidence","ready"),
                wire("material","text","evidence","evidence"),
                wire("evidence","completed","review","ready"),
                wire("evidence","text","review","prompt"),
                wire("evidence","evidence","review","review_subject"),
                wire("review","completed","validate","ready"),
                wire("review","response","validate","source")
            ]},
            "inputs":{"ready":pin("start","ready")},
            "outputs":{"done":pin("validate","completed"),"result":pin("validate","object")}
        }
    });
    let root = json!({
        "key":"root","name":"Repeat review", "references":{"repeat":{"source":"draft","key":"body"}},
        "body":{"kind":"pipeline","graph":{"name":"Repeat review","nodes":[
            node("start","typed_start",json!({"kind":"typed_start","fields":[]})),
            node("repeat","repeat_until",json!({
                "kind":"repeat_until","maxIterations":2,"readyInput":"ready",
                "completionOutput":"done","evidenceOutput":"result",
                "until":{"op":"equals","pointer":"/evidence/forceAnotherRound","value":false}
            }))
        ],"wires":[wire("start","start","repeat","ready")]}}
    });
    Ok(serde_json::from_value(
        json!({"name":"Repeat read-only reviews","members":[body,root]}),
    )?)
}

async fn pinned_inputs(store: &Store, run: &WorkflowRun, ordinal: u8) -> Result<NodeInputs> {
    let prefix = format!("repeat::iteration-{ordinal}::evidence");
    let view = store
        .inspect_workflow_execution(run.project_id.clone(), run.id.clone())
        .await?
        .context("Review workflow is unavailable")?;
    Ok(view
        .outputs
        .into_iter()
        .filter(|output| output.artifact.node_instance_id == prefix)
        .map(|output| {
            let artifact = output.artifact;
            let name = if artifact.port == "text" {
                "prompt"
            } else {
                "review_subject"
            };
            (name.into(), artifact.id)
        })
        .collect())
}

fn admission(
    repo: &ProjectCheckout,
    run: &WorkflowRun,
    scope: SessionScope,
    ordinal: u8,
    artifacts: NodeInputs,
) -> WorkflowAgentSubmission {
    WorkflowAgentSubmission {
        project_id: repo.project_id.clone(),
        dispatch: PipelineDispatch {
            pipeline_run_id: run.id.clone(),
            node_instance_id: format!("repeat::iteration-{ordinal}::review"),
            plan_revision: None,
            release_id: None,
        },
        scope,
        request: JobRequest {
            checkout_id: Some(repo.checkout_id.clone()),
            task_id: None,
            mode: SessionMode::ReadOnly,
            instructions: "Fixture instructions are replaced by the pinned prompt".into(),
            dependencies: vec![],
            requires_bridge: false,
        },
        input: Default::default(),
        artifacts,
        unit_id: None,
    }
}

async fn complete(
    store: &Store,
    repo: &ProjectCheckout,
    attempt: &crate::conversations::AttemptPlan,
    approved: bool,
) -> Result<()> {
    let source_run = &attempt.run.id;
    store.mark_agent_running(source_run.clone()).await?;
    let layout = repository::verify_checkout(
        store,
        repo.checkout_id.clone(),
        attempt.scope.checkout.clone(),
    )
    .await?;
    let (config, skills, instructions) =
        configuration::inspect(&attempt.scope, &layout, &ReportedCapabilities::default())?;
    let capabilities = CapabilityAssessment {
        version: 1,
        entries: vec![],
        scope: CapabilityScope::from_dispatch(
            &attempt.scope,
            repo.checkout_id.clone(),
            DirectWindowsRunner.executable_revision(&attempt.scope.executable),
            DirectWindowsRunner.execution_platform(),
            config,
            attempt.scope.model.clone(),
            None,
        ),
    };
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version":13, "contextId":attempt.context_id, "scope":attempt.scope,
        "baseCommit":repository::base_commit(&layout.cwd)?, "submittedMessages":attempt.input,
        "nativeHistory":{"mode":"fresh"}, "capabilities":capabilities,
        "instructions":instructions,"filesystemSkills":skills,"questionRoute":attempt.question_route
    }))?;
    let dispatch = store
        .prepare_dispatch(
            source_run.clone(),
            snapshot,
            vec![b"review fixture".to_vec()],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    store
        .record_agent_observation(
            source_run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some(format!("review-session-{source_run}")),
                    turn_id: Some(format!("review-turn-{source_run}")),
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
            source_run.clone(),
            RunStatus::Succeeded,
            Some(json!({"approved":approved}).to_string()),
            None,
            true,
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn unchanged_approved_review_is_reused_after_restart_and_rejected_review_is_dispatched_again()
-> Result<()> {
    for approved in [true, false] {
        let folder = tempfile::tempdir()?;
        let database = folder.path().join("review.sqlite");
        let store = Store::open(database.clone())?;
        let repo = approved_delivery_repository(&store, folder.path()).await?;
        let project = &repo.project_id;
        let draft = store
            .create_workflow_package(project.clone(), None, package()?)
            .await?;
        let WorkflowPackagePublishOutcome::Published { publication } = store
            .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
            .await?
        else {
            anyhow::bail!("The repeat package did not publish")
        };
        let version = &publication
            .members
            .iter()
            .find(|item| item.key == "root")
            .context("The root pipeline did not publish")?
            .artifact
            .version;
        let run = store.launch_workflow_request(serde_json::from_value(json!({
            "projectId":project, "workflow":WorkflowRevision {id:version.id.clone(),revision:version.revision.clone()},
            "checkoutId":repo.checkout_id, "requestId":"review-reuse", "inputs":{}, "automatic":true
        }))?).await?;
        let scope = SessionScope {
            provider: Provider::Codex,
            profile_id: "fixture-profile".into(),
            account_binding: Some("fixture-account".into()),
            executable: std::env::current_exe()?,
            checkout: repo.checkout_root.clone().into(),
            cli_version: "0.169.0".into(),
            runner: DirectWindowsRunner::IDENTITY.into(),
            permission_revision: "fixture".into(),
            mode: SessionMode::ReadOnly,
            model: Some("fixture-model".into()),
            effort: None,
        };
        let first_inputs = pinned_inputs(&store, &run, 1).await?;
        let first = store
            .next_workflow_agent()
            .await?
            .context("Missing initial review stage")?;
        first
            .enqueue(
                &store,
                admission(&repo, &run, scope.clone(), 1, first_inputs.clone()),
            )
            .await?;
        let attempt = store
            .claim_next_job(AdmissionEnvironment {
                runner: scope.runner.clone(),
                bridge_available: false,
            })
            .await?
            .context("The first review did not dispatch")?;
        complete(&store, &repo, &attempt, approved).await?;
        let second_inputs = pinned_inputs(&store, &run, 2).await?;
        assert_eq!(second_inputs.len(), 2);
        assert_ne!(first_inputs, second_inputs);
        drop(store);

        let store = Store::open(database)?;
        let second = store
            .next_workflow_agent()
            .await?
            .context("The second review was lost on restart")?;
        second
            .enqueue(&store, admission(&repo, &run, scope, 2, second_inputs))
            .await?;
        let slots = store
            .workflow_operator_jobs(project.clone(), run.id.clone())
            .await?;
        if approved {
            assert_eq!(slots.len(), 1);
            let view = store
                .inspect_workflow_execution(project.clone(), run.id.clone())
                .await?
                .unwrap();
            let output = view
                .outputs
                .into_iter()
                .find(|output| {
                    output.artifact.node_instance_id == "repeat::iteration-2::review"
                        && output.artifact.port == "response"
                })
                .context("The second round lost its reused output")?;
            let ArtifactValue::AgentResponse(response) = output.artifact.value else {
                unreachable!()
            };
            assert_eq!(response.run_id, attempt.run.id);
            assert!(response.reused_from_artifact_id.is_some());
            assert!(store.next_workflow_agent().await?.is_none());
        } else {
            assert_eq!(slots.len(), 2);
        }
    }
    Ok(())
}
