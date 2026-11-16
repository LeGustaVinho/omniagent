use omniagent::{
    diagnostics,
    jobs::{InstructionDeliveryEvidence, JobScheduler},
    model_catalog::ModelSelection,
    providers::SessionMode,
    store::Store,
};
use tauri::State;

fn preview(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn preview_conversation_steering(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
    expected_context_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
) -> Result<bool, String> {
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
            .can_steer_conversation_draft(conversation_id, run_id, expected_context_id, scope)
            .await
    }
    .await
    .map_err(preview)
}

#[tauri::command]
pub async fn steer_queued_conversation_instruction(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
    run_id: String,
    instruction_id: String,
) -> Result<bool, String> {
    async {
        let Some(revision) = store
            .queued_steering_revision(conversation_id.clone(), instruction_id.clone())
            .await?
        else {
            return Ok(false);
        };
        scheduler
            .steer_pending_instruction(&store, conversation_id, run_id, instruction_id, revision)
            .await
    }
    .await
    .map_err(preview)
}

#[tauri::command]
pub async fn conversation_instruction_deliveries(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<InstructionDeliveryEvidence>, String> {
    store
        .conversation_instruction_deliveries(conversation_id)
        .await
        .map_err(preview)
}
