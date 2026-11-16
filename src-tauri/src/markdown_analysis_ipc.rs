use omniagent::{
    diagnostics,
    markdown_analysis::{MarkdownAnalysis, analyze_markdown},
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
