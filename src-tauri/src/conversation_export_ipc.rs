use omniagent::{
    conversations::transcript::{default_export_name, save_markdown},
    store::Store,
};
use tauri::{State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn export_conversation_markdown(
    window: WebviewWindow,
    store: State<'_, Store>,
    checkout_id: String,
    conversation_id: String,
) -> Result<bool, String> {
    let title = store
        .transcript_export_title(checkout_id.clone(), conversation_id.clone())
        .await
        .map_err(report)?;
    let destination = tauri::async_runtime::spawn_blocking(move || {
        window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Export conversation as Markdown")
            .set_file_name(default_export_name(&title))
            .add_filter("Markdown", &["md", "markdown"])
            .blocking_save_file()
            .map(|path| path.into_path())
            .transpose()
    })
    .await
    .map_err(report)?
    .map_err(report)?;
    let Some(destination) = destination else {
        return Ok(false);
    };
    let text = store
        .export_conversation_transcript(checkout_id, conversation_id)
        .await
        .map_err(report)?;
    tauri::async_runtime::spawn_blocking(move || save_markdown(&destination, &text))
        .await
        .map_err(report)?
        .map_err(report)?;
    Ok(true)
}

fn report(error: impl std::fmt::Display) -> String {
    omniagent::diagnostics::preview(&error.to_string())
}
