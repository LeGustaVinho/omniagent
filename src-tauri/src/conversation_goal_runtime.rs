use anyhow::Result;
use omniagent::{
    diagnostics,
    jobs::{ConversationGoalOverview, GoalDefinition, GoalRevision, JobContext},
    model_catalog::ModelSelection,
    providers::SessionMode,
    store::Store,
};
use tauri::State;

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn conversation_goal_overview(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Option<ConversationGoalOverview>, String> {
    store
        .conversation_goal_overview(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn start_conversation_goal(
    store: State<'_, Store>,
    conversation_id: String,
    expected_context_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    definition: GoalDefinition,
    submitted_draft: Option<String>,
) -> Result<GoalRevision, String> {
    async {
        let conversation = store.conversation(conversation_id.clone()).await?;
        let scope = crate::conversation_runtime::selected_scope(
            &store,
            conversation.checkout_id,
            selection,
            effort,
            SessionMode::ReadOnly,
        )
        .await?;
        store
            .start_conversation_goal(
                JobContext {
                    conversation_id,
                    context_id: expected_context_id,
                },
                scope,
                None,
                definition,
                submitted_draft,
            )
            .await
    }
    .await
    .map_err(ui_error)
}
