use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;
use uuid::Uuid;

use crate::{customizations, providers::configuration::plain_metadata, store::Store};

use super::{
    Provision, Source, add_linked_worktree, base_commit, branch_head, check, check_branch,
    git_command, git_path, git_query, inspect, materialize, normalize_root, persist_checkout,
    repository_gate, validate_source,
};
use crate::repository::{
    ProjectCheckout,
    documents::lock_directories,
    git_review::{branch_reference, validate_oid},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualGitWorktree {
    pub branch: String,
    pub existing: bool,
    pub base_reference: Option<String>,
    pub history_reference: Option<String>,
    pub expected_head: String,
    pub path: Option<PathBuf>,
}

pub async fn create_git_worktree(
    store: &Store,
    default_root: PathBuf,
    project: String,
    checkout: String,
    request: ManualGitWorktree,
) -> Result<ProjectCheckout> {
    let source = store
        .execute({
            let project = project.clone();
            move |db| {
                db.query_row(
                    "SELECT c.root, c.git_dir, p.git_common_dir, r.root
                 FROM checkouts c JOIN projects p ON p.id = c.project_id
                 LEFT JOIN project_worktree_roots r ON r.project_id = p.id
                 WHERE c.id = ?1 AND c.project_id = ?2",
                    params![checkout, project],
                    |row| {
                        Ok(Source {
                            root: PathBuf::from(row.get::<_, String>(0)?),
                            git_dir: row.get(1)?,
                            common: row.get(2)?,
                            override_root: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
                        })
                    },
                )
                .optional()?
                .context("the source checkout does not belong to this project")
            }
        })
        .await?;
    let _gate = repository_gate(&source.common).lock_owned().await;
    let (source, provision, existing, created, inventory, _directories) =
        tokio::task::spawn_blocking({
            let project = project.clone();
            move || {
                let git = git_path()?;
                let provision = plan(&git, &source, &project, &default_root, &request)?;
                let _directories =
                    lock_directories(provision.root.parent().context("missing worktree parent")?)?;
                ensure!(
                    plain_metadata(&provision.root)?.is_none(),
                    "the requested worktree path is already occupied"
                );
                validate_source(&source)?;
                add_linked_worktree(&git, &source, &provision, request.existing)?;
                let prepared = (|| {
                    let created = materialize(&git, &source, &provision)?;
                    let inventory = customizations::inspect(&provision.root, &provision.root)?;
                    Ok((created, inventory))
                })();
                let (created, inventory) = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        return Err(with_rollback(error, &source, &provision, request.existing));
                    }
                };
                Ok::<_, anyhow::Error>((
                    source,
                    provision,
                    request.existing,
                    created,
                    inventory,
                    _directories,
                ))
            }
        })
        .await??;
    let saved = store
        .execute(move |db| {
            let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let checkout = persist_checkout(&transaction, created, &inventory)?;
            ensure!(
                checkout.project_id == project,
                "the created worktree belongs to another project"
            );
            transaction.commit()?;
            Ok(checkout)
        })
        .await;
    match saved {
        Ok(saved) => Ok(saved),
        Err(error) => Err(tokio::task::spawn_blocking(move || {
            with_rollback(error, &source, &provision, existing)
        })
        .await?),
    }
}

fn plan(
    git: &Path,
    source: &Source,
    project: &str,
    default_root: &Path,
    request: &ManualGitWorktree,
) -> Result<Provision> {
    validate_source(source)?;
    validate_oid(&request.expected_head)?;
    let branch = request.branch.trim();
    ensure!(
        !branch.is_empty() && !branch.starts_with('-'),
        "choose a valid local branch name"
    );
    git_query(
        git,
        &source.root,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    )
    .context("choose a valid local branch name")?;
    let existing = branch_head(git, &source.root, branch)?;
    let base = request.base_reference.as_deref();
    ensure!(
        request.history_reference.is_none()
            || (!request.existing && base.is_some_and(|revision| validate_oid(revision).is_ok())),
        "a history reference requires an exact starting commit for a new branch"
    );
    let head = if request.existing {
        ensure!(
            request.base_reference.is_none(),
            "an existing branch already has a starting revision"
        );
        existing.context("the selected local branch no longer exists")?
    } else {
        ensure!(existing.is_none(), "this local branch already exists");
        match base {
            Some(reference) if validate_oid(reference).is_ok() => {
                let tip = match request.history_reference.as_deref() {
                    Some(history) => branch_reference(&source.root, history)?.sha,
                    None => base_commit(&source.root)?
                        .context("the source checkout has no base commit")?,
                };
                ensure!(
                    git_query(git, &source.root, &["cat-file", "-t", reference])?.trim()
                        == "commit",
                    "select an exact Git commit as the starting revision"
                );
                let output = git_command(git, &source.root)
                    .args(["merge-base", "--is-ancestor", reference, &tip])
                    .output()?;
                ensure!(
                    output.status.success(),
                    "the selected commit is not in the chosen starting history"
                );
                reference.to_owned()
            }
            Some(reference) => branch_reference(&source.root, reference)?.sha,
            None => base_commit(&source.root)?.context("the source checkout has no base commit")?,
        }
    };
    ensure!(
        head == request.expected_head,
        "the starting revision changed; refresh the worktrees and try again"
    );
    let root = destination(source, project, default_root, request.path.as_deref())?;
    Ok(Provision {
        root,
        branch: branch.into(),
        base_commit: head,
    })
}

