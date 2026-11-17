use std::{fs, io, path::Path};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    backup::{
        BackupEntry, DATABASE_FILE, read_verified_manifest_with_digest, validate_backup_inventory,
        write_manifest,
    },
    installation::{InstallationIdentity, atomic_file},
    providers::configuration::open_locked_local_file,
    store::open_database,
};

use super::{
    GENERATIONS, PENDING, PendingRestore, RestorePreview, RestoreReceipt, active_receipt,
    generation_path, record_bytes, record_exists, require_plain_directory,
};

/// Verify a candidate backup and report the exact manifest digest for confirmation.
pub fn preview_restore(identity: &InstallationIdentity, archive: &Path) -> Result<RestorePreview> {
    identity.require_current()?;
    let (manifest, manifest_sha256) = read_verified_manifest_with_digest(archive, identity)?;
    let package_count = manifest
        .entries
        .iter()
        .filter(|entry| matches!(entry, BackupEntry::MaintenancePackage { .. }))
        .count();
    Ok(RestorePreview {
        checkpoint_id: manifest.checkpoint_id,
        schema_version: manifest.schema_version,
        manifest_sha256,
        package_count,
    })
}

/// Validate and stage the confirmed same-installation backup without touching the live Store.
pub fn stage_restore(
    identity: &InstallationIdentity,
    archive: &Path,
    expected_manifest_sha256: &str,
) -> Result<RestoreReceipt> {
    ensure!(
        !record_exists(identity, PENDING)?,
        "A restore is already staged for restart"
    );
    let previous_generation = active_receipt(identity)?.map(|receipt| receipt.generation_id);
    let archive = dunce::canonicalize(archive).context("Resolve the application backup")?;
    let (mut manifest, manifest_sha256) = read_verified_manifest_with_digest(&archive, identity)?;
    ensure!(
        manifest_sha256 == expected_manifest_sha256,
        "The backup changed after the restore preview"
    );
    let parent = identity.app_data().join(GENERATIONS);
    fs::create_dir_all(&parent)?;
    require_plain_directory(&parent)?;
    let staged = tempfile::Builder::new()
        .prefix(".preparing-")
        .tempdir_in(&parent)
        .context("Create the private restore staging directory")?;
    for entry in &manifest.entries {
        entry.copy_verified(&archive, staged.path())?;
    }
    let database = staged.path().join(DATABASE_FILE);
    require_standalone_database(&database)?;
    let schema_before = {
        let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("Open the private database snapshot")?;
        let schema = check_database(&db)?;
        ensure!(
            schema == manifest.schema_version,
            "The database schema differs from its backup manifest"
        );
        validate_backup_inventory(&db, &manifest)?;
        schema
    };
    let schema_after = {
        let db = open_database(&database).context("Migrate the private restore database")?;
        let schema = check_database(&db)?;
        manifest.schema_version = schema;
        validate_backup_inventory(&db, &manifest)?;
        let busy: u32 = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        ensure!(busy == 0, "The restored database WAL did not settle");
        let journal: String =
            db.pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))?;
        ensure!(
            journal.eq_ignore_ascii_case("delete"),
            "Could not consolidate the private restore database"
        );
        db.close().map_err(|(_, error)| error)?;
        schema
    };
    require_standalone_database(&database)?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&database)?
        .sync_all()?;
    let (database_size, database_sha256) = database_digest(&database)?;
    let entry = manifest
        .entries
        .iter_mut()
        .find(|entry| matches!(entry, BackupEntry::Database { .. }))
        .context("The backup is missing its database")?;
    *entry = BackupEntry::Database {
        size: database_size,
        sha256: database_sha256.clone(),
    };
    manifest.settings_revision.clone_from(&database_sha256);
    write_manifest(staged.path(), identity, &manifest)
        .context("Seal the verified private restore generation")?;

    let receipt = RestoreReceipt {
        generation_id: Uuid::new_v4().to_string(),
        checkpoint_id: manifest.checkpoint_id,
        schema_before,
        schema_after,
    };
    let marker = PendingRestore {
        receipt: receipt.clone(),
        previous_generation,
        staged_database_sha256: database_sha256,
    };
    let target = generation_path(identity, &receipt.generation_id)?;
    let sealed = record_bytes(identity, PENDING, &marker)?;
    identity.require_current()?;
    atomic_file::publish_directory(staged.path(), &target)
        .context("Publish the private restore generation")?;
    if let Err(error) = atomic_file::write_new(&identity.app_data().join(PENDING), &sealed) {
        let _ = atomic_file::publish_directory(&target, staged.path());
        return Err(error).context("Stage the verified restore for the next restart");
    }
    let _ = staged.keep();
    Ok(receipt)
}

fn check_database(db: &Connection) -> Result<u32> {
    let integrity: String = db
        .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
        .context("Verify restored SQLite integrity")?;
    ensure!(
        integrity == "ok",
        "The restored SQLite database failed its integrity check"
    );
    ensure!(
        !db.prepare("PRAGMA foreign_key_check")?.exists([])?,
        "The restored SQLite database contains invalid references"
    );
    Ok(db.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

pub(super) fn require_standalone_database(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let component = format!("{DATABASE_FILE}{suffix}");
        match fs::symlink_metadata(path.with_file_name(component)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Check the private SQLite generation"),
            Ok(_) => anyhow::bail!("The private restore database has an unsealed SQLite sidecar"),
        }
    }
    Ok(())
}

fn database_digest(path: &Path) -> Result<(u64, String)> {
    let mut file = open_locked_local_file(path).context("Open the staged application database")?;
    let mut digest = Sha256::new();
    let size = io::copy(&mut file, &mut digest)?;
    Ok((size, format!("{:x}", digest.finalize())))
}
