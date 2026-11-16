use omniagent::{
    store::Store,
    workflows::{NodeDefinition, authoring::ProjectWorkflowDraft, builtin_registry},
};
use tauri::State;

#[tauri::command]
pub fn workflow_graph_registry() -> Vec<NodeDefinition> {
    builtin_registry()
}

#[tauri::command]
pub async fn project_workflow_graph_drafts(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<ProjectWorkflowDraft>, String> {
    store
        .project_workflow_graph_drafts(project_id)
        .await
        .map_err(super::report)
}
