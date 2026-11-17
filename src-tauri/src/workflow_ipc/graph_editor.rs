use omniagent::{
    store::Store,
    workflows::{BuiltinNode, NodeDefinition, authoring::ProjectWorkflowDraft, builtin_registry},
};
use tauri::State;

#[tauri::command]
pub fn workflow_node_definitions(kinds: Vec<BuiltinNode>) -> Vec<NodeDefinition> {
    builtin_registry()
        .into_iter()
        .filter(|definition| kinds.contains(&definition.kind))
        .collect()
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
