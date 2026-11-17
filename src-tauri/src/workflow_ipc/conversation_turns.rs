use omniagent::{
    store::Store,
    workflows::conversation_turns::{
        ConversationPipelineSchema, ConversationPipelineTurn, ConversationPipelineTurnRequest,
    },
};
use tauri::State;

#[tauri::command]
pub async fn conversation_pipeline_schema(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<ConversationPipelineSchema, String> {
    store
        .conversation_pipeline_schema(conversation_id)
        .await
        .map_err(super::report)
}

#[tauri::command]
pub async fn enqueue_conversation_pipeline_turn(
    store: State<'_, Store>,
    request: ConversationPipelineTurnRequest,
) -> Result<ConversationPipelineTurn, String> {
    store
        .enqueue_conversation_pipeline_turn(request)
        .await
        .map_err(super::report)
}

#[tauri::command]
pub async fn conversation_pipeline_turns(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<ConversationPipelineTurn>, String> {
    store
        .conversation_pipeline_turns(conversation_id)
        .await
        .map_err(super::report)
}
