use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc as std_mpsc,
    sync::{Arc, Mutex, Weak},
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
    include_str!("../migrations/058-checklist-execution.sql"),
    include_str!("../migrations/059-checklist-progress.sql"),
    include_str!("../migrations/060-branch-strategy.sql"),
    include_str!("../migrations/061-dispatch-lineage.sql"),
    include_str!("../migrations/062-candidate-verification.sql"),
    include_str!("../migrations/063-workflow-runtime.sql"),
    include_str!("../migrations/064-verified-git.sql"),
    include_str!("../migrations/065-task-deliveries.sql"),
    include_str!("../migrations/066-subagent-audit-routing.sql"),
    include_str!("../migrations/067-attempt-job-policies.sql"),
    include_str!("../migrations/068-goal-criterion-evidence.sql"),
    include_str!("../migrations/069-workflow-structured-planning.sql"),
    include_str!("../migrations/070-generated-image-evidence.sql"),
    include_str!("../migrations/071-attachment-history-deletion.sql"),
    include_str!("../migrations/072-project-description-drafts.sql"),
    include_str!("../migrations/073-repository-note-index.sql"),
    include_str!("../migrations/074-repository-note-links.sql"),
    include_str!("../migrations/075-markdown-rename-previews.sql"),
    include_str!("../migrations/076-conversation-library-activity.sql"),
    include_str!("../migrations/077-conversation-continuation.sql"),
    include_str!("../migrations/078-verified-merge.sql"),
    include_str!("../migrations/079-delivery-milestones.sql"),
    include_str!("../migrations/080-destination-drift.sql"),
    include_str!("../migrations/081-conversation-history-journal.sql"),
    include_str!("../migrations/082-conversation-references.sql"),
    include_str!("../migrations/083-agile-work-events.sql"),
    include_str!("../migrations/084-conversation-attachment-drafts.sql"),
    include_str!("../migrations/085-conversation-fork-checkouts.sql"),
    include_str!("../migrations/086-conversation-compaction.sql"),
    include_str!("../migrations/087-history-payload-deletion.sql"),
    include_str!("../migrations/088-workflow-authoring.sql"),
    include_str!("../migrations/089-native-conversation-compaction.sql"),
    include_str!("../migrations/090-model-comparisons.sql"),
    include_str!("../migrations/091-conversation-templates.sql"),
    include_str!("../migrations/092-workflow-commands.sql"),
    include_str!("../migrations/093-workflow-triggers.sql"),
    include_str!("../migrations/094-workflow-launches.sql"),
    include_str!("../migrations/095-reviewed-workflow-publication.sql"),
    include_str!("../migrations/096-comparison-synthesis.sql"),
    include_str!("../migrations/097-workflow-approval-rules.sql"),
    include_str!("../migrations/098-workflow-completion-inputs.sql"),
    include_str!("../migrations/099-workflow-control-receipts.sql"),
    include_str!("../migrations/100-workflow-authoring-packages.sql"),
    include_str!("../migrations/101-workflow-subflow-pins.sql"),
    include_str!("../migrations/102-workflow-stage-assessments.sql"),
    include_str!("../migrations/103-workflow-package-mcp.sql"),
    include_str!("../migrations/104-workflow-mcp-tools.sql"),
    include_str!("../migrations/105-claude-plugin-actions.sql"),
    include_str!("../migrations/106-workflow-iteration-artifacts.sql"),
    include_str!("../migrations/107-workflow-settings-proposals.sql"),
    include_str!("../migrations/108-workflow-maintenance.sql"),
    include_str!("../migrations/109-workflow-plan-mode.sql"),
    include_str!("../migrations/110-image-generation-qualifications.sql"),
    include_str!("../migrations/111-interrupted-writer-reconciliation.sql"),
    include_str!("../migrations/112-workflow-goal-requalification.sql"),
];
const WRITER_QUEUE_CAPACITY: usize = 64;
type DatabaseJob = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Store {
    writer: mpsc::Sender<DatabaseJob>,
    pub(crate) event_signal: watch::Sender<()>,
    pub(crate) job_signal: watch::Sender<()>,
    pub(crate) note_index_locks: Arc<Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>>,
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
            note_index_locks: Arc::default(),
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
    migrate(&mut db, MIGRATIONS.len())?;
    Ok(db)
}

fn migrate(db: &mut Connection, target: usize) -> Result<()> {
    const TABLE_REBUILDS: [usize; 2] = [30, 106];
    let installed: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for rebuild in TABLE_REBUILDS {
        if rebuild <= installed as usize || rebuild > target {
            continue;
        }
        migrate_through(db, rebuild - 1, false)?;
        db.pragma_update(None, "foreign_keys", "OFF")?;
        db.pragma_update(None, "legacy_alter_table", "ON")?;
        let rebuilt = migrate_through(db, rebuild, true);
        db.pragma_update(None, "legacy_alter_table", "OFF")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        rebuilt?;
    }
    migrate_through(db, target, false)
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
        bail!("Schema migration {target} left an invalid reference");
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iteration_migration_preserves_workflow_artifacts_and_completion_inputs() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut db, 105)?;
        db.execute_batch(
            "INSERT INTO projects (id, git_common_dir, root) VALUES ('project', 'fixture.git', 'fixture');
             INSERT INTO job_pipeline_runs (id, project_id) VALUES ('run', 'project');
             INSERT INTO job_pipeline_nodes (pipeline_run_id, instance_id, kind)
                 VALUES ('run', 'node', 'execution');
             INSERT INTO workflow_revisions VALUES ('project', 'workflow', 'revision', x'00');
             INSERT INTO workflow_run_pins VALUES ('run', 'project', 'workflow', 'revision');
             INSERT INTO workflow_node_pins
                 (pipeline_run_id, node_instance_id, instance_revision, definition_revision, requires_plan)
                 VALUES ('run', 'node', 'pinned', 1, 0);
             INSERT INTO workflow_artifacts VALUES ('saved', 'run', 'node', 'result', NULL, 'plan', x'0102');
             INSERT INTO workflow_host_completions VALUES ('run', 'node', 'execution', 'request', 'saved');
             INSERT INTO workflow_completion_inputs VALUES ('saved', 'execution', 'input', x'0304');",
        )?;

        migrate(&mut db, 106)?;
        let evidence: (Vec<u8>, Vec<u8>) = db.query_row(
            "SELECT a.payload, i.payload FROM workflow_host_completions h
             JOIN workflow_artifacts a ON a.id = h.artifact_id
             JOIN workflow_completion_inputs i ON i.output_artifact_id = a.id
             WHERE h.pipeline_run_id = 'run' AND i.input_revision = 'input'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(evidence, (vec![1, 2], vec![3, 4]));
        assert!(!db.prepare("PRAGMA foreign_key_check")?.exists([])?);
        assert_eq!(db.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i32>(0))?, 1);
        db.execute(
            "INSERT INTO workflow_artifacts VALUES ('items', 'run', 'node', 'items', NULL, 'json_list', x'05')",
            [],
        )?;
        Ok(())
    }

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
