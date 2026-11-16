use omniagent::{
    conversations::{
        ContinuationChoice, ContinuationInspection, ContinuationLevel, ContinuationSettings,
        ContinuedConversation,
        references::{
            ConversationReferenceSelection, ConversationReferenceSnapshot,
            ConversationReferenceSource,
        },
    },
    diagnostics,
    model_catalog::ModelSelection,
    providers::SessionMode,
    repository::ProjectCheckout,
    store::Store,
};
use tauri::State;

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn conversation_continuation_settings(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<ContinuationSettings, String> {
    store
        .conversation_continuation_settings(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn set_conversation_continuation_policy(
    store: State<'_, Store>,
    conversation_id: String,
    level: ContinuationLevel,
    choice: Option<ContinuationChoice>,
) -> Result<ContinuationSettings, String> {
    store
        .set_conversation_continuation_policy(conversation_id, level, choice)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn submit_conversation_continuation(
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    text: String,
    reference_ids: Option<Vec<String>>,
    attachment_preview_id: Option<String>,
    target_checkout_id: Option<String>,
) -> Result<ContinuationInspection, String> {
    async {
        let checkout_id = store.conversation_continuation_checkout_id(conversation_id.clone(), target_checkout_id.clone()).await?;
        let scope = crate::conversation_runtime::selected_scope(
            &store,
            checkout_id,
            selection,
            effort,
            mode,
        )
        .await?;
        store
            .submit_conversation_with_files_at_checkout(
                conversation_id,
                expected_context_id,
                scope,
                text,
                reference_ids.unwrap_or_default(),
                attachment_preview_id,
                target_checkout_id,
            )
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_continuation_checkouts(store: State<'_, Store>, conversation_id: String) -> Result<Vec<ProjectCheckout>, String> {
    store.conversation_continuation_checkouts(conversation_id).await.map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_reference_source(
    store: State<'_, Store>,
    destination_conversation_id: String,
    source_conversation_id: String,
    source_context_id: Option<String>,
) -> Result<ConversationReferenceSource, String> {
    store
        .conversation_reference_source(
            destination_conversation_id,
            source_conversation_id,
            source_context_id,
        )
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn preview_conversation_reference(
    store: State<'_, Store>,
    destination_conversation_id: String,
    source_conversation_id: String,
    selection: ConversationReferenceSelection,
) -> Result<ConversationReferenceSnapshot, String> {
    store
        .preview_conversation_reference(
            destination_conversation_id,
            source_conversation_id,
            selection,
        )
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn conversation_reference_draft(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<ConversationReferenceSnapshot>, String> {
    store
        .conversation_reference_draft(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn edit_conversation_reference_draft(
    store: State<'_, Store>,
    conversation_id: String,
    preview_id: String,
    attach: bool,
) -> Result<Vec<ConversationReferenceSnapshot>, String> {
    store
        .edit_conversation_reference_draft(conversation_id, preview_id, attach)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn confirm_conversation_continuation(
    store: State<'_, Store>,
    conversation_id: String,
    preview_id: String,
    choice: ContinuationChoice,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    text: String,
) -> Result<ContinuedConversation, String> {
    async {
        let checkout_id = store.conversation_continuation_review_checkout(conversation_id.clone(), preview_id.clone()).await?;
        let scope = crate::conversation_runtime::selected_scope(
            &store,
            checkout_id,
            selection,
            effort,
            mode,
        )
        .await?;
        store
            .confirm_conversation_continuation(conversation_id, preview_id, choice, scope, text)
            .await
    }
    .await
    .map_err(ui_error)
}
