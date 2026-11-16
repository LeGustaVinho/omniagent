use omniagent::{
    conversations::{
        ContinuationLevel, Conversation,
        fork_checkout::{
            ForkCheckoutChoice, ForkCheckoutPolicy, ForkCheckoutPreview, ForkCheckoutSettings,
            ForkConversationRequest,
        },
    },
    diagnostics,
    store::Store,
};
use tauri::{AppHandle, Manager, State};

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn conversation_fork_checkout_settings(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<ForkCheckoutSettings, String> {
    store
        .conversation_fork_checkout_settings(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn set_conversation_fork_checkout_policy(
    store: State<'_, Store>,
    conversation_id: String,
    level: ContinuationLevel,
    choice: Option<ForkCheckoutPolicy>,
) -> Result<ForkCheckoutSettings, String> {
    store
        .set_conversation_fork_checkout_policy(conversation_id, level, choice)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn preview_conversation_fork_checkout(
    app: AppHandle,
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: String,
    choice: Option<ForkCheckoutChoice>,
    selected_paths: Option<Vec<String>>,
) -> Result<ForkCheckoutPreview, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|error| diagnostics::preview(&error.to_string()))?
        .join("worktrees");
    store
        .preview_conversation_fork_checkout(
            conversation_id,
            expected_context_id,
            choice,
            selected_paths.unwrap_or_default(),
            root,
        )
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn fork_conversation_with_checkout(
    store: State<'_, Store>,
    request: ForkConversationRequest,
) -> Result<Conversation, String> {
    store
        .fork_conversation_with_checkout(request)
        .await
        .map_err(ui_error)
}
