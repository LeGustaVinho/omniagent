use omniagent::{
    store::Store,
    workflows::{
        authoring::package::WorkflowPackageDraft,
        templates::{WorkflowFactoryEntry, WorkflowFactoryTemplate},
    },
};
use tauri::State;

use super::report;

#[tauri::command]
pub fn workflow_factory_templates() -> Vec<WorkflowFactoryEntry> {
    omniagent::workflows::templates::workflow_factory_templates()
}

#[tauri::command]
pub async fn create_workflow_factory_draft(
    store: State<'_, Store>,
    project_id: String,
    template: WorkflowFactoryTemplate,
    revision: u16,
) -> Result<WorkflowPackageDraft, String> {
    store
        .create_workflow_factory_draft(project_id, template, revision)
        .await
        .map_err(report)
}
