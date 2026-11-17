use crate::conversation_runtime::selected_scope_with_model_option;
use anyhow::Result;
use omniagent::{
    conversations::SessionScope,
    model_catalog::{ModelOption, ModelSelection},
    providers::SessionMode,
    store::Store,
    workflows::comparison::{
        ComparisonEnvironment, ComparisonInput, ComparisonMarkdownSource, ComparisonOutcome,
        ComparisonSelection, ComparisonSynthesis, ModelComparison,
    },
};
use tauri::{AppHandle, Manager, State};

use super::{report, selected_scope};

#[tauri::command]
pub async fn comparison_source_commit(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
) -> Result<Option<String>, String> {
    store
        .comparison_source_commit(project_id, checkout_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn model_comparisons(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<ModelComparison>, String> {
    store.model_comparisons(project_id).await.map_err(report)
}

#[tauri::command]
pub async fn model_comparison_environments(
    store: State<'_, Store>,
    project_id: String,
    group_id: String,
) -> Result<Vec<ComparisonEnvironment>, String> {
    store
        .model_comparison_environments(project_id, group_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn model_comparison_outcomes(
    store: State<'_, Store>,
    project_id: String,
    group_id: String,
) -> Result<Vec<ComparisonOutcome>, String> {
    store
        .model_comparison_outcomes(project_id, group_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn model_comparison_job_inputs(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
) -> Result<Vec<ComparisonInput>, String> {
    store
        .model_comparison_job_inputs(project_id, job_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn model_comparison_syntheses(
    store: State<'_, Store>,
    project_id: String,
    group_id: String,
) -> Result<Vec<ComparisonSynthesis>, String> {
    store
        .model_comparison_syntheses(project_id, group_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn start_model_comparison_synthesis(
    store: State<'_, Store>,
    project_id: String,
    sources: ComparisonSelection,
    selection: ModelSelection,
) -> Result<ComparisonSynthesis, String> {
    async {
        let checkout = store
            .model_comparison_checkout(project_id.clone(), sources.group_id.clone())
            .await?;
        let scope =
            selected_scope(&store, checkout, selection, None, SessionMode::ReadOnly).await?;
        store
            .start_model_comparison_synthesis(project_id, sources, scope)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn start_model_comparison(
    app: AppHandle,
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    commit: String,
    prompt: String,
    selections: Vec<ModelSelection>,
) -> Result<ModelComparison, String> {
    async {
        let scopes = comparison_profiles(&store, &checkout_id, selections).await?;
        let storage = app
            .path()
            .app_local_data_dir()?
            .join("comparison-checkouts");
        store
            .start_model_comparison_with_options(
                project_id,
                checkout_id,
                commit,
                prompt,
                scopes,
                storage,
            )
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn start_model_comparison_from_markdown(
    app: AppHandle,
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    revision_id: String,
    relative_path: String,
    sha256: String,
    prompt: String,
    selections: Vec<ModelSelection>,
) -> Result<ModelComparison, String> {
    async {
        let scopes = comparison_profiles(&store, &checkout_id, selections).await?;
        let storage = app
            .path()
            .app_local_data_dir()?
            .join("comparison-checkouts");
        store
            .start_model_comparison_from_markdown(
                project_id,
                checkout_id,
                ComparisonMarkdownSource {
                    revision_id,
                    relative_path,
                    sha256,
                },
                prompt,
                scopes,
                storage,
            )
            .await
    }
    .await
    .map_err(report)
}

async fn comparison_profiles(
    store: &Store,
    checkout_id: &str,
    selections: Vec<ModelSelection>,
) -> Result<Vec<(SessionScope, Option<ModelOption>)>> {
    let mut profiles = Vec::with_capacity(selections.len());
    for selection in selections {
        let (scope, option) = selected_scope_with_model_option(
            store,
            checkout_id.to_owned(),
            selection,
            None,
            SessionMode::ReadOnly,
        )
        .await?;
        profiles.push((scope, Some(option)));
    }
    Ok(profiles)
}
