use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
    params,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    backup::{BackupEntry, BackupManifest, write_manifest},
    protection,
    store::{DraftKey, Store},
};

use super::*;

const OLD_DRAFT: &[u8] = b"protected draft committed through the original WAL";

#[tokio::test]
async fn validates_and_migrates_a_wal_checkpoint_before_selecting_a_complete_generation()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("installed");
    let original = open_installation(&root)?;
    let initial = Store::open(original.layout.database_path())?;
    initial
        .save_draft(draft_key(), b"live draft".to_vec())
        .await?;
    let archive = old_archive(fixture.path(), &original.identity)?;
    let preview = preview_restore(&original.identity, &archive)?;
    assert_eq!(preview.schema_version, 2);
    assert_eq!(preview.package_count, 0);
    assert!(stage_restore(&original.identity, &archive, &"0".repeat(64)).is_err());

    let receipt = stage_restore(&original.identity, &archive, &preview.manifest_sha256)?;
    assert_eq!(receipt.checkpoint_id, preview.checkpoint_id);
    assert_eq!(receipt.schema_before, 2);
    assert!(receipt.schema_after > receipt.schema_before);
    assert_eq!(
        initial.load_draft(draft_key()).await?,
        Some(b"live draft".to_vec())
    );
    assert!(restore_status(&original.identity)?.active.is_none());
    assert_eq!(
        restore_status(&original.identity)?.pending,
        Some(receipt.clone())
    );
    assert!(stage_restore(&original.identity, &archive, &preview.manifest_sha256).is_err());
    let prepared_marker = fs::read(original.identity.app_data().join(PENDING))?;

    let restored = open_installation(&root)?;
    assert_eq!(restored.identity.id(), original.identity.id());
    assert_eq!(restored.restore.active, Some(receipt.clone()));
    assert!(restored.restore.pending.is_none() && restored.restore.rejection.is_none());
    assert_ne!(
        restored.layout.database_path(),
        original.layout.database_path()
    );
    assert_eq!(
        restored.layout.database_path().parent(),
        restored.layout.maintenance_packages().parent()
    );
    assert_eq!(restored.layout.app_data(), original.layout.app_data());
    assert_eq!(
        initial.load_draft(draft_key()).await?,
        Some(b"live draft".to_vec())
    );
    {
        let db = crate::store::open_database(&restored.layout.database_path())?;
        let encrypted: Vec<u8> = db.query_row(
            "SELECT payload FROM drafts WHERE owner_id = 'retained'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(protection::unprotect(&encrypted)?, OLD_DRAFT);
        let setting: String = db.query_row(
            "SELECT value FROM workspace_settings WHERE key = 'restore'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(setting, "committed via WAL");
        db.close().map_err(|(_, error)| error)?;
    }

    // Recreate an interrupted settlement after the pointer was published and
    // the selected generation received live changes.
    let db = Connection::open(restored.layout.database_path())?;
    db.execute(
        "UPDATE workspace_settings SET value = 'new live work' WHERE key = 'restore'",
        [],
    )?;
    db.close().map_err(|(_, error)| error)?;
    atomic_file::write_new(
        &restored.identity.app_data().join(PENDING),
        &prepared_marker,
    )?;
    let restarted = open_installation(&root)?;
    assert_eq!(restarted.restore.active, Some(receipt));
    assert!(restarted.restore.rejection.is_none());
    assert!(!record_exists(&restarted.identity, PENDING)?);
    let db = Connection::open(restarted.layout.database_path())?;
    let value: String = db.query_row(
        "SELECT value FROM workspace_settings WHERE key = 'restore'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(value, "new live work");
    Ok(())
}

#[test]
fn corrupt_manifest_and_different_installation_do_not_stage_or_change_live_data() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let installed = open_installation(&fixture.path().join("installed"))?;
    let foreign = open_installation(&fixture.path().join("other-installation"))?;
    let archive = old_archive(fixture.path(), &installed.identity)?;
    let preview = preview_restore(&installed.identity, &archive)?;
    assert!(preview_restore(&foreign.identity, &archive).is_err());
    assert!(stage_restore(&foreign.identity, &archive, &preview.manifest_sha256).is_err());
    assert!(restore_status(&foreign.identity)?.pending.is_none());

    let path = archive.join("manifest.json");
    let mut manifest = fs::read(&path)?;
    manifest.push(b' ');
    fs::write(&path, manifest)?;
    assert!(stage_restore(&installed.identity, &archive, &preview.manifest_sha256).is_err());
    assert!(restore_status(&installed.identity)?.pending.is_none());
    assert_eq!(
        open_installation(installed.layout.app_data())?
            .layout
            .database_path(),
        installed.layout.database_path()
    );
    Ok(())
}

#[test]
fn startup_rejects_a_different_schema_and_an_invalid_package_layout_before_publishing() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let installed = open_installation(&fixture.path().join("installed"))?;
    let archive = old_archive(fixture.path(), &installed.identity)?;
    let preview = preview_restore(&installed.identity, &archive)?;
    stage_restore(&installed.identity, &archive, &preview.manifest_sha256)?;
    let mut pending = read_record::<PendingRestore>(&installed.identity, PENDING)?.unwrap();
    pending.receipt.schema_after += 1;
    replace_record(&installed.identity, PENDING, &pending)?;

    let restarted = open_installation(installed.layout.app_data())?;
    assert_eq!(
        restarted.layout.database_path(),
        installed.layout.database_path()
    );
    assert!(restarted.restore.active.is_none());
    assert!(
        restarted
            .restore
            .rejection
            .as_deref()
            .is_some_and(|reason| reason.contains("Preview and stage this backup again"))
    );

    let retry = stage_restore(&installed.identity, &archive, &preview.manifest_sha256)?;
    let bad =
        generation_path(&installed.identity, &retry.generation_id)?.join(MAINTENANCE_PACKAGES);
    fs::write(bad, b"not a package directory")?;
    let restarted = open_installation(installed.layout.app_data())?;
    assert_eq!(
        restarted.layout.database_path(),
        installed.layout.database_path()
    );
    assert!(restarted.restore.active.is_none());
    assert!(
        restarted
            .restore
            .rejection
            .as_deref()
            .is_some_and(|reason| reason.contains("private restore directory changed"))
    );
    Ok(())
}

