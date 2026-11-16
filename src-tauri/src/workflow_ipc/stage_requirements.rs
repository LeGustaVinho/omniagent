use omniagent::{
    model_catalog::ModelSelection,
    providers::SessionMode,
    store::Store,
    workflows::stage_requirements::{
        StageProfileMatch, StageRequirementConfiguration, StageRequirementInventory,
        WorkflowStageAssessment,
    },
};
use tauri::State;

use super::report;
use crate::conversation_runtime::selected_scope;

#[tauri::command]
pub async fn workflow_stage_inventory(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    model: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    configuration: StageRequirementConfiguration,
) -> Result<StageRequirementInventory, String> {
    async {
        store
            .validate_conversation_template_checkout(project_id.clone(), checkout_id.clone())
            .await?;
        let scope = selected_scope(&store, checkout_id.clone(), model, effort, mode).await?;
        store
            .workflow_stage_inventory(project_id, checkout_id, scope, configuration)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn workflow_stage_assessment(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
) -> Result<Option<WorkflowStageAssessment>, String> {
    store
        .workflow_stage_assessment(project_id, run_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_stage_matching_profiles(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    models: Vec<ModelSelection>,
    effort: Option<String>,
    mode: SessionMode,
    configuration: StageRequirementConfiguration,
) -> Result<Vec<StageProfileMatch>, String> {
    async {
        anyhow::ensure!(models.len() <= 8, "Check up to eight CLI profiles at once");
        store
            .validate_conversation_template_checkout(project_id.clone(), checkout_id.clone())
            .await?;
        let mut tasks = tokio::task::JoinSet::new();
        for (ordinal, model) in models.into_iter().enumerate() {
            let (store, project, checkout, effort, configuration) = (
                store.inner().clone(),
                project_id.clone(),
                checkout_id.clone(),
                effort.clone(),
                configuration.clone(),
            );
            tasks.spawn(async move {
                let checked = async {
                    let scope =
                        selected_scope(&store, checkout.clone(), model.clone(), effort, mode)
                            .await?;
                    store
                        .workflow_stage_profile_match(
                            project,
                            checkout,
                            scope,
                            model.clone(),
                            configuration,
                        )
                        .await
                }
                .await;
                let result = checked.unwrap_or_else(|error| StageProfileMatch {
                    selection: model,
                    attempt_id: None,
                    checked_at: None,
                    blockers: vec![report(error)],
                });
                (ordinal, result)
            });
        }
        let mut matches = Vec::new();
        while let Some(result) = tasks.join_next().await {
            matches.push(result?);
        }
        matches.sort_by_key(|(ordinal, _)| *ordinal);
        Ok(matches.into_iter().map(|(_, result)| result).collect())
    }
    .await
    .map_err(report)
}
