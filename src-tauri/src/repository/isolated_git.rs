use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, ensure};

use super::{CheckoutPaths, git_command, git_path, git_query};
use crate::providers::configuration::{open_locked_local_file, plain_metadata};

/// Git's object store and real worktree with an app-owned Git configuration.
/// Checkout and status may otherwise invoke configured smudge, clean or fsmonitor commands.
pub(super) struct IsolatedGit {
    scratch: tempfile::TempDir,
    git: PathBuf,
    checkout: PathBuf,
    common: PathBuf,
    passive_options: Vec<String>,
}

impl IsolatedGit {
    pub(super) fn open(paths: &CheckoutPaths, source: &Path, head: &str) -> Result<Self> {
        let mut view = Self::open_objects(paths, source, head)?;
        let git_dir = view.scratch.path().join(".git");
        for file in ["info/exclude", "info/attributes"] {
            let source_file = view.common.join(file);
            if plain_metadata(&source_file)?.is_some_and(|metadata| metadata.is_file()) {
                fs::create_dir_all(git_dir.join("info"))?;
                copy_file(&source_file, &git_dir.join(file))?;
            }
        }
        for key in [
            "core.autocrlf",
            "core.eol",
            "core.filemode",
            "core.ignorecase",
        ] {
            let output = git_command(&view.git, source)
                .args(["config", "--get", key])
                .output()?;
            if output.status.code() == Some(1) && output.stderr.is_empty() {
                continue;
            }
            let value = String::from_utf8(check(output, "read passive Git checkout settings")?)?;
            view.passive_options
                .push(format!("{key}={}", value.trim_end_matches(['\r', '\n'])));
        }
        Ok(view)
    }

    /// Use the registered object store with app-owned Git metadata and no checkout settings.
    pub(super) fn open_objects(paths: &CheckoutPaths, source: &Path, head: &str) -> Result<Self> {
        let git = git_path()?;
        let scratch = tempfile::tempdir()?;
        let format = git_query(&git, source, &["rev-parse", "--show-object-format"])?;
        let mut init = git_command(&git, scratch.path());
        clean_environment(&mut init, scratch.path());
        check(
            init.args(["init", "--quiet", "--template="])
                .arg(format!("--object-format={}", format.trim()))
                .output()?,
            "initialize isolated Git metadata",
        )?;
        let git_dir = scratch.path().join(".git");
        fs::write(git_dir.join("HEAD"), format!("{head}\n"))?;
        Ok(Self {
            scratch,
            git,
            checkout: PathBuf::from(&paths.checkout_root),
            common: PathBuf::from(&paths.git_common_dir),
            passive_options: Vec::new(),
        })
    }

    pub(super) fn copy_index(&self, paths: &CheckoutPaths) -> Result<()> {
        let git_dir = Path::new(&paths.git_dir);
        let index = git_dir.join("index");
        ensure!(
            plain_metadata(&index)?.is_some_and(|metadata| metadata.is_file()),
            "managed Git index is unavailable"
        );
        copy_file(&index, &self.scratch.path().join(".git/index"))?;
        let shared = git_query(
            &self.git,
            &self.checkout,
            &["rev-parse", "--path-format=absolute", "--shared-index-path"],
        )?;
        if !shared.trim().is_empty() {
            let path = dunce::canonicalize(shared.trim())?;
            ensure!(
                path.starts_with(git_dir) || path.starts_with(&self.common),
                "shared Git index is outside the repository"
            );
            let file = path.file_name().context("invalid shared index path")?;
            copy_file(&path, &self.scratch.path().join(".git").join(file))?;
        }
        Ok(())
    }

    pub(super) fn materialize(&self, index: &Path, head: &str) -> Result<()> {
        let output = self
            .command()
            .env("GIT_INDEX_FILE", index)
            .args(["read-tree", "-m", "-u", "--no-sparse-checkout", head])
            .output()?;
        check(output, "materialize pinned worktree").map(|_| ())
    }

    pub(super) fn read(&self, args: &[&str]) -> Result<Vec<u8>> {
        check(self.command().args(args).output()?, args[0])
    }

    pub(super) fn index_path(&self) -> PathBuf {
        self.scratch.path().join(".git/index")
    }

    pub(super) fn passive_options(&self) -> &[String] {
        &self.passive_options
    }

    pub(super) fn command(&self) -> Command {
        let mut command = git_command(&self.git, &self.checkout);
        clean_environment(&mut command, self.scratch.path());
        command
            .env("GIT_DIR", self.scratch.path().join(".git"))
            .env("GIT_WORK_TREE", &self.checkout)
            .env("GIT_OBJECT_DIRECTORY", self.common.join("objects"))
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=NUL",
                "-c",
                "core.untrackedCache=false",
            ]);
        for option in &self.passive_options {
            command.arg("-c").arg(option);
        }
        command
    }
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    let mut source = open_locked_local_file(source)?;
    let modified = source.metadata()?.modified()?;
    let mut destination = fs::File::create(destination)?;
    std::io::copy(&mut source, &mut destination)?;
    // Git uses the index mtime to detect edits that match its cached file stats.
    destination.set_times(fs::FileTimes::new().set_modified(modified))?;
    Ok(())
}

fn clean_environment(command: &mut Command, scratch: &Path) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", scratch.join("empty-config"))
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_EXTERNAL_DIFF");
}

pub(super) fn check(output: Output, operation: &str) -> Result<Vec<u8>> {
    ensure!(
        output.status.success(),
        "Git could not {operation}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
