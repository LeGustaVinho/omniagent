use anyhow::Result;
use omniagent::{
    model_catalog::ModelSelection,
    providers::SessionMode,
    store::Store,
    workflows::{
        WorkflowGraph, WorkflowRevision,
        authoring::{AuthoringBase, AuthoringDraft},
    },
};
use tauri::State;

use super::report;
use crate::conversation_runtime::selected_scope;

#[tauri::command]
pub async fn workflow_authoring_bases(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<AuthoringBase>, String> {
    store
        .workflow_authoring_bases(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn conversation_workflow_authoring(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<AuthoringDraft>, String> {
    store
        .conversation_workflow_authoring(conversation_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn start_workflow_authoring(
    store: State<'_, Store>,
    conversation_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    base: Option<WorkflowRevision>,
    brief: String,
) -> Result<AuthoringDraft, String> {
    async {
        let chat = store.conversation(conversation_id.clone()).await?;
        let scope = selected_scope(
            &store,
            chat.checkout_id.clone(),
            selection,
            effort,
            SessionMode::ReadOnly,
        )
        .await?;
        store
            .start_workflow_authoring(conversation_id, chat.checkout_id, scope, base, brief)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn inspect_workflow_authoring(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
) -> Result<AuthoringDraft, String> {
    store
        .inspect_workflow_authoring(project_id, draft_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_workflow_authoring_draft(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    selected_revision: String,
    graph: WorkflowGraph,
) -> Result<AuthoringDraft, String> {
    store
        .save_workflow_authoring_draft(project_id, draft_id, selected_revision, graph)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn request_workflow_authoring_changes(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    selected_revision: String,
    feedback: String,
    selection: ModelSelection,
    effort: Option<String>,
) -> Result<AuthoringDraft, String> {
    async {
        let checkout = store
            .workflow_authoring_checkout(project_id.clone(), draft_id.clone())
            .await?;
        let scope =
            selected_scope(&store, checkout, selection, effort, SessionMode::ReadOnly).await?;
        store
            .request_workflow_authoring_changes(
                project_id,
                draft_id,
                selected_revision,
                scope,
                feedback,
            )
            .await
    }
    .await
    .map_err(report)
}
