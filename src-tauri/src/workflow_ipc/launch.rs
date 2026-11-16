use omniagent::{
    store::Store,
    workflows::{ManualWorkflowLaunch, WorkflowLaunchReceipt, WorkflowRun},
};
use tauri::State;

#[tauri::command]
pub async fn launch_workflow_manual(
    store: State<'_, Store>,
    request: ManualWorkflowLaunch,
) -> Result<WorkflowRun, String> {
    store
        .launch_workflow_manual(request)
        .await
        .map_err(super::report)
}

#[tauri::command]
pub async fn workflow_launch(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<WorkflowLaunchReceipt>, String> {
    store
        .workflow_launch(project_id, run_id)
        .await
        .map_err(super::report)
}
