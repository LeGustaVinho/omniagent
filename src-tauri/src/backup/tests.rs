use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    conversations::{AttemptPlan, SessionScope},
    dispatch::{Delivery, HistoryDeletionMode, HistoryDeletionTarget},
    jobs::{
        AdmissionEnvironment, GoalDefinition, JobContext, JobPermission, JobRole, JobRule,
        JobWorkspace, QueueState,
    },
    protection,
    providers::{Provider, SessionMode},
    repository::{self, AttemptWorktreeEvidence},
    runner::DirectWindowsRunner,
    runs::RunStatus,
    workflows::{ApprovalDecisionSource, WorkflowRevision, maintenance::MaintenanceHandoff},
};

use super::*;

const PROMPT: &str = "private original prompt 28f19";
const MARKDOWN: &str = "# Private preserved note b36e1\n";
const CHANGE: &str = "private worktree change 74dd2";

#[tokio::test]
async fn a_pinned_backup_preserves_queue_settings_and_protected_history_during_live_writes()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = dunce::canonicalize(temporary.path())?;
    let source = root.join("repository");
    fs::create_dir(&source)?;
    git(&source, &["init", "--quiet"])?;
    fs::write(source.join("note.md"), MARKDOWN)?;
    fs::write(source.join("file.txt"), "baseline\n")?;
    git(&source, &["add", "--", "note.md", "file.txt"])?;
    git(
        &source,
        &[
            "-c",
            "user.name=OmniAgent Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=NUL",
            "commit",
            "--quiet",
            "-m",
            "Baseline",
        ],
    )?;

    let live = root.join("live");
    let identity = InstallationIdentity::load_or_create(&live)?;
    let store = Store::open(live.join(DATABASE_FILE))?;
    store
        .execute(|db| {
            db.pragma_update(None, "wal_autocheckpoint", 0)?;
            Ok(())
        })
        .await?;
    let checkout = repository::register(&store, source.clone()).await?;
    let project = checkout.project_id.clone();
    let note = store
        .capture_repository_markdown(checkout.checkout_id.clone(), source.join("note.md"))
        .await?;
    let conversation = store
        .create_conversation(checkout.checkout_id.clone(), "Source".into())
        .await?;
    let mut scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "backup-fixture".into(),
        account_binding: None,
        checkout: source.clone(),
        executable: std::env::current_exe()?,
        cli_version: "1.0.0".into(),
        runner: DirectWindowsRunner::IDENTITY.into(),
        permission_revision: "fixture".into(),
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    };
    let prompt = store
        .prepare_agent_attempt(
            conversation.id.clone(),
            conversation.context_id,
            scope.clone(),
            PROMPT.into(),
        )
        .await?;
    submit_fixture_dispatch(&store, &prompt, Some(&note), PROMPT).await?;
    store
        .finish_observed_agent_attempt(
            prompt.run.id.clone(),
            RunStatus::Succeeded,
            None,
            None,
            true,
        )
        .await?;

    let queue_conversation = store
        .create_conversation(checkout.checkout_id.clone(), "Queue".into())
        .await?;
    let queue_context = JobContext {
        conversation_id: queue_conversation.id,
        context_id: queue_conversation.context_id,
    };
    let queue_scope = scope.clone();
    let worktrees = tempfile::tempdir_in(&root)?;
    let task = store
        .create_project_task(project.clone(), "Back up captured writer".into())
        .await?;
    let worktree = repository::provision_task_worktree(
        &store,
        worktrees.path().to_owned(),
        project.clone(),
        task.id.clone(),
        checkout.checkout_id.clone(),
    )
    .await?;
    store
        .set_project_job_rules(
            project.clone(),
            vec![],
            vec![JobRule {
                role: JobRole::Operator,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    scope.checkout = worktree.root.clone();
    scope.mode = SessionMode::ApprovedWrite;
    let writer_conversation = store
        .create_conversation(worktree.checkout_id.clone(), "Writer".into())
        .await?;
    store
        .start_conversation_goal(
            JobContext {
                conversation_id: writer_conversation.id.clone(),
                context_id: writer_conversation.context_id,
            },
            scope.clone(),
            Some(task.id),
            GoalDefinition {
                objective: "Edit a tracked file".into(),
                criteria: vec!["The file was edited".into()],
                max_turns: Some(1),
                max_seconds: None,
                token_budget: None,
            },
            None,
        )
        .await?;
    let environment = AdmissionEnvironment {
        runner: scope.runner,
        bridge_available: true,
    };
    let writer = store
        .claim_next_job(environment.clone())
        .await?
        .context("Claim the fixture writer")?;
    store.mark_agent_running(writer.run.id.clone()).await?;
    ensure!(repository::capture_worktree_start(&store, writer.run.id.clone()).await?);
    fs::write(worktree.root.join("file.txt"), format!("{CHANGE}\n"))?;
    repository::save_worktree_candidate(&store, writer.run.id.clone()).await?;
    store
        .finish_observed_agent_attempt(
            writer.run.id.clone(),
            RunStatus::Succeeded,
            None,
            None,
            true,
        )
        .await?;
    let diff = store
        .attempt_worktree_evidence(project.clone(), writer.run.id.clone())
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = &diff else {
        anyhow::bail!("The fixture did not capture the writer diff");
    };
    let changes = artifact
        .candidate
        .changes
        .as_ref()
        .context("The fixture did not capture unstaged changes")?;
    let patch = STANDARD.decode(&changes.unstaged_patch_base64)?;
    assert!(String::from_utf8_lossy(&patch).contains(CHANGE));
    let encoded_patch = changes.unstaged_patch_base64.clone();

    store
        .set_project_question_timeout(project.clone(), 17)
        .await?;
    store.set_job_queue_paused(true).await?;
    store
        .save_conversation_draft(
            queue_context.conversation_id.clone(),
            "Already queued".into(),
        )
        .await?;
    let first = store
        .enqueue_conversation_turn(
            queue_context.clone(),
            queue_scope.clone(),
            "Already queued".into(),
        )
        .await?;
    assert!(fs::metadata(live.join(format!("{DATABASE_FILE}-wal")))?.len() > 32);
    let packages = live.join(manifest::PACKAGES_DIRECTORY);
    let pinned = store
        .execute({
            let packages = packages.clone();
            move |writer| snapshot::pin(writer, &packages)
        })
        .await?;

    // These operations complete with the checkpoint open, before any backup pages are copied.
    store
        .save_conversation_draft(
            queue_context.conversation_id.clone(),
            "Queued after the checkpoint".into(),
        )
        .await?;
    let later = store
        .enqueue_conversation_turn(
            queue_context,
            queue_scope,
            "Queued after the checkpoint".into(),
        )
        .await?;
    store
        .set_project_question_timeout(project.clone(), 31)
        .await?;
    fs::write(source.join("note.md"), "# New contents\n")?;
    let new_note = store
        .capture_repository_markdown(checkout.checkout_id.clone(), source.join("note.md"))
        .await?;
    for target in [
        HistoryDeletionTarget::Message {
            conversation_id: conversation.id.clone(),
            message_revision_id: prompt.input[0].revision_id.clone(),
        },
        HistoryDeletionTarget::AttemptDiff {
            conversation_id: writer_conversation.id.clone(),
            run_id: writer.run.id.clone(),
        },
    ] {
        let mode = HistoryDeletionMode::WithDerivedCopies;
        let preview = store
            .preview_conversation_history_deletion(target.clone(), mode)
            .await?;
        assert!(
            preview.unsettled_run_ids.is_empty(),
            "The fixture must settle {target:?} before deletion"
        );
        store
            .confirm_conversation_history_deletion(target, mode, preview.revision)
            .await?;
    }
    assert!(
        store
            .agent_dispatch(conversation.id.clone(), prompt.run.id.clone())
            .await?
            .unwrap()
            .history_payload_deleted
    );
    assert!(matches!(
        store
            .attempt_worktree_evidence(project.clone(), writer.run.id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Unavailable { .. })
    ));
    assert_eq!(store.project_question_timeout(project.clone()).await?, 31);

    let receipt = snapshot::publish(pinned, &identity, &root)?;
    assert_eq!(
        read_verified_manifest(&receipt.root, &identity)?,
        receipt.manifest
    );
    assert!(!receipt.root.join(format!("{DATABASE_FILE}-wal")).exists());
    assert!(!receipt.root.join(format!("{DATABASE_FILE}-shm")).exists());
    let database = fs::read(receipt.root.join(DATABASE_FILE))?;
    for private in [PROMPT, MARKDOWN, &encoded_patch] {
        assert!(
            !database
                .windows(private.len())
                .any(|chunk| chunk == private.as_bytes())
        );
    }
    let staging = root.join("read-copy");
    for entry in &receipt.manifest.entries {
        entry.copy_verified(&receipt.root, &staging)?;
    }
    let staged = Connection::open(staging.join(DATABASE_FILE))?;
    validate_backup_inventory(&staged, &receipt.manifest)?;
    let integrity: String = staged.pragma_query_value(None, "integrity_check", |row| row.get(0))?;
    assert_eq!(integrity, "ok");
    drop(staged);
    let backed_up = Store::open(staging.join(DATABASE_FILE))?;
    assert!(backed_up.job_queue_settings().await?.paused);
    assert_eq!(
        backed_up.project_question_timeout(project.clone()).await?,
        17
    );
    let job = backed_up
        .inspect_job(project.clone(), first.id, environment.clone())
        .await?
        .context("The checkpoint lost a queued job")?
        .job;
    assert_eq!(job.queue_state, QueueState::Queued);
    assert!(
        backed_up
            .inspect_job(project.clone(), later.id, environment)
            .await?
            .is_none()
    );
    assert_eq!(
        backed_up
            .attempt_worktree_evidence(project.clone(), writer.run.id.clone())
            .await?,
        diff
    );
    let dispatch = backed_up
        .agent_dispatch(conversation.id.clone(), prompt.run.id.clone())
        .await?
        .context("The checkpoint lost the original dispatch")?;
    assert_eq!(dispatch.frames, [PROMPT.as_bytes()]);
    assert_eq!(dispatch.snapshot.submitted_messages[0].text, PROMPT);
    assert_eq!(dispatch.snapshot.repository_markdown.as_ref(), Some(&note));
    assert_eq!(
        backed_up
            .repository_markdown(checkout.checkout_id.clone(), note.revision.id.clone())
            .await?,
        Some(note)
    );
    assert!(
        backed_up
            .repository_markdown(checkout.checkout_id, new_note.revision.id)
            .await?
            .is_none()
    );

    let current = store
        .create_application_backup(identity.clone(), packages, root.clone())
        .await?;
    assert_ne!(
        current.manifest.settings_revision,
        receipt.manifest.settings_revision
    );
    assert_eq!(
        read_verified_manifest(&current.root, &identity)?,
        current.manifest
    );
    let current_copy = root.join("current-copy");
    for entry in &current.manifest.entries {
        entry.copy_verified(&current.root, &current_copy)?;
    }
    let current_store = Store::open(current_copy.join(DATABASE_FILE))?;
    let deleted = current_store
        .agent_dispatch(conversation.id, prompt.run.id)
        .await?
        .context("The audit dispatch must survive deletion")?;
    assert!(deleted.history_payload_deleted);
    assert!(
        deleted
            .frames
            .iter()
            .all(|frame| !String::from_utf8_lossy(frame).contains(PROMPT))
    );
    assert!(matches!(
        current_store
            .attempt_worktree_evidence(project, writer.run.id)
            .await?,
        Some(AttemptWorktreeEvidence::Unavailable { .. })
    ));
    Ok(())
}

async fn submit_fixture_dispatch(
    store: &Store,
    attempt: &AttemptPlan,
    note: Option<&repository::RepositoryMarkdown>,
    frame: &str,
) -> Result<()> {
    store.mark_agent_running(attempt.run.id.clone()).await?;
    let snapshot = serde_json::from_value(json!({
        "version": 13, "contextId": attempt.context_id, "scope": attempt.scope,
        "submittedMessages": attempt.input, "nativeHistory": {"mode": "fresh"},
        "repositoryMarkdown": note, "questionRoute": attempt.question_route,
    }))?;
    let dispatch = store
        .prepare_dispatch(
            attempt.run.id.clone(),
            snapshot,
            vec![frame.as_bytes().to_vec()],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git.exe")
        .args(args)
        .current_dir(root)
        .output()?;
    ensure!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn package_handles_pin_the_inventory_and_verification_detects_altered_bytes() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = dunce::canonicalize(temporary.path())?;
    let identity = InstallationIdentity::load_or_create(&root)?;
    let packages = root.join(manifest::PACKAGES_DIRECTORY);
    fs::create_dir(&packages)?;
    let writer = Connection::open(root.join("package-source.sqlite3"))?;
    writer.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA user_version=107;
        CREATE TABLE workflow_maintenance_handoffs(id TEXT PRIMARY KEY, receipt BLOB NOT NULL);",
    )?;
    let first = insert_package(&writer, &packages, b"initial package bytes")?;
    let pinned = snapshot::pin(&writer, &packages)?;
    writer.execute(
        "DELETE FROM workflow_maintenance_handoffs WHERE id = ?1",
        [&first.id],
    )?;
    let first_path = packages.join(manifest::package_filename(&first.id)?);
    assert!(fs::remove_file(&first_path).is_err());
    assert!(fs::write(&first_path, b"changed package bytes").is_err());
    let later = insert_package(&writer, &packages, b"second package bytes")?;

    let receipt = snapshot::publish(pinned, &identity, &root)?;
    fs::remove_file(first_path)?;
    let (manifest, digest) = read_verified_manifest_with_digest(&receipt.root, &identity)?;
    assert_eq!(manifest, receipt.manifest);
    assert_eq!(digest, receipt.manifest_sha256);
    assert_eq!(manifest.entries.len(), 2);
    let entry = &manifest.entries[1];
    assert!(
        matches!(entry, BackupEntry::MaintenancePackage { handoff_id, .. } if handoff_id == &first.id)
    );
    assert!(
        !receipt
            .root
            .join(manifest::PACKAGES_DIRECTORY)
            .join(manifest::package_filename(&later.id)?)
            .exists()
    );
    let copy = root.join("verified-copy");
    for entry in &manifest.entries {
        entry.copy_verified(&receipt.root, &copy)?;
    }
    validate_backup_inventory(&Connection::open(copy.join(DATABASE_FILE))?, &manifest)?;
    assert!(validate_backup_inventory(&writer, &manifest).is_err());
    assert_eq!(
        fs::read(copy.join(entry.relative_path()?))?,
        b"initial package bytes"
    );
    fs::write(
        receipt.root.join(entry.relative_path()?),
        b"changed package bytes",
    )?;
    assert!(read_verified_manifest(&receipt.root, &identity).is_err());
    assert!(
        entry
            .copy_verified(&receipt.root, &root.join("tampered-copy"))
            .is_err()
    );

    fs::write(
        packages.join(manifest::package_filename(&later.id)?),
        b"forged package bytes",
    )?;
    let corrupt = snapshot::pin(&writer, &packages)?;
    assert!(snapshot::publish(corrupt, &identity, &root).is_err());
    let mut published = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(!name.starts_with(".omniagent-backup-"));
        if name.starts_with("OmniAgent-backup-") {
            published.push(entry.path());
        }
    }
    assert_eq!(published, [receipt.root]);
    Ok(())
}

fn insert_package(
    db: &Connection,
    package_root: &Path,
    bytes: &[u8],
) -> Result<MaintenanceHandoff> {
    let id = Uuid::new_v4().to_string();
    fs::write(package_root.join(manifest::package_filename(&id)?), bytes)?;
    // Package pinning uses the exact authenticated receipt shape persisted by maintenance.
    let handoff = MaintenanceHandoff {
        id,
        project_id: "project".into(),
        workflow_run_id: "run".into(),
        workflow: WorkflowRevision {
            id: "workflow".into(),
            revision: "revision".into(),
        },
        node_id: "node".into(),
        git_artifact_id: "git-artifact".into(),
        task_id: "task".into(),
        unit_id: "unit".into(),
        plan_revision: "plan".into(),
        settings_revision: "settings".into(),
        version: "0.1.0".into(),
        source_revision: "source".into(),
        source_tree: "tree".into(),
        candidate_revision: "candidate".into(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size: bytes.len() as u64,
        package_name: "fixture.exe".into(),
        commit_evidence_id: "commit".into(),
        verification_id: "verification".into(),
        checks: vec![],
        approval_artifact_id: "approval".into(),
        approval_source: ApprovalDecisionSource::Operator,
        acceptance_artifact_id: "acceptance".into(),
        review_artifact_id: "review".into(),
        review_run_id: "review-run".into(),
        build_executable_revision: "build".into(),
        recorded_at: "00".into(),
        activation: None,
    };
    db.execute(
        "INSERT INTO workflow_maintenance_handoffs(id, receipt) VALUES (?1, ?2)",
        params![
            handoff.id,
            protection::protect(&serde_json::to_vec(&handoff)?)?
        ],
    )?;
    Ok(handoff)
}
