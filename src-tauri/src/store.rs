use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc as std_mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot, watch};

use crate::protection;

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001-store.sql"),
    include_str!("../migrations/002-protected-drafts.sql"),
    include_str!("../migrations/003-projects.sql"),
    include_str!("../migrations/004-runs.sql"),
    include_str!("../migrations/005-turn-payloads.sql"),
    include_str!("../migrations/006-workbench-layout.sql"),
    include_str!("../migrations/007-provider-executables.sql"),
    include_str!("../migrations/008-conversations.sql"),
    include_str!("../migrations/009-dispatches.sql"),
    include_str!("../migrations/010-capability-evidence.sql"),
    include_str!("../migrations/011-plugin-inventory.sql"),
    include_str!("../migrations/012-customizations.sql"),
    include_str!("../migrations/013-agent-permissions.sql"),
    include_str!("../migrations/014-permission-routes.sql"),
    include_str!("../migrations/015-message-attachments.sql"),
    include_str!("../migrations/016-project-attachment-limits.sql"),
    include_str!("../migrations/017-agent-questions.sql"),
    include_str!("../migrations/018-structured-question-continuations.sql"),
    include_str!("../migrations/019-project-tasks.sql"),
    include_str!("../migrations/020-job-queue.sql"),
    include_str!("../migrations/021-job-admission.sql"),
    include_str!("../migrations/022-job-recovery.sql"),
    include_str!("../migrations/023-managed-worktrees.sql"),
    include_str!("../migrations/024-attempt-worktree-evidence.sql"),
    include_str!("../migrations/025-worktree-writers.sql"),
    include_str!("../migrations/026-job-notices.sql"),
    include_str!("../migrations/027-job-failures.sql"),
    include_str!("../migrations/028-job-policies.sql"),
    include_str!("../migrations/029-pipeline-job-origin.sql"),
    include_str!("../migrations/030-command-jobs.sql"),
    include_str!("../migrations/031-runtime-inspection.sql"),
    include_str!("../migrations/032-downstream-job-waits.sql"),
    include_str!("../migrations/033-conversation-history-policy.sql"),
    include_str!("../migrations/034-subagent-quotas.sql"),
    include_str!("../migrations/035-native-subagent-reservations.sql"),
    include_str!("../migrations/036-subagent-audit.sql"),
    include_str!("../migrations/037-question-waits.sql"),
    include_str!("../migrations/038-question-answers.sql"),
    include_str!("../migrations/039-pending-instructions.sql"),
    include_str!("../migrations/040-conversation-history-boundaries.sql"),
    include_str!("../migrations/041-instruction-delivery-attempts.sql"),
    include_str!("../migrations/042-goal-revisions.sql"),
    include_str!("../migrations/043-native-instruction-delivery.sql"),
    include_str!("../migrations/044-goal-stop-controls.sql"),
    include_str!("../migrations/045-native-goal-turns.sql"),
    include_str!("../migrations/046-native-goal-recovery.sql"),
    include_str!("../migrations/047-goal-waits.sql"),
    include_str!("../migrations/048-native-goal-stop-reservations.sql"),
    include_str!("../migrations/049-conversation-forks.sql"),
    include_str!("../migrations/050-side-requests.sql"),
    include_str!("../migrations/051-side-validation.sql"),
    include_str!("../migrations/052-task-hierarchy.sql"),
    include_str!("../migrations/053-task-participation.sql"),
    include_str!("../migrations/054-task-descriptions.sql"),
    include_str!("../migrations/055-repository-markdown.sql"),
    include_str!("../migrations/056-context-records.sql"),
    include_str!("../migrations/057-job-role-context.sql"),
];
const WRITER_QUEUE_CAPACITY: usize = 64;
type DatabaseJob = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Store {
    writer: mpsc::Sender<DatabaseJob>,
    pub(crate) event_signal: watch::Sender<()>,
    pub(crate) job_signal: watch::Sender<()>,
}

pub enum DraftKey {
    Conversation(String),
    CheckoutFile {
        checkout_id: String,
        relative_path: String,
    },
}

