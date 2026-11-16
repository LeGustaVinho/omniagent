use std::{
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use omniagent::{
    conversations::draft_files::{AttachmentDraft, AttachmentDraftReview, DraftAttachmentEdit},
    diagnostics,
    model_catalog::ModelSelection,
    providers::SessionMode,
    repository,
    store::Store,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, Webview, WebviewEvent, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

const DROP_EVENT: &str = "conversation-attachment-drop";

#[derive(Default)]
pub(crate) struct AttachmentDropReceipts(Mutex<Option<DroppedFiles>>);

struct DroppedFiles {
    id: String,
    window: String,
    received: Instant,
    paths: Vec<PathBuf>,
}

#[derive(Clone, Serialize)]
struct DropPreview {
    id: String,
    x: f64,
    y: f64,
}

impl AttachmentDropReceipts {
    fn issue(&self, window: &str, paths: Vec<PathBuf>) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        *self
            .0
            .lock()
            .ok()
            .context("File drop state is unavailable")? = Some(DroppedFiles {
            id: id.clone(),
            window: window.to_owned(),
            received: Instant::now(),
            paths,
        });
        Ok(id)
    }

    fn take(&self, id: &str, window: &str) -> Result<Vec<PathBuf>> {
        let mut state = self
            .0
            .lock()
            .ok()
            .context("File drop state is unavailable")?;
        let matches = state.as_ref().is_some_and(|receipt| {
            receipt.id == id
                && receipt.window == window
                && receipt.received.elapsed() < Duration::from_secs(30)
        });
        anyhow::ensure!(matches, "This file drop expired. Drop the files again.");
        Ok(state
            .take()
            .context("The files were already claimed")?
            .paths)
    }
}

pub(crate) fn retain_attachment_drop(webview: &Webview, event: &WebviewEvent) {
    let WebviewEvent::DragDrop(tauri::DragDropEvent::Drop {
        paths, position, ..
    }) = event
    else {
        return;
    };
    if paths.is_empty() {
        return;
    }
    let receipts = webview.state::<AttachmentDropReceipts>();
    if let Ok(id) = receipts.issue(webview.label(), paths.clone()) {
        let _ = webview.emit_to(
            webview.label(),
            DROP_EVENT,
            DropPreview {
                id,
                x: position.x,
                y: position.y,
            },
        );
    }
}

fn ui_error(error: anyhow::Error) -> String {
    diagnostics::preview(&format!("{error:#}"))
}

#[tauri::command]
pub async fn conversation_attachment_draft(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<AttachmentDraft, String> {
    store
        .conversation_attachment_draft(conversation_id)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn pick_conversation_attachments(
    app: AppHandle,
    window: WebviewWindow,
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<AttachmentDraft, String> {
    async {
        let conversation = store.conversation(conversation_id.clone()).await?;
        let checkout = repository::checkout_root(&store, conversation.checkout_id).await?;
        let files = tauri::async_runtime::spawn_blocking(move || {
            app.dialog()
                .file()
                .set_title("Attach files to the conversation")
                .set_directory(checkout)
                .set_parent(&window)
                .blocking_pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|file| file.into_path())
                .collect::<Result<Vec<_>, _>>()
        })
        .await??;
        store
            .add_conversation_attachment_paths(conversation_id, files)
            .await
    }
    .await
    .map_err(ui_error)
}

#[tauri::command]
pub async fn accept_conversation_attachment_drop(
    window: WebviewWindow,
    receipts: State<'_, AttachmentDropReceipts>,
    store: State<'_, Store>,
    conversation_id: String,
    drop_id: String,
) -> Result<AttachmentDraft, String> {
    let paths = receipts.take(&drop_id, window.label()).map_err(ui_error)?;
    store
        .add_conversation_attachment_paths(conversation_id, paths)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn edit_conversation_attachment_draft(
    store: State<'_, Store>,
    conversation_id: String,
    expected_revision: String,
    file_id: String,
    edit: DraftAttachmentEdit,
) -> Result<AttachmentDraft, String> {
    store
        .edit_conversation_attachment_draft(conversation_id, expected_revision, file_id, edit)
        .await
        .map_err(ui_error)
}

#[tauri::command]
pub async fn review_conversation_attachment_draft(
    store: State<'_, Store>,
    conversation_id: String,
    expected_revision: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    text: String,
) -> Result<AttachmentDraftReview, String> {
    async {
        let owner = store.conversation(conversation_id.clone()).await?;
        let scope = crate::conversation_runtime::selected_scope(
            &store,
            owner.checkout_id,
            selection,
            effort,
            mode,
        )
        .await?;
        store
            .review_conversation_attachment_draft(conversation_id, expected_revision, scope, text)
            .await
    }
    .await
    .map_err(ui_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_drop_can_only_be_claimed_once_by_the_receiving_window() -> Result<()> {
        let receipts = AttachmentDropReceipts::default();
        let path = PathBuf::from("C:/fixture/preview.txt");
        let token = receipts.issue("main", vec![path.clone()])?;
        assert!(receipts.take(&token, "other").is_err());
        assert!(receipts.take("forged", "main").is_err());
        assert_eq!(receipts.take(&token, "main")?, [path]);
        assert!(receipts.take(&token, "main").is_err());
        Ok(())
    }
}