fn destination(
    source: &Source,
    project: &str,
    default_root: &Path,
    requested: Option<&Path>,
) -> Result<PathBuf> {
    let path = if let Some(path) = requested {
        normalize_root(path)?
    } else {
        normalize_root(source.override_root.as_deref().unwrap_or(default_root))?
            .join(Uuid::parse_str(project)?.to_string())
            .join(Uuid::new_v4().to_string())
    };
    let name = path
        .file_name()
        .context("choose a directory name for the new worktree")?;
    let parent = path
        .parent()
        .context("choose a worktree path with a parent directory")?;
    validate_outside_worktrees(source, &path)?;
    if requested.is_none() {
        fs::create_dir_all(parent)?;
    }
    ensure!(
        plain_metadata(parent)?.is_some_and(|info| info.is_dir()),
        "the worktree parent directory must exist"
    );
    let root = dunce::canonicalize(parent)?.join(name);
    validate_outside_worktrees(source, &root)?;
    ensure!(
        plain_metadata(&root)?.is_none(),
        "the requested worktree path is already occupied"
    );
    Ok(root)
}

fn validate_outside_worktrees(source: &Source, path: &Path) -> Result<()> {
    ensure!(
        !path.starts_with(&source.common),
        "choose a path outside the repository metadata"
    );
    let entries = git_query(
        &git_path()?,
        &source.root,
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    for entry in entries
        .split('\0')
        .filter_map(|field| field.strip_prefix("worktree "))
    {
        let worktree = dunce::canonicalize(entry).unwrap_or_else(|_| PathBuf::from(entry));
        ensure!(
            !path.starts_with(worktree),
            "choose a path outside the existing repository worktrees"
        );
    }
    Ok(())
}

fn with_rollback(
    error: anyhow::Error,
    source: &Source,
    provision: &Provision,
    existing: bool,
) -> anyhow::Error {
    match rollback(source, provision, existing) {
        Ok(()) => error,
        Err(cleanup) => error.context(format!(
            "review the new worktree at {} because rollback could not finish: {cleanup:#}",
            provision.root.display()
        )),
    }
}

fn rollback(source: &Source, provision: &Provision, existing: bool) -> Result<()> {
    let git = git_path()?;
    validate_source(source)?;
    let checkout = inspect(&provision.root)?;
    ensure!(
        Path::new(&checkout.checkout_root) == provision.root
            && checkout.git_common_dir == source.common
            && checkout.git_dir != checkout.git_common_dir,
        "the new checkout identity changed"
    );
    check_branch(&provision.root, &git, &provision.branch)?;
    ensure!(
        base_commit(&provision.root)?.as_deref() == Some(&provision.base_commit),
        "the new checkout revision changed"
    );
    let mut command = git_command(&git, &source.root);
    let empty_hooks = tempfile::tempdir()?;
    let hooks = format!("core.hooksPath={}", empty_hooks.path().display());
    command.arg("-c").arg(&hooks).args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "maintenance.auto=false",
        "-c",
        "gc.auto=0",
    ]);
    let filters = git_command(&git, &provision.root)
        .args([
            "config",
            "--name-only",
            "-z",
            "--get-regexp",
            "^filter[.].*[.](clean|smudge|process|required)$",
        ])
        .output()?;
    if filters.status.code() != Some(1) {
        for key in
            String::from_utf8(check(filters, "read existing Git filters")?)?.split_terminator('\0')
        {
            let value = if key.ends_with(".required") {
                "false"
            } else {
                ""
            };
            command.arg("-c").arg(format!("{key}={value}"));
        }
    }
    check(
        command
            .args(["worktree", "remove", "--"])
            .arg(&provision.root)
            .output()?,
        "remove the incomplete worktree",
    )?;
    if !existing {
        let reference = format!("refs/heads/{}", provision.branch);
        check(
            git_command(&git, &source.root)
                .args([
                    "-c",
                    &hooks,
                    "update-ref",
                    "-d",
                    &reference,
                    &provision.base_commit,
                ])
                .output()?,
            "remove the incomplete worktree branch",
        )?;
    }
    Ok(())
}
