use std::collections::BTreeMap;

use omniagent::{
    diagnostics,
    model_catalog::{MODEL_CATALOG, ModelRating, ModelSelection},
    providers::Provider,
    store::Store,
    tasks::PlannedAssignment,
};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskModelAssignment {
    role: String,
    provider: Provider,
    profile_id: String,
    model_id: Option<String>,
}

impl From<PlannedAssignment> for TaskModelAssignment {
    fn from(assignment: PlannedAssignment) -> Self {
        Self {
            role: assignment.role,
            provider: assignment.provider,
            profile_id: assignment.profile_id,
            model_id: assignment.model,
        }
    }
}

fn report(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn project_task_assignments(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
) -> Result<Vec<TaskModelAssignment>, String> {
    let details = store
        .project_task_details(project_id, task_id)
        .await
        .map_err(report)?
        .ok_or_else(|| "Task is outside the selected project".to_owned())?;
    Ok(details
        .planned_assignments
        .into_iter()
        .map(TaskModelAssignment::from)
        .collect())
}

#[tauri::command]
pub async fn set_project_task_assignment(
    store: State<'_, Store>,
    project_id: String,
    task_id: String,
    role: String,
    selection: Option<ModelSelection>,
) -> Result<Vec<TaskModelAssignment>, String> {
    store
        .set_project_task_assignment(project_id, task_id, role, selection)
        .await
        .map(|assignments| {
            assignments
                .into_iter()
                .map(TaskModelAssignment::from)
                .collect()
        })
        .map_err(report)
}

#[tauri::command]
pub fn catalog_model_ratings(
    provider: Provider,
    model_ids: Vec<String>,
) -> BTreeMap<String, ModelRating> {
    model_ids
        .into_iter()
        .filter_map(|id| {
            let rating = MODEL_CATALOG.lookup(provider, &id)?.rating.clone();
            Some((id, rating))
        })
        .collect()
}
