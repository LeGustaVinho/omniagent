use omniagent::{
    diagnostics,
    model_catalog::ModelSelection,
    providers::{SessionMode, composer_customizations::ComposerCustomization},
    store::Store,
};
use tauri::State;

use crate::conversation_runtime::selected_scope;

#[tauri::command]
pub async fn composer_customizations(
    store: State<'_, Store>,
    conversation_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
) -> Result<Vec<ComposerCustomization>, String> {
    async {
        let chat = store.conversation(conversation_id.clone()).await?;
        let scope = selected_scope(&store, chat.checkout_id, selection, effort, mode).await?;
        store.composer_customizations(conversation_id, scope).await
    }
    .await
    .map_err(|error| diagnostics::preview(&error.to_string()))
}
