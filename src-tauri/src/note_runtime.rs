use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use omniagent::{
    diagnostics,
    markdown_recovery::{
        RepositoryMarkdownDraft, RepositoryMarkdownInvalidation, RepositoryMarkdownWatch,
    },
    repository::documents::{
        ExtractRepositoryNote, ExtractedRepositoryNote, RepositoryAttachment,
        RepositoryDocumentOpen, RepositoryDocumentSave,
    },
    repository::markdown_merge::{MarkdownMergePlan, MarkdownMergePreview},
    repository::markdown_tags::{MarkdownTagRenamePlan, MarkdownTagRenamePreview},
    store::Store,
    workflows::{RepositoryNotePlanning, WorkflowLaunchSource},
};
use tauri::{State, ipc::Channel};

fn report(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[derive(Default)]
pub struct NoteWatches(Mutex<HashMap<String, RepositoryMarkdownWatch>>);

pub struct ReviewedNotePreviews<T>(Mutex<HashMap<String, (Instant, T)>>);
pub type NoteMergePreviews = ReviewedNotePreviews<MarkdownMergePlan>;
pub type NoteTagRenamePreviews = ReviewedNotePreviews<MarkdownTagRenamePlan>;

const REVIEW_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_REVIEWS: usize = 8;

impl<T> Default for ReviewedNotePreviews<T> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<T> ReviewedNotePreviews<T> {
    fn remember(&self, id: String, plan: T) -> Result<(), String> {
        let mut previews = self.0.lock().map_err(report)?;
        previews.retain(|_, (created, _)| created.elapsed() < REVIEW_TTL);
        if previews.len() >= MAX_REVIEWS
            && let Some(oldest) = previews
                .iter()
                .min_by_key(|(_, (created, _))| *created)
                .map(|(id, _)| id.clone())
        {
            previews.remove(&oldest);
        }
        previews.insert(id, (Instant::now(), plan));
        Ok(())
    }

    fn take(&self, id: &str) -> Result<T, String> {
        self.0
            .lock()
            .map_err(report)?
            .remove(id)
            .filter(|(created, _)| created.elapsed() < REVIEW_TTL)
            .map(|(_, plan)| plan)
            .ok_or_else(|| "The preview expired. Review the changes again.".to_string())
    }
}

#[tauri::command]
pub async fn preview_markdown_merge(
    store: State<'_, Store>,
    previews: State<'_, NoteMergePreviews>,
    checkout_id: String,
    source: String,
    destination: String,
) -> Result<MarkdownMergePreview, String> {
    let plan = store
        .preview_markdown_merge(checkout_id, source, destination)
        .await
        .map_err(report)?;
    let preview = plan.preview.clone();
    previews.remember(preview.id.clone(), plan)?;
    Ok(preview)
}

#[tauri::command]
pub async fn stage_markdown_merge(
    store: State<'_, Store>,
    previews: State<'_, NoteMergePreviews>,
    checkout_id: String,
    preview_id: String,
) -> Result<Vec<RepositoryMarkdownDraft>, String> {
    let plan = previews.take(&preview_id)?;
    store
        .stage_markdown_merge(checkout_id, plan)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn preview_markdown_tag_rename(
    store: State<'_, Store>,
    previews: State<'_, NoteTagRenamePreviews>,
    checkout_id: String,
    source: String,
    destination: String,
    include_nested: bool,
) -> Result<MarkdownTagRenamePreview, String> {
    let plan = store
        .preview_markdown_tag_rename(checkout_id, source, destination, include_nested)
        .await
        .map_err(report)?;
    let preview = plan.preview.clone();
    previews.remember(preview.id.clone(), plan)?;
    Ok(preview)
}

#[tauri::command]
pub async fn stage_markdown_tag_rename(
    store: State<'_, Store>,
    previews: State<'_, NoteTagRenamePreviews>,
    checkout_id: String,
    preview_id: String,
) -> Result<Vec<RepositoryMarkdownDraft>, String> {
    let plan = previews.take(&preview_id)?;
    store
        .stage_markdown_tag_rename(checkout_id, plan)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn repository_note_planning(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    revision_id: String,
) -> Result<Vec<RepositoryNotePlanning>, String> {
    store
        .repository_note_planning(
            project_id,
            WorkflowLaunchSource {
                checkout_id,
                revision_id,
            },
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn open_repository_document(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
) -> Result<RepositoryDocumentOpen, String> {
    store
        .open_repository_document(checkout_id, path)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_repository_document(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
    expected_revision: String,
    markdown: String,
) -> Result<RepositoryDocumentSave, String> {
    store
        .save_repository_document(checkout_id, path, expected_revision, markdown)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn read_repository_attachment(
    store: State<'_, Store>,
    checkout_id: String,
    path: PathBuf,
) -> Result<Option<RepositoryAttachment>, String> {
    store
        .read_repository_attachment(checkout_id, path)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn read_repository_markdown_draft(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
) -> Result<Option<RepositoryMarkdownDraft>, String> {
    store
        .repository_markdown_draft(checkout_id, path)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn list_repository_markdown_drafts(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<String>, String> {
    store
        .repository_markdown_draft_paths(checkout_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_repository_markdown_draft(
    store: State<'_, Store>,
    checkout_id: String,
    base_revision: String,
    expected_draft_revision: Option<String>,
    markdown: String,
) -> Result<RepositoryMarkdownDraft, String> {
    store
        .save_repository_markdown_draft(
            checkout_id,
            base_revision,
            expected_draft_revision,
            markdown,
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn extract_repository_note_selection(
    store: State<'_, Store>,
    checkout_id: String,
    request: ExtractRepositoryNote,
) -> Result<ExtractedRepositoryNote, String> {
    store
        .extract_repository_note_selection(checkout_id, request)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn discard_repository_markdown_draft(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    expected_draft_revision: String,
) -> Result<bool, String> {
    store
        .discard_repository_markdown_draft(checkout_id, path, expected_draft_revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn watch_repository_note(
    store: State<'_, Store>,
    watches: State<'_, NoteWatches>,
    checkout_id: String,
    base_revision: String,
    channel: Channel<RepositoryMarkdownInvalidation>,
) -> Result<String, String> {
    let watch = store
        .watch_repository_markdown(checkout_id, base_revision, move |event| {
            let _ = channel.send(event);
        })
        .await
        .map_err(report)?;
    let id = uuid::Uuid::new_v4().to_string();
    watches.0.lock().map_err(report)?.insert(id.clone(), watch);
    Ok(id)
}

#[tauri::command]
pub fn unwatch_repository_note(
    watches: State<'_, NoteWatches>,
    watch_id: String,
) -> Result<(), String> {
    drop(watches.0.lock().map_err(report)?.remove(&watch_id));
    Ok(())
}
