use anyhow::Result;
use omniagent::{
    diagnostics,
    jobs::{Job, JobRequest, PipelineAgentInput, PipelineDispatch},
    model_catalog::ModelSelection,
    store::Store,
    workflows::{
        BuiltinNode, NodeInputs, WorkflowAgentSubmission, WorkflowPlanHistory, WorkflowRun,
    },
};
use serde::Deserialize;
use tauri::State;

use crate::conversation_runtime::selected_scope;

pub mod authoring;
pub mod command;
pub mod comparison;
pub mod conversation_turns;
pub mod decisions;
pub mod github;
pub mod goal;
pub mod graph_editor;
pub mod inspection;
pub mod launch;
pub mod maintenance;
pub mod mcp_tools;
pub mod role_templates;
pub mod stage_requirements;
pub mod triggers;

fn report(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn list_repository_deliveries(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<WorkflowRun>, String> {
    store.list_workflow_runs(project_id).await.map_err(report)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryAgentInput {
    project_id: String,
    run_id: String,
    node_id: String,
    unit_id: Option<String>,
    inputs: NodeInputs,
    selection: ModelSelection,
    effort: Option<String>,
}

#[tauri::command]
pub async fn submit_repository_delivery_agent(
    store: State<'_, Store>,
    input: DeliveryAgentInput,
) -> Result<Job, String> {
    async {
        let submission = delivery_agent_submission(&store, input).await?;
        store.submit_workflow_agent(submission).await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn workflow_agent_response(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    artifact_id: String,
) -> Result<String, String> {
    store
        .workflow_agent_response(project_id, run_id, artifact_id)
        .await
        .map_err(report)
}

async fn delivery_agent_submission(
    store: &Store,
    input: DeliveryAgentInput,
) -> Result<WorkflowAgentSubmission> {
    let target = store
        .workflow_operator_agent_target(
            &input.project_id,
            &input.run_id,
            &input.node_id,
            input.unit_id.as_deref(),
            &input.inputs,
        )
        .await?;
    anyhow::ensure!(target.kind == BuiltinNode::Agent, "Select an AI Node Agent");
    let scope = selected_scope(
        store,
        target.checkout_id.clone(),
        input.selection,
        input.effort,
        target.mode,
    )
    .await?;
    Ok(WorkflowAgentSubmission {
        project_id: input.project_id,
        dispatch: PipelineDispatch {
            pipeline_run_id: input.run_id,
            node_instance_id: input.node_id,
            plan_revision: target.plan_revision,
            release_id: target.release_id,
        },
        scope,
        request: JobRequest {
            checkout_id: Some(target.checkout_id),
            task_id: target.task_id,
            mode: target.mode,
            instructions: String::new(),
            dependencies: Vec::new(),
            requires_bridge: false,
        },
        input: PipelineAgentInput::default(),
        artifacts: input.inputs,
        unit_id: input.unit_id,
    })
}

#[tauri::command]
pub async fn repository_plan_history(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<WorkflowPlanHistory>, String> {
    store
        .workflow_plan_history(project_id, run_id)
        .await
        .map_err(report)
}
