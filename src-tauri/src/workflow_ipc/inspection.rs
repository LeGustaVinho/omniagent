use omniagent::{
    jobs::{AdmissionEnvironment, JobInspection},
    runner::{DirectWindowsRunner, Runner},
    store::Store,
    workflows::inspection::WorkflowExecutionInspection,
};
use tauri::State;

use super::report;

#[tauri::command]
pub async fn inspect_workflow_execution(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<WorkflowExecutionInspection>, String> {
    store
        .inspect_workflow_execution(project_id, run_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn inspect_workflow_execution_job(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    job_id: String,
) -> Result<Option<JobInspection>, String> {
    store
        .inspect_workflow_execution_job(
            project_id,
            run_id,
            job_id,
            AdmissionEnvironment {
                runner: DirectWindowsRunner.identity().into(),
                bridge_available: std::env::current_exe().is_ok(),
            },
        )
        .await
        .map_err(report)
}
