use std::sync::{Arc, Mutex};

use omniagent::{
    repository::file_links::{FileLink, ResolvedFileLink},
    store::Store,
};
use tauri::{AppHandle, Emitter, State, WebviewWindow};

#[derive(Clone, Default)]
pub struct PendingFileLink(Arc<Mutex<Option<FileLink>>>);

impl PendingFileLink {
    pub fn enqueue(&self, arguments: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
        let mut arguments = arguments.into_iter();
        let Some(first) = arguments.next() else {
            return false;
        };
        if arguments.next().is_some() {
            return false;
        }
        let Some(link) = FileLink::parse(first.as_ref()) else {
            return false;
        };
        *self.0.lock().expect("file link inbox poisoned") = Some(link);
        true
    }

    pub fn forward(&self, app: &AppHandle, arguments: Vec<String>) {
        if self.enqueue(arguments.into_iter().skip(1)) {
            let _ = app.emit_to("main", "omniagent-file-link", ());
        }
    }
}

#[tauri::command]
pub async fn take_pending_file_link(
    window: WebviewWindow,
    state: State<'_, PendingFileLink>,
    store: State<'_, Store>,
) -> Result<Option<ResolvedFileLink>, String> {
    if window.label() != "main" {
        return Err("File links open in the main window.".into());
    }
    let link = state.0.lock().expect("file link inbox poisoned").take();
    match link {
        Some(link) => link
            .resolve(&store)
            .await
            .map(Some)
            .map_err(|_| "The linked file or checkout is unavailable.".into()),
        None => Ok(None),
    }
}