#[tokio::test]
async fn rejects_tampered_staging_on_startup_and_retains_the_current_database() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let installed = open_installation(&fixture.path().join("installed"))?;
    let live = Store::open(installed.layout.database_path())?;
    live.save_draft(draft_key(), b"original".to_vec()).await?;
    let archive = old_archive(fixture.path(), &installed.identity)?;
    let preview = preview_restore(&installed.identity, &archive)?;
    let receipt = stage_restore(&installed.identity, &archive, &preview.manifest_sha256)?;
    let corrupt = generation_path(&installed.identity, &receipt.generation_id)?.join(DATABASE_FILE);
    fs::write(corrupt, "tampered staging")?;
    let restarted = open_installation(installed.layout.app_data())?;
    assert!(restarted.restore.active.is_none());
    assert!(restarted.restore.pending.is_none());
    assert!(
        restarted
            .restore
            .rejection
            .as_deref()
            .is_some_and(|reason| reason.contains("private restore generation"))
    );
    assert_eq!(
        restarted.layout.database_path(),
        installed.layout.database_path()
    );
    assert_eq!(
        live.load_draft(draft_key()).await?,
        Some(b"original".to_vec())
    );
    stage_restore(&installed.identity, &archive, &preview.manifest_sha256)?;
    assert!(restore_status(&installed.identity)?.pending.is_some());
    Ok(())
}

fn draft_key() -> DraftKey {
    DraftKey::Conversation("retained".into())
}

fn old_archive(directory: &Path, identity: &InstallationIdentity) -> Result<PathBuf> {
    let source = directory.join("old.sqlite");
    let writer = Connection::open(&source)?;
    writer.pragma_update(None, "journal_mode", "WAL")?;
    writer.pragma_update(None, "wal_autocheckpoint", 0)?;
    writer.execute_batch(include_str!("../../migrations/001-store.sql"))?;
    writer.execute_batch(include_str!("../../migrations/002-protected-drafts.sql"))?;
    writer.pragma_update(None, "user_version", 2)?;
    writer.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, u32>(0)
    })?;
    writer.execute(
        "INSERT INTO workspace_settings VALUES ('restore', 'committed via WAL')",
        [],
    )?;
    writer.execute(
        "INSERT INTO drafts (kind, owner_id, document, payload) VALUES ('conversation', 'retained', '', ?1)",
        params![protection::protect(OLD_DRAFT)?],
    )?;
    assert!(fs::metadata(directory.join("old.sqlite-wal"))?.len() > 0);

    let archive = directory.join("backup");
    fs::create_dir(&archive)?;
    let snapshot = archive.join(DATABASE_FILE);
    let mut destination = Connection::open(&snapshot)?;
    {
        let backup = Backup::new(&writer, &mut destination)?;
        assert!(matches!(backup.step(-1)?, StepResult::Done));
    }
    let journal: String =
        destination.pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))?;
    assert_eq!(journal, "delete");
    destination.close().map_err(|(_, error)| error)?;
    let database = fs::read(snapshot)?;
    let sha256 = format!("{:x}", Sha256::digest(&database));
    let manifest = BackupManifest {
        format_version: 1,
        installation_id: identity.id().to_owned(),
        checkpoint_id: Uuid::new_v4().to_string(),
        schema_version: 2,
        settings_revision: sha256.clone(),
        entries: vec![BackupEntry::Database {
            size: database.len() as u64,
            sha256,
        }],
    };
    write_manifest(&archive, identity, &manifest)?;
    Ok(archive)
}
