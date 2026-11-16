use omniagent::{
    diagnostics,
    model_catalog::{
        ModelSelection,
        defaults::{ModelDefaultPurpose, ProjectModelDefaults},
    },
    providers::Provider,
    store::Store,
};
use tauri::State;

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn project_model_defaults(
    store: State<'_, Store>,
    project_id: String,
) -> Result<ProjectModelDefaults, String> {
    store
        .project_model_defaults(project_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn checkout_model_defaults(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<ProjectModelDefaults, String> {
    store
        .checkout_model_defaults(checkout_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn set_project_model_default(
    store: State<'_, Store>,
    project_id: String,
    purpose: ModelDefaultPurpose,
    selection: Option<ModelSelection>,
) -> Result<ProjectModelDefaults, String> {
    async {
        if let Some(selection) = &selection {
            crate::conversation_runtime::validate_model_choice(&store, selection).await?;
        }
        store
            .set_project_model_default(project_id, purpose, selection)
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn execution_profile_default_model(
    store: State<'_, Store>,
    provider: Provider,
    profile_id: String,
) -> Result<Option<ModelSelection>, String> {
    store
        .execution_profile_default_model(provider, profile_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn set_execution_profile_default_model(
    store: State<'_, Store>,
    provider: Provider,
    profile_id: String,
    selection: Option<ModelSelection>,
) -> Result<Option<ModelSelection>, String> {
    async {
        if let Some(selection) = &selection {
            crate::conversation_runtime::validate_model_choice(&store, selection).await?;
        }
        store
            .set_execution_profile_default_model(provider, profile_id, selection)
            .await
    }
    .await
    .map_err(ui_error)
}
