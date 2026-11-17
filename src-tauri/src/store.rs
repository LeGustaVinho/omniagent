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
    include_str!("../migrations/113-workflow-signed-activation.sql"),
    include_str!("../migrations/114-native-user-plugin-actions.sql"),
    include_str!("../migrations/115-visual-pipeline-executors.sql"),
    include_str!("../migrations/116-workflow-trigger-optional-markdown.sql"),
    include_str!("../migrations/117-conversation-library-organization.sql"),
    include_str!("../migrations/118-workflow-automatic-agents.sql"),
    include_str!("../migrations/119-workflow-backlog.sql"),
    include_str!("../migrations/120-workflow-backlog-execution.sql"),
    include_str!("../migrations/121-workflow-automatic-preparations.sql"),
    include_str!("../migrations/122-workflow-test-phases.sql"),
    include_str!("../migrations/123-workflow-github.sql"),
    include_str!("../migrations/124-workflow-automatic-delivery-results.sql"),
    include_str!("../migrations/125-workflow-integrated-checks.sql"),
    include_str!("../migrations/126-workflow-unit-actions.sql"),
    include_str!("../migrations/127-workflow-github-source-sync.sql"),
    include_str!("../migrations/128-checkout-document-history.sql"),
    include_str!("../migrations/129-git-review-comments.sql"),
    include_str!("../migrations/130-workflow-delivery-attempts.sql"),
    include_str!("../migrations/131-workflow-agent-review-reuse.sql"),
    include_str!("../migrations/132-git-reviewed-hunks.sql"),
];
// Branch snapshots used some numbers for different migrations. Table names identify the schema
// already installed, regardless of the original number stored in user_version.
const BRANCH_MIGRATIONS: &[(usize, &str)] = &[
    (118, "workflow_automatic_runs"),
    (119, "workflow_backlog_epics"),
    (120, "workflow_backlog_executions"),
    (128, "checkout_document_history"),
    (129, "git_review_comments"),
    (130, "workflow_delivery_attempts"),
    (131, "workflow_agent_review_reuses"),
    (132, "git_reviewed_hunks"),
];
const WRITER_QUEUE_CAPACITY: usize = 64;
type DatabaseJob = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Store {
    writer: mpsc::Sender<DatabaseJob>,
    pub(crate) event_signal: watch::Sender<()>,
    pub(crate) job_signal: watch::Sender<()>,
    pub(crate) note_index_locks: Arc<Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>>,
    pub(crate) document_write_locks: Arc<Mutex<HashMap<(String, String), Weak<tokio::sync::Mutex<()>>>>>,
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

    pub(crate) fn read(&self, db: &Connection) -> Result<Option<Vec<u8>>> {
        let (kind, owner, document) = self.parts();
        let payload: Option<Vec<u8>> = db.query_row(
            "SELECT payload FROM drafts WHERE kind = ?1 AND owner_id = ?2 AND document = ?3",
            params![kind, owner, document],
            |row| row.get(0),
        ).optional()?;
        payload
            .map(|bytes| protection::unprotect(&bytes))
            .transpose()
            .map_err(Into::into)
    }

    pub(crate) fn write(&self, db: &Connection, contents: &[u8]) -> Result<()> {
        let (kind, owner, document) = self.parts();
        let payload = protection::protect(contents)?;
        db.execute(
            "INSERT INTO drafts(kind, owner_id, document, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(kind, owner_id, document) DO UPDATE
             SET payload = excluded.payload, updated_at = unixepoch()",
            params![kind, owner, document, payload],
        )?;
        Ok(())
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
            document_write_locks: Arc::default(),
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
        self.execute(move |db| key.write(db, &contents)).await
    }

    pub async fn load_draft(&self, key: DraftKey) -> Result<Option<Vec<u8>>> {
        self.execute(move |db| key.read(db)).await
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

pub(crate) fn current_schema_version() -> u32 {
    MIGRATIONS.len() as u32
}

pub(crate) fn open_database(path: &Path) -> Result<Connection> {
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
    crate::conversations::initialize_pipeline_schema(&mut db)?;
    Ok(db)
}

fn migrate(db: &mut Connection, target: usize) -> Result<()> {
    const TABLE_REBUILDS: [usize; 3] = [30, 106, 115];
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
    if matches!(version, 118..=120 | 128..=129) && target >= 128 {
        for &(number, table) in BRANCH_MIGRATIONS {
            if number <= version as usize && !has_table(&tx, table)? {
                tx.execute_batch(MIGRATIONS[number - 1])
                    .with_context(|| format!("reconcile branch schema migration {number}"))?;
            }
        }
    }
    for (index, script) in MIGRATIONS
        .iter()
        .take(target)
        .enumerate()
        .skip(version as usize)
    {
        let already_applied = match BRANCH_MIGRATIONS.iter().find(|(number, _)| *number == index + 1) {
            Some((_, table)) => has_table(&tx, table)?,
            None => false,
        };
        if !already_applied {
            tx.execute_batch(script)
                .with_context(|| format!("apply schema migration {}", index + 1))?;
        }
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

fn has_table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db.prepare("SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1")?.exists([name])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_history_migration_links_earlier_editor_captures_without_rewriting_bodies() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut db, 117)?;
        db.execute_batch(
            "INSERT INTO projects (id, git_common_dir, root) VALUES ('project', 'fixture.git', 'fixture');
             INSERT INTO checkouts (id, project_id, git_dir, root) VALUES ('checkout', 'project', 'fixture.git', 'fixture');",
        )?;
        let protected = protection::protect(b"prior bytes")?;
        for (id, at) in [("editor-later", 200), ("source-earlier", 100), ("workflow-input", 300)] {
            db.execute(
                "INSERT INTO repository_markdown_revisions (checkout_id, id, path, sha256, body, captured_at)
                 VALUES ('checkout', ?1, 'note.md', ?2, ?3, ?4)",
                params![id, "0".repeat(64), protected, at],
            )?;
        }
        migrate(&mut db, 128)?;
        let entries = db.prepare(
            "SELECT history.revision_id, history.origin, history.recorded_at, revision.body
             FROM checkout_document_history history JOIN repository_markdown_revisions revision
             ON revision.checkout_id = history.checkout_id AND revision.id = history.revision_id
             ORDER BY history.position",
        )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, Vec<u8>>(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(entries, vec![
            ("source-earlier".into(), "captured".into(), 100, protected.clone()),
            ("editor-later".into(), "captured".into(), 200, protected),
        ]);
        assert!(!db.prepare("PRAGMA foreign_key_check")?.exists([])?);
        Ok(())
    }

    #[test]
    fn reconciles_context_menu_database_versions_with_the_pipeline_schema() -> Result<()> {
        for version in 118..=120 {
            let mut db = Connection::open_in_memory()?;
            db.pragma_update(None, "foreign_keys", "ON")?;
            migrate(&mut db, 117)?;
            db.execute_batch(
                "INSERT INTO projects (id, git_common_dir, root) VALUES ('project', 'fixture.git', 'fixture');
                 INSERT INTO checkouts (id, project_id, git_dir, root) VALUES ('checkout', 'project', 'fixture.git', 'fixture');",
            )?;
            let protected = protection::protect(b"saved editor revision")?;
            db.execute(
                "INSERT INTO repository_markdown_revisions (checkout_id, id, path, sha256, body, captured_at)
                 VALUES ('checkout', 'editor-saved', 'note.md', ?1, ?2, 10)",
                params!["0".repeat(64), protected],
            )?;
            db.execute_batch(MIGRATIONS[127])?;
            if version >= 119 {
                db.execute_batch(MIGRATIONS[128])?;
                db.execute_batch("INSERT INTO git_review_comments (id, checkout_id, surface, path, payload)
                    VALUES ('comment', 'checkout', 'working', 'note.md', x'0102')")?;
            }
            if version >= 120 {
                db.execute_batch(MIGRATIONS[131])?;
                db.execute_batch(
                    "INSERT INTO git_reviewed_hunks VALUES ('checkout', 'comparison', 'hunk');",
                )?;
            }
            db.pragma_update(None, "user_version", version)?;
            migrate(&mut db, MIGRATIONS.len())?;
            migrate(&mut db, MIGRATIONS.len())?;
            assert!(has_table(&db, "workflow_automatic_agents")?);
            assert!(has_table(&db, "workflow_backlog_tasks")?);
            assert!(has_table(&db, "workflow_backlog_executions")?);
            assert!(has_table(&db, "workflow_agent_review_reuses")?);
            assert_eq!(db.query_row("SELECT count(*) FROM checkout_document_history WHERE revision_id = 'editor-saved'", [], |row| row.get::<_, i64>(0))?, 1);
            let body: Vec<u8> = db.query_row(
                "SELECT body FROM repository_markdown_revisions WHERE checkout_id = 'checkout' AND id = 'editor-saved'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(body, protected);
            let comment: Option<Vec<u8>> = db.query_row("SELECT payload FROM git_review_comments WHERE id = 'comment'", [], |row| row.get(0)).optional()?;
            assert_eq!(comment, (version >= 119).then_some(vec![1, 2]));
            assert_eq!(
                db.query_row("SELECT count(*) FROM git_reviewed_hunks WHERE hunk_id = 'hunk'", [], |row| row.get::<_, i64>(0))?,
                if version >= 120 { 1 } else { 0 },
            );
            assert!(!db.prepare("PRAGMA foreign_key_check")?.exists([])?);
        }
        Ok(())
    }

    #[test]
    fn reconciles_pipeline_database_versions_with_the_context_menu_schema() -> Result<()> {
        for version in 128..=129 {
            let mut db = Connection::open_in_memory()?;
            db.pragma_update(None, "foreign_keys", "ON")?;
            migrate(&mut db, 127)?;
            db.execute_batch(MIGRATIONS[129])?;
            db.execute_batch(
                "INSERT INTO projects (id, git_common_dir, root) VALUES ('project', 'fixture.git', 'fixture');
                 INSERT INTO checkouts (id, project_id, git_dir, root) VALUES ('checkout', 'project', 'fixture.git', 'fixture');
                 INSERT INTO project_tasks (project_id, id, title) VALUES ('project', 'task', 'Saved task');
                 INSERT INTO job_pipeline_runs (id, project_id) VALUES ('run', 'project');
                 INSERT INTO job_pipeline_nodes (pipeline_run_id, instance_id, kind) VALUES ('run', 'node', 'execution');
                 INSERT INTO workflow_revisions VALUES ('project', 'workflow', 'revision', x'00');
                 INSERT INTO workflow_run_pins VALUES ('run', 'project', 'workflow', 'revision');
                 INSERT INTO workflow_node_pins (pipeline_run_id, node_instance_id, instance_revision, definition_revision, requires_plan)
                     VALUES ('run', 'node', 'pinned', 1, 0);
                 INSERT INTO workflow_artifacts (id, pipeline_run_id, node_instance_id, port_name, kind, payload)
                     VALUES ('plan', 'run', 'node', 'plan', 'plan', x'01'), ('review', 'run', 'node', 'review', 'review', x'02');
                 INSERT INTO workflow_delivery_units VALUES ('unit', 'run', 'plan', 'project', 'task', 'checkout', 1, x'00', NULL);
                 INSERT INTO workflow_delivery_attempts VALUES ('run', 'plan', 'node', 'unit', 1, 1);",
            )?;
            if version == 129 {
                db.execute_batch(MIGRATIONS[130])?;
                db.execute_batch("INSERT INTO workflow_agent_review_reuses VALUES ('run', 'node', 'plan', 'unit', 'input', 'review', 'plan');")?;
            }
            db.pragma_update(None, "user_version", version)?;
            migrate(&mut db, MIGRATIONS.len())?;
            migrate(&mut db, MIGRATIONS.len())?;
            assert!(has_table(&db, "checkout_document_history")?);
            assert!(has_table(&db, "git_review_comments")?);
            assert!(has_table(&db, "git_reviewed_hunks")?);
            assert_eq!(db.query_row("SELECT attempt_number FROM workflow_delivery_attempts WHERE unit_id = 'unit'", [], |row| row.get::<_, i64>(0))?, 1);
            let reuse: Option<String> = db.query_row("SELECT output_artifact_id FROM workflow_agent_review_reuses WHERE node_instance_id = 'node'", [], |row| row.get(0)).optional()?;
            assert_eq!(reuse.as_deref(), (version == 129).then_some("review"));
            assert!(!db.prepare("PRAGMA foreign_key_check")?.exists([])?);
        }
        Ok(())
    }

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
        assert_eq!(
            db.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i32>(0))?,
            1
        );
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
