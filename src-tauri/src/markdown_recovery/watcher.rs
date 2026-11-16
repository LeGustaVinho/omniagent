use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;

use crate::{repository, store::Store};

use super::stored_revision;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryMarkdownInvalidation {
    pub checkout_id: String,
    pub relative_path: String,
}

/// Keep this handle alive while the corresponding editable buffer is open.
pub struct RepositoryMarkdownWatch {
    _watcher: RecommendedWatcher,
}

impl Store {
    pub async fn watch_repository_markdown(
        &self,
        checkout_id: String,
        base_revision: String,
        invalidate: impl Fn(RepositoryMarkdownInvalidation) + Send + 'static,
    ) -> Result<RepositoryMarkdownWatch> {
        let revision = self
            .execute({
                let checkout_id = checkout_id.clone();
                move |db| stored_revision(db, &checkout_id, &base_revision)
            })
            .await?;
        let root = repository::checkout_root(self, checkout_id.clone()).await?;
        let layout = repository::verify_checkout(self, checkout_id.clone(), root).await?;
        let document = layout.root.join(&revision.relative_path);
        let invalidation = RepositoryMarkdownInvalidation {
            checkout_id,
            relative_path: revision.relative_path,
        };
        tokio::task::spawn_blocking(move || {
            RepositoryMarkdownWatch::new(layout.root, document, invalidation, invalidate)
        })
        .await?
    }
}

impl RepositoryMarkdownWatch {
    fn new(
        root: PathBuf,
        document: PathBuf,
        invalidation: RepositoryMarkdownInvalidation,
        invalidate: impl Fn(RepositoryMarkdownInvalidation) + Send + 'static,
    ) -> Result<Self> {
        let watched_paths: HashSet<_> = dunce::simplified(&document)
            .ancestors()
            .take_while(|path| path.starts_with(dunce::simplified(&root)))
            .map(event_path_key)
            .collect();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            if event.as_ref().is_err()
                || event.is_ok_and(|event| affects_document(&event, &watched_paths))
            {
                invalidate(invalidation.clone());
            }
        })
        .context("Start watching the repository Markdown document")?;
        // A checkout watch also covers deletion and recreation of an ancestor of the note.
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .context("Watch the Markdown document's checkout")?;
        Ok(Self { _watcher: watcher })
    }
}

fn affects_document(event: &Event, watched_paths: &HashSet<String>) -> bool {
    event.need_rescan()
        || (!event.kind.is_access()
            && event
                .paths
                .iter()
                .any(|path| watched_paths.contains(&event_path_key(path))))
}

fn event_path_key(path: &Path) -> String {
    dunce::simplified(path).to_string_lossy().to_lowercase()
}
