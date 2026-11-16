use omniagent::{diagnostics, store::Store};
use tauri::State;

#[tauri::command]
pub async fn delete_conversation_side_question(
    store: State<'_, Store>,
    conversation_id: String,
    job_id: String,
) -> Result<(), String> {
    store
        .delete_conversation_side_question(conversation_id, job_id)
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}
