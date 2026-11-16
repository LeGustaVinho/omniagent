use omniagent::{
    conversations::{
        ContinuationChoice, ContinuationInspection, ContinuationLevel, ContinuationSettings,
        ContinuedConversation,
    },
    diagnostics,
    model_catalog::ModelSelection,
    providers::SessionMode,
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
) -> Result<ContinuationInspection, String> {
    async {
        let conversation = store.conversation(conversation_id.clone()).await?;
        let scope = crate::conversation_runtime::selected_scope(
            &store,
            conversation.checkout_id,
            selection,
            effort,
            mode,
        )
        .await?;
        store
            .submit_conversation_continuation(conversation_id, expected_context_id, scope, text)
            .await
    }
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
        let conversation = store.conversation(conversation_id.clone()).await?;
        let scope = crate::conversation_runtime::selected_scope(
            &store,
            conversation.checkout_id,
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
