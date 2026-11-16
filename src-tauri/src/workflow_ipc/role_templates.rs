use anyhow::{Result, ensure};
use omniagent::{
    conversations::SessionScope,
    model_catalog::ModelSelection,
    providers::SessionMode,
    store::Store,
    workflows::role_templates::{
        ConversationTemplate, ConversationTemplateDetail, ConversationTemplateHead,
        SaveConversationTemplate, TemplateBase, TemplateProfileMatrix,
    },
};
use tauri::State;

use crate::conversation_runtime::selected_scope;

use super::report;

#[tauri::command]
pub async fn list_conversation_templates(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<ConversationTemplateHead>, String> {
    store
        .list_conversation_templates(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn conversation_template_revision(
    store: State<'_, Store>,
    project_id: String,
    template_id: String,
    revision: String,
) -> Result<ConversationTemplateDetail, String> {
    store
        .conversation_template_revision(project_id, template_id, revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn conversation_template_profile(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    model: ModelSelection,
    effort: Option<String>,
    tool_scope: SessionMode,
) -> Result<TemplateProfileMatrix, String> {
    async {
        let scope =
            template_scope(&store, &project_id, &checkout_id, model, effort, tool_scope).await?;
        store
            .conversation_template_profile(checkout_id, scope)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn save_conversation_template(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    base: Option<TemplateBase>,
    template: ConversationTemplate,
) -> Result<SaveConversationTemplate, String> {
    async {
        let scope = template_scope(
            &store,
            &project_id,
            &checkout_id,
            template.model.clone(),
            template.effort.clone(),
            template.tool_scope,
        )
        .await?;
        let matrix = store
            .conversation_template_profile(checkout_id, scope)
            .await?;
        if let Some(reason) = matrix
            .tool_scopes
            .iter()
            .find(|option| option.mode == template.tool_scope)
            .and_then(|option| option.blocker)
        {
            anyhow::bail!("{reason}");
        }
        store
            .save_conversation_template(project_id, base, template)
            .await
    }
    .await
    .map_err(report)
}

async fn template_scope(
    store: &Store,
    project: &str,
    checkout: &str,
    model: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
) -> Result<SessionScope> {
    ensure!(
        matches!(mode, SessionMode::ReadOnly | SessionMode::ApprovedWrite),
        "Choose repository read or approved worktree write for this template"
    );
    store
        .validate_conversation_template_checkout(project.into(), checkout.into())
        .await?;
    selected_scope(store, checkout.into(), model, effort, mode).await
}
