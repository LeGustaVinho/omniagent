use omniagent::{
    diagnostics,
    markdown_analysis::{MarkdownAnalysis, analyze_markdown},
    markdown_properties::replace_markdown_property,
};

#[tauri::command]
pub async fn analyze_note_markdown(
    markdown: String,
    buffer_revision: String,
) -> Result<MarkdownAnalysis, String> {
    tokio::task::spawn_blocking(move || analyze_markdown(&markdown, buffer_revision))
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))
}

#[tauri::command]
pub async fn replace_note_property(
    markdown: String,
    key: String,
    value: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || replace_markdown_property(&markdown, &key, &value))
        .await
        .map_err(|error| diagnostics::preview(&error.to_string()))?
        .map_err(|error| diagnostics::preview(&error.to_string()))
}
