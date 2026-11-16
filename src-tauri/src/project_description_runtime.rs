use omniagent::{
    diagnostics,
    store::Store,
    tasks::{DescriptionDraft, DescriptionEditorSave, DescriptionEditorState, DescriptionOwner},
};
use tauri::State;

fn report(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn project_description_editor(
    store: State<'_, Store>,
    project_id: String,
    owner: DescriptionOwner,
) -> Result<DescriptionEditorState, String> {
    store
        .project_description_editor(project_id, owner)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_project_description_draft(
    store: State<'_, Store>,
    project_id: String,
    owner: DescriptionOwner,
    base_revision: Option<String>,
    expected_draft_revision: Option<String>,
    markdown: String,
) -> Result<DescriptionDraft, String> {
    store
        .save_project_description_draft(
            project_id,
            owner,
            base_revision,
            expected_draft_revision,
            markdown,
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn discard_project_description_draft(
    store: State<'_, Store>,
    project_id: String,
    owner: DescriptionOwner,
    expected_draft_revision: String,
) -> Result<bool, String> {
    store
        .discard_project_description_draft(project_id, owner, expected_draft_revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_project_description_editor(
    store: State<'_, Store>,
    project_id: String,
    owner: DescriptionOwner,
    expected_revision: Option<String>,
    expected_draft_revision: Option<String>,
    markdown: Option<String>,
) -> Result<DescriptionEditorSave, String> {
    store
        .save_project_description_editor(
            project_id,
            owner,
            expected_revision,
            expected_draft_revision,
            markdown,
        )
        .await
        .map_err(report)
}