impl DraftKey {
    fn parts(&self) -> (&str, &str, &str) {
        match self {
            Self::Conversation(id) => ("conversation", id, ""),
            Self::CheckoutFile {
                checkout_id,
                relative_path,
            } => ("checkout_file", checkout_id, relative_path),
        }
    }
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let (writer, mut reader) = mpsc::channel::<DatabaseJob>(WRITER_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(0);
        thread::Builder::new()
            .name("sqlite-writer".into())
            .spawn(move || match open_database(&path) {
                Ok(mut db) => {
                    if ready_tx.send(Ok(())).is_ok() {
                        while let Some(job) = reader.blocking_recv() {
                            job(&mut db);
                        }
                    }
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        ready_rx
            .recv()
            .context("database writer stopped during initialization")??;
        Ok(Self {
            writer,
            event_signal: watch::channel(()).0,
            job_signal: watch::channel(()).0,
        })
    }

    pub(crate) async fn execute<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, received) = oneshot::channel();
        self.writer
            .send(Box::new(move |db| {
                let _ = reply.send(operation(db));
            }))
            .await
            .map_err(|_| anyhow!("database writer is closed"))?;
        received
            .await
            .context("database operation did not return")?
    }

    pub async fn save_draft(&self, key: DraftKey, contents: Vec<u8>) -> Result<()> {
        self.execute(move |db| {
            let (kind, owner, document) = key.parts();
            let payload = protection::protect(&contents)?;
            db.execute(
                "INSERT INTO drafts(kind, owner_id, document, payload) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(kind, owner_id, document) DO UPDATE
                 SET payload = excluded.payload, updated_at = unixepoch()",
                params![kind, owner, document, payload],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn load_draft(&self, key: DraftKey) -> Result<Option<Vec<u8>>> {
        self.execute(move |db| {
            let (kind, owner, document) = key.parts();
            let payload: Option<Vec<u8>> = db.query_row(
                "SELECT payload FROM drafts WHERE kind = ?1 AND owner_id = ?2 AND document = ?3",
                params![kind, owner, document],
                |row| row.get(0),
            ).optional()?;
            payload
                .map(|bytes| protection::unprotect(&bytes))
                .transpose()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn delete_draft(&self, key: DraftKey) -> Result<()> {
        self.execute(move |db| {
            let (kind, owner, document) = key.parts();
            db.execute(
                "DELETE FROM drafts WHERE kind = ?1 AND owner_id = ?2 AND document = ?3",
                params![kind, owner, document],
            )?;
            Ok(())
        })
        .await
    }
}

fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut db = Connection::open(path).context("open local OmniAgent database")?;
    db.busy_timeout(Duration::from_secs(5))?;
    let mode: String = db.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        bail!("SQLite did not enable WAL");
    }
    db.pragma_update(None, "foreign_keys", "ON")?;
    // Probe the active engine and the MATCH operation before applying migrations.
    db.execute_batch(
        "CREATE VIRTUAL TABLE temp.fts_check USING fts5(body);
                      INSERT INTO fts_check(body) VALUES ('ready');",
    )?;
    let matches: i64 = db.query_row(
        "SELECT count(*) FROM temp.fts_check WHERE fts_check MATCH 'ready'",
        [],
        |row| row.get(0),
    )?;
    if matches != 1 {
        bail!("SQLite FTS5 probe did not match");
    }
    db.execute_batch("DROP TABLE temp.fts_check")?;
    migrate(&mut db)?;
    Ok(db)
}

fn migrate(db: &mut Connection) -> Result<()> {
    const COMMAND_SCHEMA: usize = 30;
    let installed: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (installed as usize) < COMMAND_SCHEMA {
        migrate_through(db, COMMAND_SCHEMA - 1, false)?;
        db.pragma_update(None, "foreign_keys", "OFF")?;
        db.pragma_update(None, "legacy_alter_table", "ON")?;
        let rebuilt = migrate_through(db, COMMAND_SCHEMA, true);
        db.pragma_update(None, "legacy_alter_table", "OFF")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        rebuilt?;
    }
    migrate_through(db, MIGRATIONS.len(), false)
}

fn migrate_through(db: &mut Connection, target: usize, check_references: bool) -> Result<()> {
    // Read the schema version after acquiring the writer lock.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version as usize > MIGRATIONS.len() {
        bail!("SQLite schema {version} is newer than this build");
    }
    for (index, script) in MIGRATIONS
        .iter()
        .take(target)
        .enumerate()
        .skip(version as usize)
    {
        tx.execute_batch(script)
            .with_context(|| format!("apply schema migration {}", index + 1))?;
        if index + 1 == 25 {
            crate::repository::writers::migrate_writer_evidence(&tx)?;
        }
        if index + 1 == 34 {
            crate::jobs::migrate_subagent_observations(&tx)?;
        }
        if index + 1 == 39 {
            crate::jobs::migrate_pending_instructions(&tx)?;
        }
        tx.pragma_update(None, "user_version", (index + 1) as u32)?;
    }
    if check_references && tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        bail!("The command-job schema migration left an invalid reference");
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clean_database_has_fts_wal_and_protected_drafts() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("state").join("omniagent.sqlite3"))?;
        let payload = "draft revision ☕".as_bytes().to_vec();
        let key = || DraftKey::CheckoutFile {
            checkout_id: "checkout".into(),
            relative_path: "notes.md".into(),
        };
        store.save_draft(key(), payload.clone()).await?;
        assert_eq!(store.load_draft(key()).await?, Some(payload.clone()));
        store
            .execute(move |db| {
                let journal: String =
                    db.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
                let keys: i64 = db.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
                let stored: Vec<u8> =
                    db.query_row("SELECT payload FROM drafts", [], |row| row.get(0))?;
                assert_eq!(journal, "wal");
                assert_eq!(keys, 1);
                assert!(
                    stored
                        .windows(payload.len())
                        .all(|window| window != payload)
                );
                Ok(())
            })
            .await?;
        store.delete_draft(key()).await?;
        assert_eq!(store.load_draft(key()).await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn previous_database_migrates_without_losing_records() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("omniagent.sqlite3");
        {
            let db = Connection::open(&path)?;
            db.execute_batch(MIGRATIONS[0])?;
            db.execute(
                "INSERT INTO workspace_settings VALUES ('layout', 'saved')",
                [],
            )?;
            db.pragma_update(None, "user_version", 1)?;
        }
        let store = Store::open(path)?;
        store
            .save_draft(DraftKey::Conversation("chat".into()), b"message".to_vec())
            .await?;
        store
            .execute(|db| {
                let value: String = db.query_row(
                    "SELECT value FROM workspace_settings WHERE key = 'layout'",
                    [],
                    |row| row.get(0),
                )?;
                let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
                assert_eq!(value, "saved");
                assert_eq!(version as usize, MIGRATIONS.len());
                Ok(())
            })
            .await
    }
}
