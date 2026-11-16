use std::{
    env,
    ffi::OsStr,
    io,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::{customizations, store::Store};

mod candidates;
pub mod documents;
mod isolated_git;
mod markdown;
mod markdown_catalog;
pub mod markdown_index;
pub mod verification;
pub mod verified_git;
mod worktrees;
pub(crate) mod writers;
pub use candidates::{AttemptWorktreeEvidence, CandidateRevision, GitChanges, LiveWorktree};
pub(crate) use candidates::{
    WorktreeCheckpointMismatch, save_reconciled_native_candidate, save_worktree_candidate,
};
pub(crate) use markdown::bind_job_markdown;
pub use markdown::{RepositoryMarkdown, RepositoryMarkdownRevision};
pub use markdown_catalog::RepositoryNoteCatalogEntry;
pub use worktrees::strategy::{BranchStrategy, TaskBranchStrategy};
pub use worktrees::{GitStartingState, ManagedWorktree, WorktreeStart, provision_task_worktree};
pub(crate) use worktrees::{
    capture_worktree_start, validate_task_worktree_checkout, verify_managed_recovery_candidate,
    verify_managed_recovery_checkout,
};
pub use writers::with_managed_worktree_writer;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ProjectCheckout {
    pub project_id: String,
    pub repository_root: String,
    pub checkout_id: String,
    pub checkout_root: String,
}

pub async fn checkout_root(store: &Store, checkout_id: String) -> Result<PathBuf> {
    store
        .execute(move |db| {
            let path: String = db.query_row(
                "SELECT root FROM checkouts WHERE id = ?1",
                [checkout_id],
                |row| row.get(0),
            )?;
            Ok(path.into())
        })
        .await
}

#[derive(Clone)]
pub struct CheckoutLayout {
    pub root: PathBuf,
    pub primary_root: PathBuf,
    pub cwd: PathBuf,
}

pub async fn verify_checkout(
    store: &Store,
    checkout_id: String,
    cwd: PathBuf,
) -> Result<CheckoutLayout> {
    let (root, git_dir, common): (String, String, String) = store
        .execute(move |db| {
            Ok(db.query_row(
                "SELECT c.root, c.git_dir, p.git_common_dir FROM checkouts c
             JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                [checkout_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .await?;
    tokio::task::spawn_blocking(move || {
        ensure!(
            dunce::canonicalize(&cwd)? == cwd,
            "working directory changed after admission"
        );
        let observed = inspect(&cwd)?;
        if observed.checkout_root != root
            || observed.git_dir != git_dir
            || observed.git_common_dir != common
        {
            bail!("checkout identity changed after registration");
        }
        Ok(CheckoutLayout {
            root: PathBuf::from(root),
            primary_root: PathBuf::from(observed.repository_root),
            cwd,
        })
    })
    .await?
}

pub async fn register(store: &Store, directory: PathBuf) -> Result<ProjectCheckout> {
    let (repository, inventory) = tokio::task::spawn_blocking(move || {
        let cwd = dunce::canonicalize(&directory)?;
        let repository = inspect(&cwd)?;
        let inventory = customizations::inspect(Path::new(&repository.checkout_root), &cwd)?;
        Ok::<_, anyhow::Error>((repository, inventory))
    })
    .await??;
    store
        .execute(move |db| {
            let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let checkout = persist_checkout(&transaction, repository, &inventory)?;
            transaction.commit()?;
            Ok(checkout)
        })
        .await
}

fn persist_checkout(
    db: &Connection,
    repository: CheckoutPaths,
    inventory: &customizations::CustomizationInventory,
) -> Result<ProjectCheckout> {
    let project_id = db.query_row(
        "INSERT INTO projects(id, git_common_dir, root) VALUES (?1, ?2, ?3)
             ON CONFLICT(git_common_dir) DO UPDATE SET root = excluded.root RETURNING id",
        params![
            Uuid::new_v4().to_string(),
            repository.git_common_dir,
            repository.repository_root
        ],
        |row| row.get(0),
    )?;
    let checkout_id: String = db.query_row(
        "INSERT INTO checkouts(id, project_id, git_dir, root) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, git_dir) DO UPDATE SET root = excluded.root RETURNING id",
        params![
            Uuid::new_v4().to_string(),
            project_id,
            repository.git_dir,
            repository.checkout_root
        ],
        |row| row.get(0),
    )?;
    customizations::save(db, &checkout_id, inventory)?;
    Ok(ProjectCheckout {
        project_id,
        repository_root: repository.repository_root,
        checkout_id,
        checkout_root: repository.checkout_root,
    })
}

struct CheckoutPaths {
    repository_root: String,
    git_common_dir: String,
    checkout_root: String,
    git_dir: String,
}

fn inspect(directory: &Path) -> Result<CheckoutPaths> {
    let directory = dunce::canonicalize(directory).context("open selected directory")?;
    let git = git_path()?;
    let details = git_query(
        &git,
        &directory,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
    )?;
    let mut paths = details.lines();
    let checkout_root = canonical_git_path(paths.next())?;
    let git_dir = canonical_git_path(paths.next())?;
    let git_common_dir = canonical_git_path(paths.next())?;
    let worktrees = git_query(&git, &directory, &["worktree", "list", "--porcelain", "-z"])?;
    let main = worktrees
        .split('\0')
        .next()
        .and_then(|line| line.strip_prefix("worktree "));
    let repository_root = canonical_git_path(main)?;
    Ok(CheckoutPaths {
        repository_root,
        git_common_dir,
        checkout_root,
        git_dir,
    })
}

fn canonical_git_path(path: Option<&str>) -> Result<String> {
    let path = path
        .filter(|path| !path.is_empty())
        .context("Git did not return a repository path")?;
    dunce::canonicalize(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("repository path cannot be represented in UTF-8"))
}

fn git_path() -> io::Result<PathBuf> {
    let paths = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git.exe"))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Git for Windows was not found on PATH",
            )
        })
}

fn git_query(git: &Path, directory: &Path, args: &[impl AsRef<OsStr>]) -> Result<String> {
    let output = git_command(git, directory).args(args).output()?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn base_commit(directory: &Path) -> Result<Option<String>> {
    let output = git_command(&git_path()?, directory)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()?;
    if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
        return Ok(None);
    }
    ensure!(
        output.status.success(),
        "Git could not resolve the checkout HEAD"
    );
    let hash = String::from_utf8(output.stdout)?;
    let hash = hash.trim();
    ensure!(
        matches!(hash.len(), 40 | 64) && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Git returned an invalid HEAD object ID"
    );
    Ok(Some(hash.to_owned()))
}

fn git_command(git: &Path, directory: &Path) -> Command {
    let mut command = Command::new(git);
    command
        .creation_flags(CREATE_NO_WINDOW)
        // Inherited repository overrides would misidentify the selected checkout.
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_NAMESPACE")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .arg("-C")
        .arg(directory);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn linked_checkouts_share_one_stable_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("Repositório principal");
        let linked = temp.path().join("Árvore de trabalho");
        let moved = temp.path().join("Árvore renomeada");
        let nested = linked.join("docs");
        fs::create_dir(&root)?;
        let git = git_path()?;
        git_query(&git, &root, &["init", "--quiet"])?;
        assert_eq!(base_commit(&root)?, None);
        git_query(
            &git,
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                "Fixture",
            ],
        )?;
        git_query(
            &git,
            &root,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--detach"),
                linked.as_os_str(),
                OsStr::new("HEAD"),
            ],
        )?;
        fs::create_dir(&nested)?;
        assert_eq!(base_commit(&root)?, base_commit(&linked)?);
        assert!(base_commit(&root)?.is_some());

        let store = Store::open(temp.path().join("db.sqlite"))?;
        assert!(register(&store, temp.path().to_path_buf()).await.is_err());
        let first = register(&store, nested.clone()).await?;
        let primary = register(&store, root.clone()).await?;
        assert_eq!(first.project_id, primary.project_id);
        assert_ne!(first.checkout_id, primary.checkout_id);
        assert_eq!(first.repository_root, primary.checkout_root);
        let nested = dunce::canonicalize(nested)?;
        let layout = verify_checkout(&store, first.checkout_id.clone(), nested.clone()).await?;
        assert_eq!(layout.cwd, nested);
        assert_eq!(layout.root, PathBuf::from(&first.checkout_root));
        assert_eq!(layout.primary_root, PathBuf::from(&primary.checkout_root));
        assert!(
            verify_checkout(&store, first.checkout_id.clone(), layout.primary_root)
                .await
                .is_err()
        );

        git_query(
            &git,
            &root,
            &[
                OsStr::new("worktree"),
                OsStr::new("move"),
                linked.as_os_str(),
                moved.as_os_str(),
            ],
        )?;
        let after_move = register(&store, moved).await?;
        assert_eq!(after_move.checkout_id, first.checkout_id);
        assert_ne!(after_move.checkout_root, first.checkout_root);
        Ok(())
    }
}
