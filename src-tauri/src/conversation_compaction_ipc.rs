use omniagent::{
    conversations::compaction::{CompactionProfile, CompactionSettings, CompactionStatus},
    diagnostics,
    jobs::JobScheduler,
    model_catalog::ModelSelection,
    providers::SessionMode,
    store::Store,
};
use tauri::State;

use crate::conversation_runtime::selected_scope;

#[tauri::command]
pub async fn conversation_compaction_state(
    store: State<'_, Store>,
    conversation_id: String,
    selection: Option<ModelSelection>,
    effort: Option<String>,
    mode: SessionMode,
    pending_text: Option<String>,
) -> Result<CompactionStatus, String> {
    let profile = selection.map(|model| CompactionProfile {
        model,
        effort,
        mode,
    });
    store
        .conversation_compaction(conversation_id, profile, pending_text.unwrap_or_default())
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn set_conversation_compaction_settings(
    store: State<'_, Store>,
    conversation_id: String,
    automatic: bool,
    threshold_percent: u8,
) -> Result<CompactionSettings, String> {
    store
        .set_conversation_compaction_settings(
            conversation_id,
            CompactionSettings {
                automatic,
                threshold_percent,
            },
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn request_conversation_compaction(
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    pending_text: String,
    automatic: bool,
) -> Result<CompactionStatus, String> {
    async {
        let checkout = store
            .conversation(conversation_id.clone())
            .await?
            .checkout_id;
        let scope = selected_scope(&store, checkout.clone(), selection, effort, mode).await?;
        let planning = match store
            .conversation_compaction_planning_model(conversation_id.clone())
            .await?
        {
            Some(model) => {
                selected_scope(&store, checkout, model, None, SessionMode::ReadOnly).await?
            }
            None => {
                let mut fallback = scope.clone();
                fallback.mode = SessionMode::ReadOnly;
                fallback
            }
        };
        store
            .request_conversation_compaction(
                conversation_id,
                expected_context_id,
                scope,
                planning,
                pending_text,
                automatic,
            )
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn request_native_conversation_compaction(
    store: State<'_, Store>,
    scheduler: State<'_, JobScheduler>,
    conversation_id: String,
    expected_context_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    pending_text: String,
) -> Result<CompactionStatus, String> {
    async {
        let checkout = store
            .conversation(conversation_id.clone())
            .await?
            .checkout_id;
        let scope =
            selected_scope(&store, checkout, selection.clone(), effort.clone(), mode).await?;
        scheduler
            .run_native_compaction(
                store.inner().clone(),
                conversation_id.clone(),
                expected_context_id,
                scope,
            )
            .await?;
        store
            .conversation_compaction(
                conversation_id,
                Some(CompactionProfile {
                    model: selection,
                    effort,
                    mode,
                }),
                pending_text,
            )
            .await
    }
    .await
    .map_err(report)
}

fn report(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}
