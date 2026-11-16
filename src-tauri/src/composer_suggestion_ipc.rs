use omniagent::{
    diagnostics,
    model_catalog::ModelSelection,
    providers::{SessionMode, suggestions::SuggestionOutcome},
    store::Store,
};
use tauri::State;

use crate::conversation_runtime::selected_scope;

#[tauri::command]
pub async fn composer_native_suggestion(
    store: State<'_, Store>,
    conversation_id: String,
    run_id: String,
    context_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
) -> Result<SuggestionOutcome, String> {
    async {
        let chat = store.conversation(conversation_id.clone()).await?;
        if chat.context_id != context_id {
            return Ok(SuggestionOutcome::Ineligible);
        }
        let scope = selected_scope(&store, chat.checkout_id, selection, effort, mode).await?;
        store
            .composer_native_suggestion(conversation_id, context_id, run_id, scope)
            .await
    }
    .await
    .map_err(|error| diagnostics::preview(&error.to_string()))
}
