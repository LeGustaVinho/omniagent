use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use crate::{
    diagnostics,
    providers::configuration::open_locked_local_file,
    repository::verification::{
        CandidateGateAction, CandidateGateOutcome, command, materialize_maintenance_commit,
    },
};

use super::{MaintenanceBuild, MaintenanceHandoff, evidence::CandidateEvidence};

pub(super) struct BuiltPackage {
    pub version: String,
    pub sha256: String,
    pub size: u64,
    pub package_name: String,
    temporary: tempfile::NamedTempFile,
    destination: PathBuf,
}

impl BuiltPackage {
    pub(super) fn persist(self) -> Result<PathBuf> {
        self.temporary.persist_noclobber(&self.destination)?;
        Ok(self.destination)
    }
}

pub(super) fn pin_build(build: &mut MaintenanceBuild) -> Result<String> {
    ensure!(
        build.executable.is_absolute(),
        "Use an absolute path to a local build executable"
    );
    ensure!(
        build.timeout_seconds != Some(0),
        "Build timeout must be positive"
    );
    let package = output_path(&build.package_path)?;
    let version = output_path(&build.version_path)?;
    ensure!(
        !package.eq_ignore_ascii_case(&version),
        "Use distinct output paths for package and version"
    );
    build.executable = dunce::canonicalize(&build.executable)
        .context("Resolve the configured local build executable")?;
    let (_, revision) = command::pin_executable(&build.executable)
        .context("Cannot pin the configured local build executable")?;
    Ok(revision)
}

pub(super) fn build(root: &Path, id: &str, candidate: &CandidateEvidence) -> Result<BuiltPackage> {
    let checkout = materialize_maintenance_commit(
        &candidate.git_common_dir,
        &candidate.commit.sha,
        &candidate.commit.tree,
    )?;
    let source = dunce::canonicalize(checkout.path())?;
    let settings = &candidate.settings;
    let build = &settings.configuration.build;
    let relative_package = output_path(&build.package_path)?;
    let relative_version = output_path(&build.version_path)?;
    let package = source.join(&relative_package);
    let version = source.join(relative_version);
    ensure_missing(&package)?;
    ensure_missing(&version)?;
    let outcome = command::run_in_checkout(
        &source,
        &CandidateGateAction::Command {
            executable: build.executable.clone(),
            arguments: build.arguments.clone(),
            timeout_seconds: build.timeout_seconds,
        },
        Some(&settings.build_executable_revision),
    );
    match outcome {
        CandidateGateOutcome::Passed { .. } => {}
        CandidateGateOutcome::CouldNotRun { reason }
        | CandidateGateOutcome::Interrupted { reason, .. } => anyhow::bail!("{reason}"),
        CandidateGateOutcome::Failed {
            exit_code, stderr, ..
        } => anyhow::bail!(
            "Candidate build exited with code {exit_code}: {}",
            diagnostics::preview(&stderr)
        ),
    }
    let version = read_version(&version)?;
    let mut package =
        open_locked_local_file(&package).context("Open the candidate build package")?;
    fs::create_dir_all(root)?;
    let root = dunce::canonicalize(root)?;
    let mut output = tempfile::NamedTempFile::new_in(&root)?;
    let (sha256, size) = copy_digest(&mut package, &mut output)?;
    ensure!(size > 0, "The candidate package is empty");
    output.as_file().sync_all()?;
    Ok(BuiltPackage {
        version,
        sha256,
        size,
        package_name: Path::new(&relative_package)
            .file_name()
            .context("Package filename is unavailable")?
            .to_string_lossy()
            .into_owned(),
        temporary: output,
        destination: package_path(&root, id)?,
    })
}

pub(super) fn check_bytes(root: &Path, handoff: &MaintenanceHandoff) -> Result<File> {
    let root = dunce::canonicalize(root).context("The saved package directory is unavailable")?;
    let mut package = open_locked_local_file(&package_path(&root, &handoff.id)?)
        .context("The saved candidate package is unavailable")?;
    let (sha256, size) = copy_digest(&mut package, &mut io::sink())?;
    ensure!(
        sha256 == handoff.sha256 && size == handoff.size,
        "The saved candidate package does not match its handoff"
    );
    Ok(package)
}

fn package_path(root: &Path, id: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id).context("Invalid candidate handoff ID")?;
    Ok(root.join(format!("{id}.package")))
}

fn output_path(path: &str) -> Result<String> {
    let path = path.replace('\\', "/");
    ensure!(
        !path.is_empty()
            && Path::new(&path)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && path.split('/').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part
                    .chars()
                    .any(|character| character.is_control() || "<>:\"|?*".contains(character))
                && !part.eq_ignore_ascii_case(".git")),
        "Build outputs must be ordinary files inside the isolated candidate checkout"
    );
    Ok(path)
}

fn ensure_missing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("Build output already exists in the selected source revision"),
    }
}

fn read_version(path: &Path) -> Result<String> {
    let mut contents = String::new();
    open_locked_local_file(path)
        .context("Open the candidate build version output")?
        .take(1025)
        .read_to_string(&mut contents)
        .context("Read the candidate build version as UTF-8")?;
    let version = contents.trim();
    ensure!(
        contents.len() <= 1024
            && !version.is_empty()
            && version.len() <= 128
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".+_-".contains(&byte)),
        "The build must emit one version string"
    );
    Ok(version.to_owned())
}

fn copy_digest(source: &mut impl Read, destination: &mut impl Write) -> io::Result<(String, u64)> {
    let mut buffer = [0; 64 * 1024];
    let mut hash = Sha256::new();
    let mut size = 0;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok((format!("{:x}", hash.finalize()), size));
        }
        let bytes = &buffer[..count];
        destination.write_all(bytes)?;
        hash.update(bytes);
        size += count as u64;
    }
}
