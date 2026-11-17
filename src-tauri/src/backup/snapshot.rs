use std::{collections::BTreeMap, fs::File, io, path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use uuid::Uuid;

use crate::{
    installation::{InstallationIdentity, atomic_file},
    protection,
    providers::configuration::open_locked_local_file,
    workflows::maintenance::MaintenanceHandoff,
};

use super::{
    BackupEntry, BackupManifest, BackupReceipt, DATABASE_FILE, files,
    manifest::{package_filename, valid_sha256, write_manifest},
};

pub(super) struct PinnedBackup {
    source: Connection,
    schema_version: u32,
    packages: Vec<(BackupEntry, File)>,
}

pub(super) fn pin(writer: &Connection, managed_packages_root: &Path) -> Result<PinnedBackup> {
    let path = writer
        .path()
        .filter(|path| !path.is_empty())
        .context("The application store is not on disk")?;
    let source = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("Open the application store for backup")?;
    source.busy_timeout(Duration::from_secs(5))?;
    // Called on the Store writer while application writes are serialized.
    // Establish the WAL snapshot before the writer can accept another job.
    source.execute_batch("BEGIN DEFERRED")?;
    let _: u32 = source.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
    let schema_version = source.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let packages = package_inventory(&source)?
        .into_iter()
        .map(|(id, entry)| {
            let filename = package_filename(&id)?;
            let file = open_locked_local_file(&managed_packages_root.join(&filename))
                .with_context(|| format!("Pin managed package {filename}"))?;
            ensure!(
                file.metadata()?.len() == entry.size(),
                "Managed package {filename} changed size"
            );
            Ok((entry, file))
        })
        .collect::<Result<_>>()?;
    Ok(PinnedBackup {
        source,
        schema_version,
        packages,
    })
}

pub(super) fn publish(
    pinned: PinnedBackup,
    identity: &InstallationIdentity,
    destination_parent: &Path,
) -> Result<BackupReceipt> {
    let parent =
        dunce::canonicalize(destination_parent).context("Resolve the backup destination")?;
    let staging = tempfile::Builder::new()
        .prefix(".omniagent-backup-")
        .tempdir_in(&parent)?;
    let root = staging.path();
    let PinnedBackup {
        source,
        schema_version,
        packages,
    } = pinned;
    let database = write_database(&source, &root.join(DATABASE_FILE))?;
    drop(source);

    let mut entries = Vec::with_capacity(packages.len() + 1);
    entries.push(database);
    for (entry, file) in packages {
        files::stage_file(&entry, file, root)?;
        entries.push(entry);
    }
    let checkpoint_id = Uuid::new_v4().to_string();
    let manifest = BackupManifest::new(identity, checkpoint_id.clone(), schema_version, entries)?;
    let manifest_sha256 = write_manifest(root, identity, &manifest)?;
    let destination = parent.join(format!("OmniAgent-backup-{checkpoint_id}"));
    identity.require_current()?;
    atomic_file::publish_directory(root, &destination)
        .context("Publish the verified backup directory")?;
    let _ = staging.keep();
    Ok(BackupReceipt {
        root: destination,
        manifest,
        manifest_sha256,
    })
}

fn write_database(source: &Connection, path: &Path) -> Result<BackupEntry> {
    let mut destination = Connection::open(path).context("Create the backup database")?;
    destination.pragma_update(None, "synchronous", "FULL")?;
    {
        let backup = Backup::new(source, &mut destination)?;
        ensure!(
            matches!(backup.step(-1)?, StepResult::Done),
            "SQLite could not finish the pinned backup"
        );
    }
    let mode: String =
        destination.pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))?;
    ensure!(
        mode.eq_ignore_ascii_case("delete"),
        "The backup database is not a standalone SQLite file"
    );
    destination.close().map_err(|(_, error)| error)?;
    let (size, sha256) = files::copy_digest(&mut open_locked_local_file(path)?, &mut io::sink())?;
    Ok(BackupEntry::Database { size, sha256 })
}

pub(crate) fn validate_backup_inventory(
    database: &Connection,
    manifest: &BackupManifest,
) -> Result<()> {
    let schema_version: u32 =
        database.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        schema_version == manifest.schema_version,
        "The backup database schema differs from the manifest"
    );
    let mut expected = package_inventory(database)?;
    for entry in &manifest.entries {
        if let BackupEntry::MaintenancePackage { handoff_id, .. } = entry {
            ensure!(
                expected.remove(handoff_id).as_ref() == Some(entry),
                "The backup package differs from its stored handoff receipt"
            );
        }
    }
    ensure!(
        expected.is_empty(),
        "The backup is missing a referenced maintenance package"
    );
    Ok(())
}

fn package_inventory(database: &Connection) -> Result<BTreeMap<String, BackupEntry>> {
    let registered: bool = database.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'workflow_maintenance_handoffs')",
        [], |row| row.get(0),
    )?;
    if !registered {
        return Ok(BTreeMap::new());
    }
    let mut statement =
        database.prepare("SELECT id, receipt FROM workflow_maintenance_handoffs ORDER BY id")?;
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .map(|row| {
            let (id, receipt) = row?;
            let handoff: MaintenanceHandoff =
                serde_json::from_slice(&protection::unprotect(&receipt)?)?;
            ensure!(
                handoff.id == id && handoff.size > 0 && valid_sha256(&handoff.sha256),
                "Invalid saved maintenance package receipt"
            );
            let entry = BackupEntry::MaintenancePackage {
                handoff_id: id.clone(),
                size: handoff.size,
                sha256: handoff.sha256,
            };
            Ok((id, entry))
        })
        .collect()
}
