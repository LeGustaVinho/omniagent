use anyhow::Context;
use omniagent::{
    jobs::{Job, WorkflowGoalRequalification},
    model_catalog::ModelSelection,
    providers::SessionMode,
    repository::verification::CandidateVerification,
    store::Store,
    workflows::{
        Executor, NodeConfiguration, NodeInstance, authoring::package::PackageArtifactBody,
        builtin_registry, stage_goal::StageGoalAssessment, stage_plan::StagePlanConfiguration,
    },
};
use tauri::State;

use super::{DeliveryAgentInput, delivery_agent_submission, report};
use crate::conversation_runtime::selected_scope;

#[tauri::command]
pub async fn preview_workflow_goal_requalification(
    store: State<'_, Store>,
    input: DeliveryAgentInput,
) -> Result<WorkflowGoalRequalification, String> {
    async {
        let submission = delivery_agent_submission(&store, input).await?;
        store
            .preview_workflow_goal_requalification(submission)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn submit_workflow_goal_requalification(
    store: State<'_, Store>,
    input: DeliveryAgentInput,
    review_token: String,
) -> Result<Job, String> {
    async {
        let submission = delivery_agent_submission(&store, input).await?;
        store
            .submit_workflow_goal_requalification(submission, review_token)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn verify_workflow_goal_candidate(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    node_id: String,
    task_id: String,
    selection_id: String,
) -> Result<CandidateVerification, String> {
    store
        .verify_workflow_goal_candidate(project_id, run_id, node_id, task_id, selection_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn preview_workflow_stage_goal(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    node: NodeInstance,
    selection: ModelSelection,
    effort: Option<String>,
) -> Result<Option<StageGoalAssessment>, String> {
    async {
        store
            .validate_conversation_template_checkout(project_id.clone(), checkout_id.clone())
            .await?;
        let definition = builtin_registry()
            .into_iter()
            .find(|definition| definition.kind == node.kind)
            .context("The stage is outside the workflow catalog")?;
        let mode = match definition.executor {
            Executor::PlanAgent => StagePlanConfiguration::from_node(&node.configuration)
                .map_or(SessionMode::Plan, |plan| plan.mode()),
            Executor::ReadOnlyAgent => SessionMode::ReadOnly,
            Executor::WriteAgent => SessionMode::ApprovedWrite,
            _ => anyhow::bail!("Select an agent stage to preview Goal Mode"),
        };
        let effort = match &node.configuration {
            NodeConfiguration::AgentRole { requirements, .. }
                if requirements.bound_role.is_some() =>
            {
                let version = requirements
                    .bound_role
                    .clone()
                    .context("The pinned role is missing")?;
                let artifact = store
                    .published_workflow_package_artifact(project_id.clone(), version)
                    .await?
                    .context("The pinned role revision is unavailable")?;
                let PackageArtifactBody::Role { template } = artifact.body else {
                    anyhow::bail!("The pinned workflow artifact is not a role");
                };
                template.effort
            }
            _ => effort,
        };
        let scope = selected_scope(&store, checkout_id.clone(), selection, effort, mode).await?;
        store
            .preview_workflow_stage_goal(
                project_id,
                checkout_id,
                node,
                scope,
                std::env::current_exe().is_ok(),
            )
            .await
    }
    .await
    .map_err(report)
}
