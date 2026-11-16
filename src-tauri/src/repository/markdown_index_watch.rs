use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};
use serde::Serialize;
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval_at, timeout_at},
};

use crate::store::Store;

use super::{checkout_root, verify_checkout};

#[cfg(test)]
mod tests;

const QUIET_TIME: Duration = Duration::from_millis(120);
const MAX_BATCH_DELAY: Duration = Duration::from_secs(1);
const WATCH_HEALTH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MAX_PENDING_PATHS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum IndexInvalidation {
    Paths {
        checkout_id: String,
        paths: Vec<String>,
    },
    Rebuild {
        checkout_id: String,
        reason: IndexRebuildReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexRebuildReason {
    WatchGap,
    WatchError,
    UnexpectedPath,
    StructuralChange,
    DiscoveryChanged,
    TooManyChanges,
    WatchHealthCheck,
}

/// Retain the handle while the checkout's index is subscribed to filesystem changes.
pub struct RepositoryNoteIndexWatch {
    _watcher: RecommendedWatcher,
    dispatch: JoinHandle<()>,
}

impl Drop for RepositoryNoteIndexWatch {
    fn drop(&mut self) {
        self.dispatch.abort();
    }
}

impl Store {
    pub async fn watch_repository_note_index(
        &self,
        checkout_id: String,
        invalidate: impl Fn(IndexInvalidation) + Send + 'static,
    ) -> Result<RepositoryNoteIndexWatch> {
        let root = checkout_root(self, checkout_id.clone()).await?;
        let layout = verify_checkout(self, checkout_id.clone(), root).await?;
        let root = layout.root;
        let pending = Arc::new(Mutex::new(PendingChanges::default()));
        let (changes, receiver) = watch::channel(());
        let watcher = tokio::task::spawn_blocking({
            let pending = pending.clone();
            move || start_watcher(root, pending, changes)
        })
        .await??;
        let dispatch = tokio::spawn(dispatch_changes(
            checkout_id,
            pending,
            receiver,
            WATCH_HEALTH_INTERVAL,
            invalidate,
        ));
        Ok(RepositoryNoteIndexWatch {
            _watcher: watcher,
            dispatch,
        })
    }
}

fn start_watcher(
    root: PathBuf,
    pending: Arc<Mutex<PendingChanges>>,
    changes: watch::Sender<()>,
) -> Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher({
        let root = root.clone();
        move |result| {
            if let Some(hint) = classify_event(&root, result) {
                pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .record(hint);
                changes.send_replace(());
            }
        }
    })
    .context("Start watching the repository note index")?;
    watcher
        .watch(&root, RecursiveMode::Recursive)
        .context("Watch the note index's checkout")?;
    Ok(watcher)
}

async fn dispatch_changes(
    checkout_id: String,
    pending: Arc<Mutex<PendingChanges>>,
    mut changes: watch::Receiver<()>,
    watch_health_interval: Duration,
    invalidate: impl Fn(IndexInvalidation),
) {
    // A periodic reconciliation also covers events the filesystem backend dropped silently.
    let mut reconcile = interval_at(
        Instant::now() + watch_health_interval,
        watch_health_interval,
    );
    reconcile.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let periodic = tokio::select! {
            result = changes.changed() => {
                if result.is_err() {
                    return;
                }
                let limit = Instant::now() + MAX_BATCH_DELAY;
                while Instant::now() < limit {
                    let until = (Instant::now() + QUIET_TIME).min(limit);
                    if !matches!(timeout_at(until, changes.changed()).await, Ok(Ok(()))) {
                        break;
                    }
                }
                false
            }
            _ = reconcile.tick() => true,
        };
        let batch = {
            let mut pending = pending.lock().unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut *pending)
        };
        if let Some(reason) = batch
            .rebuild
            .or(periodic.then_some(IndexRebuildReason::WatchHealthCheck))
        {
            invalidate(IndexInvalidation::Rebuild {
                checkout_id: checkout_id.clone(),
                reason,
            });
        } else if !batch.paths.is_empty() {
            invalidate(IndexInvalidation::Paths {
                checkout_id: checkout_id.clone(),
                paths: batch.paths.into_iter().collect(),
            });
        }
    }
}

#[derive(Default)]
struct PendingChanges {
    paths: BTreeSet<String>,
    rebuild: Option<IndexRebuildReason>,
}

impl PendingChanges {
    fn record(&mut self, hint: Hint) {
        if self.rebuild.is_some() {
            return;
        }
        match hint {
            Hint::Paths(paths) => {
                self.paths.extend(paths);
                if self.paths.len() > MAX_PENDING_PATHS {
                    self.rebuild = Some(IndexRebuildReason::TooManyChanges);
                }
            }
            Hint::Rebuild(reason) => self.rebuild = Some(reason),
        }
        if self.rebuild.is_some() {
            self.paths.clear();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Hint {
    Paths(Vec<String>),
    Rebuild(IndexRebuildReason),
}

fn classify_event(root: &Path, result: notify::Result<Event>) -> Option<Hint> {
    use IndexRebuildReason as Rebuild;

    let Ok(event) = result else {
        return Some(Hint::Rebuild(Rebuild::WatchError));
    };
    if event.need_rescan() || (!event.kind.is_access() && event.paths.is_empty()) {
        return Some(Hint::Rebuild(Rebuild::WatchGap));
    }
    let mut notes = Vec::new();
    for path in &event.paths {
        let Ok(relative) = dunce::simplified(path).strip_prefix(dunce::simplified(root)) else {
            return Some(Hint::Rebuild(Rebuild::UnexpectedPath));
        };
        let Some(name) = relative.to_str().filter(|name| {
            !name.contains(':')
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
        }) else {
            return Some(Hint::Rebuild(Rebuild::UnexpectedPath));
        };
        if event.kind.is_access() {
            continue;
        }
        let name = name.replace('\\', "/");
        if discovery_changed(relative, &name) {
            return Some(Hint::Rebuild(Rebuild::DiscoveryChanged));
        }
        if relative
            .components()
            .any(|component| component.as_os_str().eq_ignore_ascii_case(".git"))
            || relative
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(".omniagent-markdown-")
                        && [".pending", ".edited", ".displaced"]
                            .iter()
                            .any(|suffix| name.ends_with(suffix))
                })
        {
            continue;
        }
        if name.is_empty()
            || matches!(
                event.kind,
                EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
            )
        {
            return Some(Hint::Rebuild(Rebuild::StructuralChange));
        }
        if relative.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        }) {
            notes.push(name);
        } else if matches!(
            event.kind,
            EventKind::Create(CreateKind::Any | CreateKind::Other)
                | EventKind::Remove(RemoveKind::Any | RemoveKind::Other)
                | EventKind::Modify(ModifyKind::Name(_))
                | EventKind::Any
                | EventKind::Other
        ) {
            // Windows does not distinguish files from directories for these events.
            return Some(Hint::Rebuild(Rebuild::StructuralChange));
        }
    }
    (!notes.is_empty()).then_some(Hint::Paths(notes))
}

fn discovery_changed(relative: &Path, name: &str) -> bool {
    relative
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(".gitignore"))
        || name.eq_ignore_ascii_case(".git")
        || name.eq_ignore_ascii_case(".git/info/exclude")
}
