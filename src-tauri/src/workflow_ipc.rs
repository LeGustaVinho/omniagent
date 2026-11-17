use anyhow::{Context, Result};
use omniagent::{
    diagnostics,
    jobs::{Job, JobRequest, PipelineAgentInput, PipelineDispatch},
    model_catalog::ModelSelection,
    store::Store,
    workflows::{
        ArtifactValue, NodeInputs, WorkflowAgentSubmission, WorkflowArtifact,
        WorkflowDefinition, WorkflowDeliveryUnit, WorkflowHostRequest, WorkflowHostSubmission, WorkflowOperatorJob,
        WorkflowLaunchReceipt, WorkflowLaunchSource,
        WorkflowPlanHistory, WorkflowPlanProposal, WorkflowPlanRevisionRequest, WorkflowRun,
        WorkflowUnitOutcome, WorkflowUnitRequest, WorkflowUnitSubmission,
    },
};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::conversation_runtime::selected_scope;

pub mod approval;
pub mod authoring;
pub mod command;
pub mod comparison;
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryDelivery {
    run: WorkflowRun,
    definition: WorkflowDefinition,
    artifacts: Vec<WorkflowArtifact>,
    jobs: Vec<WorkflowOperatorJob>,
    host_outputs: NodeInputs,
    launch: Option<WorkflowLaunchReceipt>,
}

#[derive(Serialize)]
pub struct DeliveryTaskOption {
    id: String,
    title: String,
}

#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DeliveryHostAction {
    LinkExistingTask { task_id: String },
    CreateTask { title: String },
    ApproveImplementation,
    PrepareWorktree,
}

#[tauri::command]
pub async fn start_repository_delivery(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    revision_id: String,
) -> Result<WorkflowRun, String> {
    store.launch_builtin_repository_delivery(
        project_id, WorkflowLaunchSource { checkout_id, revision_id },
    ).await.map_err(report)
}

