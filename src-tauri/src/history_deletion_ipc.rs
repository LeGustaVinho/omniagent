use omniagent::{
    diagnostics,
    dispatch::{
        HistoryDeletionDiff, HistoryDeletionMode, HistoryDeletionPreview, HistoryDeletionTarget,
    },
    store::Store,
};
use tauri::State;

#[tauri::command]
pub async fn conversation_history_deletion_diffs(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<HistoryDeletionDiff>, String> {
    store
        .conversation_history_deletion_diffs(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn preview_conversation_history_deletion(
    store: State<'_, Store>,
    target: HistoryDeletionTarget,
    mode: HistoryDeletionMode,
) -> Result<HistoryDeletionPreview, String> {
    store
        .preview_conversation_history_deletion(target, mode)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn confirm_conversation_history_deletion(
    store: State<'_, Store>,
    target: HistoryDeletionTarget,
    mode: HistoryDeletionMode,
    expected_revision: String,
) -> Result<HistoryDeletionPreview, String> {
    store
        .confirm_conversation_history_deletion(target, mode, expected_revision)
        .await
        .map_err(ui_error)
}

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&format!("{error:#}"))
}