#[tauri::command]
pub async fn list_repository_deliveries(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<WorkflowRun>, String> {
    store.list_workflow_runs(project_id).await.map_err(report)
}

#[tauri::command]
pub async fn inspect_repository_delivery(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<RepositoryDelivery>, String> {
    async {
        let Some(run) = store
            .workflow_run(project_id.clone(), run_id.clone())
            .await?
        else {
            return Ok(None);
        };
        let definition = store
            .workflow_definition(project_id.clone(), run.version.clone())
            .await?
            .ok_or_else(|| anyhow::anyhow!("The pinned delivery definition is unavailable"))?;
        let artifacts = store
            .workflow_run_artifacts(project_id.clone(), run_id.clone())
            .await?;
        let host_outputs = store
            .current_workflow_host_outputs(project_id.clone(), run_id.clone())
            .await?;
        let launch = store.workflow_launch(project_id.clone(), run_id.clone()).await?;
        let jobs = store.workflow_operator_jobs(project_id, run_id).await?;
        Ok(Some(RepositoryDelivery {
            run,
            definition,
            artifacts,
            jobs,
            host_outputs,
            launch,
        }))
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn repository_delivery_tasks(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<DeliveryTaskOption>, String> {
    Ok(store
        .project_work_hierarchy(project_id)
        .await
        .map_err(report)?
        .work
        .into_iter()
        .map(|work| DeliveryTaskOption {
            id: work.task.id,
            title: work.task.title,
        })
        .collect())
}

#[tauri::command]
pub async fn repository_delivery_units(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    run_id: String,
) -> Result<Vec<WorkflowDeliveryUnit>, String> {
    store
        .workflow_operator_units(project_id, task_id, run_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn execute_repository_delivery_host(
    app: tauri::AppHandle,
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    inputs: NodeInputs,
    action: DeliveryHostAction,
) -> Result<WorkflowArtifact, String> {
    async {
        let (node_id, request) = match action {
            DeliveryHostAction::LinkExistingTask { task_id } => {
                ("work", WorkflowHostRequest::LinkExistingTask { task_id })
            }
            DeliveryHostAction::CreateTask { title } => {
                ("work", WorkflowHostRequest::CreateTask { title })
            }
            DeliveryHostAction::ApproveImplementation => {
                ("approve", WorkflowHostRequest::ApproveImplementation)
            }
            DeliveryHostAction::PrepareWorktree => (
                "prepare",
                WorkflowHostRequest::PrepareWorktree {
                    default_root: app.path().app_local_data_dir()?.join("worktrees"),
                },
            ),
        };
        store
            .execute_workflow_host(WorkflowHostSubmission {
                project_id,
                run_id,
                node_id: node_id.into(),
                inputs,
                request,
            })
            .await
    }
    .await
    .map_err(report)
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
    comparison: Option<omniagent::workflows::comparison::ComparisonSelection>,
}

#[tauri::command]
pub async fn submit_repository_delivery_agent(
    store: State<'_, Store>,
    input: DeliveryAgentInput,
) -> Result<Job, String> {
    async {
        let instructions = match input.node_id.as_str() {
            "plan" => "Propose an implementation plan for the selected repository document. Ask for missing decisions using the planning interface.",
            "review-plan" => "Review the pinned plan and explain unresolved issues. Keep the checkout read-only.",
            "implement" => "Implement the pinned logical unit on the approved branch. Keep unrelated changes out of the unit. Leave the changes in the worktree for review, checks and commit approval.",
            "review-candidate" => "Review the selected candidate tree for correctness and evaluate the pinned unit criteria.",
            _ => anyhow::bail!("select an agent stage of the built-in delivery"),
        };
        let comparison = input.comparison.clone();
        let submission = delivery_agent_submission(&store, input, instructions.into()).await?;
        store.submit_workflow_agent_with_comparison(submission, comparison).await
    }.await.map_err(report)
}

async fn delivery_agent_submission(
    store: &Store,
    input: DeliveryAgentInput,
    instructions: String,
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
            instructions,
            dependencies: Vec::new(),
            requires_bridge: false,
        },
        input: PipelineAgentInput::default(),
        artifacts: input.inputs,
        unit_id: input.unit_id,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryPlanRevisionInput {
    project_id: String,
    run_id: String,
    context_id: String,
    source_attempt_id: String,
    proposal_sequence: String,
    expected_plan_revision: Option<String>,
    selection: ModelSelection,
    effort: Option<String>,
    feedback: String,
}

#[tauri::command]
pub async fn request_repository_plan_revision(
    store: State<'_, Store>,
    input: DeliveryPlanRevisionInput,
) -> Result<Job, String> {
    async {
        anyhow::ensure!(
            !input.feedback.trim().is_empty(),
            "Add feedback for the planner"
        );
        let base = store
            .publish_workflow_artifact(
                input.project_id.clone(),
                input.run_id.clone(),
                "plan".into(),
                "plan".into(),
                ArtifactValue::Plan {
                    run_id: input.source_attempt_id,
                    event_sequence: input.proposal_sequence.parse()?,
                },
            )
            .await?;
        let submission = delivery_agent_submission(
            &store,
            DeliveryAgentInput {
                project_id: input.project_id,
                run_id: input.run_id,
                node_id: "plan".into(),
                unit_id: None,
                inputs: NodeInputs::from([("context".into(), input.context_id)]),
                selection: input.selection,
                effort: input.effort,
                comparison: None,
            },
            String::new(),
        )
        .await?;
        store
            .request_workflow_plan_revision(
                submission,
                WorkflowPlanRevisionRequest {
                    base_revision_id: base.id,
                    expected_plan_revision: input.expected_plan_revision,
                    feedback: input.feedback,
                },
            )
            .await
    }
    .await
    .map_err(report)
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

#[tauri::command]
pub async fn repository_delivery_plan_proposals(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    attempt_id: String,
) -> Result<Vec<WorkflowPlanProposal>, String> {
    store
        .workflow_plan_proposals(project_id, run_id, attempt_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn publish_repository_delivery_evidence(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    node_id: String,
    attempt_id: String,
    plan_sequence: Option<String>,
    expected_plan_revision: Option<String>,
) -> Result<WorkflowArtifact, String> {
    async {
        let (port, value) = match node_id.as_str() {
            "plan" => (
                "plan",
                ArtifactValue::Plan {
                    run_id: attempt_id,
                    event_sequence: plan_sequence
                        .context("select a recorded plan proposal")?
                        .parse()?,
                },
            ),
            "review-plan" | "review-candidate" => {
                ("review", ArtifactValue::Review { run_id: attempt_id })
            }
            _ => anyhow::bail!("this node does not publish planning or review evidence"),
        };
        let artifact = store
            .publish_workflow_artifact(
                project_id.clone(),
                run_id.clone(),
                node_id,
                port.into(),
                value,
            )
            .await?;
        if port == "plan" {
            store
                .workflow_plan_units(project_id.clone(), run_id.clone(), artifact.id.clone())
                .await?;
            store
                .set_job_pipeline_plan(
                    project_id,
                    run_id,
                    expected_plan_revision,
                    artifact.id.clone(),
                )
                .await?;
        }
        Ok(artifact)
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn publish_repository_delivery_candidate(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    task_id: String,
    selection_id: String,
) -> Result<WorkflowArtifact, String> {
    store
        .publish_workflow_candidate(
            project_id,
            run_id,
            "implement".into(),
            task_id,
            selection_id,
        )
        .await
        .map_err(report)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryUnitInput {
    project_id: String,
    run_id: String,
    task_id: String,
    node_id: String,
    unit_id: String,
    inputs: NodeInputs,
    request: WorkflowUnitRequest,
}

#[tauri::command]
pub async fn execute_repository_delivery_unit(
    store: State<'_, Store>,
    input: DeliveryUnitInput,
) -> Result<WorkflowUnitOutcome, String> {
    store
        .execute_workflow_unit(WorkflowUnitSubmission {
            project_id: input.project_id,
            run_id: input.run_id,
            task_id: input.task_id,
            node_id: input.node_id,
            unit_id: input.unit_id,
            inputs: input.inputs,
            request: input.request,
        })
        .await
        .map_err(report)
}
