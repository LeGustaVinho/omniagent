use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::{
    customizations, protection,
    providers::configuration::{normalize_path, plain_metadata},
    store::Store,
};

use super::{
    CheckoutPaths, base_commit,
    candidates::{GitChanges, capture_git, read_status},
    git_command, git_path, git_query, inspect,
    isolated_git::{IsolatedGit, check},
    persist_checkout,
    writers::assign_shared_scope,
};

pub(super) mod strategy;

use strategy::{
    BranchStrategy, TaskBranchStrategy, branch_strategy_for_scope, pin_branch_strategy,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWorktree {
    pub project_id: String,
    pub owner_task_id: String,
    pub checkout_id: String,
    pub root: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStartingState {
    pub head: String,
    pub status: Vec<String>,
    pub index_digest: String,
    pub staged_diff_digest: String,
    pub unstaged_diff_digest: String,
    #[serde(default)]
    pub changes: Option<GitChanges>,
}

impl GitStartingState {
    pub(super) fn holds_scope_since(&self, starting: &Self) -> bool {
        self.head != starting.head || !self.status.is_empty() || !starting.status.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStart {
    pub worktree: ManagedWorktree,
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub starting: GitStartingState,
}

struct Source {
    root: PathBuf,
    git_dir: String,
    common: String,
    override_root: Option<PathBuf>,
}

#[derive(Clone)]
struct Provision {
    root: PathBuf,
    branch: String,
    base_commit: String,
}

struct ProvisionTarget {
    owner: String,
    task: String,
    strategy: TaskBranchStrategy,
}

fn provision_target(db: &Connection, project: &str, scope: &str) -> Result<ProvisionTarget> {
    let (task, strategy) = branch_strategy_for_scope(db, project, scope)?;
    let assigned_owner: Option<String> = db.query_row(
        "SELECT owner_task_id FROM shared_worktree_scopes WHERE project_id = ?1 AND scope_task_id = ?2",
        params![project, scope], |row| row.get(0),
    ).optional()?;
    let owner = assigned_owner.unwrap_or_else(|| match strategy.effective_strategy {
        BranchStrategy::Independent => scope.into(),
        BranchStrategy::SharedBranchPerTask => task.clone(),
    });
    Ok(ProvisionTarget {
        owner,
        task,
        strategy,
    })
}

pub(crate) fn validate_task_worktree_checkout(
    db: &Connection,
    project: &str,
    scope: &str,
    checkout: &str,
) -> Result<()> {
    let target = provision_target(db, project, scope)?;
    let (managed_checkout, provisioning): (Option<String>, bool) = db.query_row(
        "SELECT (SELECT checkout_id FROM managed_task_worktrees WHERE project_id = ?1 AND task_id = ?2),
                 EXISTS (SELECT 1 FROM worktree_provisions WHERE project_id = ?1 AND task_id = ?2)",
        params![project, target.owner], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        !provisioning,
        "finish provisioning the checklist work scope's managed worktree first"
    );
    ensure!(
        managed_checkout
            .as_deref()
            .is_none_or(|managed| managed == checkout),
        "checklist jobs use their work scope's managed worktree"
    );
    Ok(())
}

type RepositoryGates = Mutex<HashMap<String, Weak<AsyncMutex<()>>>>;
static REPOSITORY_GATES: OnceLock<RepositoryGates> = OnceLock::new();

fn repository_gate(common_dir: &str) -> Arc<AsyncMutex<()>> {
    let mut gates = REPOSITORY_GATES
        .get_or_init(Default::default)
        .lock()
        .expect("repository gate lock");
    if let Some(gate) = gates.get(common_dir).and_then(Weak::upgrade) {
        return gate;
    }
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(AsyncMutex::new(()));
    gates.insert(common_dir.into(), Arc::downgrade(&gate));
    gate
}

impl Store {
    pub async fn set_project_worktree_root(
        &self,
        project_id: String,
        root: Option<PathBuf>,
    ) -> Result<()> {
        let root = root
            .map(|path| -> Result<String> {
                let path = normalize_root(&path)?;
                path.into_os_string()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("worktree root is not Unicode"))
            })
            .transpose()?;
        self.execute(move |db| {
            if let Some(root) = root {
                db.execute(
                    "INSERT INTO project_worktree_roots VALUES (?1, ?2)
                            ON CONFLICT(project_id) DO UPDATE SET root = excluded.root",
                    params![project_id, root],
                )?;
            } else {
                db.execute(
                    "DELETE FROM project_worktree_roots WHERE project_id = ?1",
                    [&project_id],
                )?;
            }
            Ok(())
        })
        .await
    }

    pub async fn worktree_start(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<WorktreeStart>> {
        self.execute(move |db| {
            let state: Option<Vec<u8>> = db.query_row(
                "SELECT s.state FROM job_worktree_starts s JOIN job_attempts a ON a.run_id = s.run_id
                 JOIN jobs j ON j.id = a.job_id WHERE j.project_id = ?1 AND s.run_id = ?2",
                params![project, run], |row| row.get(0),
            ).optional()?;
            state.map(|state| Ok(serde_json::from_slice(&protection::unprotect(&state)?)?)).transpose()
        }).await
    }
}

pub async fn provision_task_worktree(
    store: &Store,
    default_root: PathBuf,
    project_id: String,
    task_id: String,
    source_checkout_id: String,
) -> Result<ManagedWorktree> {
    let scope = task_id;
    let (project, task) = (project_id.clone(), scope.clone());
    let source = store
        .execute(move |db| {
            db.query_row(
                "SELECT c.root, c.git_dir, p.git_common_dir, r.root
             FROM project_tasks t JOIN projects p ON p.id = t.project_id
             JOIN checkouts c ON c.project_id = p.id
             LEFT JOIN project_worktree_roots r ON r.project_id = p.id
             WHERE p.id = ?1 AND t.id = ?2 AND c.id = ?3",
                params![project, task, source_checkout_id],
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
            .context("task and source checkout must belong to the selected project")
        })
        .await?;
    let gate = repository_gate(&source.common).lock_owned().await;
    let (project, requested) = (project_id.clone(), scope.clone());
    let (target, existing) = store
        .execute(move |db| {
            let target = provision_target(db, &project, &requested)?;
            let existing = load_worktree(db, &project, &target.owner)?;
            Ok((target, existing))
        })
        .await?;
    let task_id = target.owner;
    if let Some((worktree, git_dir, common)) = existing {
        let (_gate, worktree) = tokio::task::spawn_blocking(move || {
            verify_managed(&worktree, &git_dir, &common)?;
            Ok::<_, anyhow::Error>((gate, worktree))
        })
        .await??;
        if scope != task_id {
            let checkout = worktree.checkout_id.clone();
            store
                .execute(move |db| {
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    assign_shared_scope(&tx, &project_id, &scope, &task_id, &checkout)?;
                    tx.commit()?;
                    Ok(())
                })
                .await?;
        }
        return Ok(worktree);
    }

    let (project, task) = (project_id.clone(), task_id.clone());
    let pending = store.execute(move |db| {
        Ok(db.query_row("SELECT root, branch, base_commit FROM worktree_provisions WHERE project_id = ?1 AND task_id = ?2",
            params![project, task], |row| Ok(Provision { root: PathBuf::from(row.get::<_, String>(0)?), branch: row.get(1)?, base_commit: row.get(2)? })
        ).optional()?)
    }).await?;
    let new_intent = pending.is_none();
    let (gate, source, git, provision) = tokio::task::spawn_blocking({
        let (project, task) = (project_id.clone(), task_id.clone());
        move || {
            let git = git_path()?;
            validate_source(&source)?;
            let provision = match pending {
                Some(saved) => saved,
                None => prepare_provision(&source, &git, &default_root, &project, &task)?,
            };
            Ok::<_, anyhow::Error>((gate, source, git, provision))
        }
    })
    .await??;
    if new_intent {
        let (project, task, requested, saved) = (
            project_id.clone(),
            task_id.clone(),
            scope.clone(),
            provision.clone(),
        );
        store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let current = provision_target(&tx, &project, &requested)?;
                ensure!(current.owner == task,
                    "branch strategy changed while preparing the worktree; retry provisioning");
                let recorded = tx.execute(
                    "INSERT INTO worktree_provisions SELECT ?1, ?2, ?3, ?4, ?5
                     WHERE NOT EXISTS (SELECT 1 FROM shared_worktree_scopes WHERE project_id = ?1 AND scope_task_id = ?2)",
                    params![
                        project,
                        task,
                        saved
                            .root
                            .to_str()
                            .context("worktree path is not Unicode")?,
                        saved.branch,
                        saved.base_commit
                    ],
                )?;
                ensure!(recorded == 1, "the scope already shares a managed worktree");
                pin_branch_strategy(&tx, &project, &current.task, current.strategy)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
    }

    let (gate, checkout, inventory) = tokio::task::spawn_blocking({
        let provision = provision.clone();
        move || {
            let checkout = materialize(&git, &source, &provision)?;
            let inventory = customizations::inspect(&provision.root, &provision.root)?;
            Ok::<_, anyhow::Error>((gate, checkout, inventory))
        }
    })
    .await??;
    let _gate = gate;
    store
        .execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let checkout = persist_checkout(&tx, checkout, &inventory)?;
            ensure!(
                checkout.project_id == project_id,
                "provisioned worktree belongs to a different project"
            );
            tx.execute(
                "INSERT INTO managed_task_worktrees VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    project_id,
                    task_id,
                    checkout.checkout_id,
                    provision.branch,
                    provision.base_commit
                ],
            )?;
            tx.execute(
                "DELETE FROM worktree_provisions WHERE project_id = ?1 AND task_id = ?2",
                params![project_id, task_id],
            )?;
            if scope != task_id {
                assign_shared_scope(&tx, &project_id, &scope, &task_id, &checkout.checkout_id)?;
            }
            tx.commit()?;
            Ok(ManagedWorktree {
                project_id,
                owner_task_id: task_id,
                checkout_id: checkout.checkout_id,
                root: provision.root,
                branch: provision.branch,
                base_commit: provision.base_commit,
            })
        })
        .await
}

fn validate_source(source: &Source) -> Result<()> {
    let actual = inspect(&source.root)?;
    ensure!(
        actual.git_dir == source.git_dir
            && actual.git_common_dir == source.common
            && Path::new(&actual.checkout_root) == source.root,
        "source checkout identity changed"
    );
    Ok(())
}

fn prepare_provision(
    source: &Source,
    git: &Path,
    default_root: &Path,
    project_id: &str,
    task_id: &str,
) -> Result<Provision> {
    let project_component = Uuid::parse_str(project_id)?.to_string();
    let task_component = Uuid::parse_str(task_id)?.to_string();
    let root = normalize_root(source.override_root.as_deref().unwrap_or(default_root))?;
    ensure!(
        !root.starts_with(&source.common),
        "worktree root is inside the repository metadata"
    );
    fs::create_dir_all(&root)?;
    let root = dunce::canonicalize(root)?;
    let parent = root.join(project_component);
    if plain_metadata(&parent)?.is_none() {
        fs::create_dir(&parent)?;
    }
    ensure!(
        dunce::canonicalize(&parent)?.parent() == Some(root.as_path()),
        "worktree parent is outside the configured root"
    );
    let root = parent.join(&task_component);
    let branch = format!("codex/task-{task_component}");
    ensure!(
        plain_metadata(&root)?.is_none(),
        "the requested worktree path is already occupied"
    );
    ensure!(
        branch_head(git, &source.root, &branch)?.is_none(),
        "the requested worktree branch already exists"
    );
    let base_commit =
        base_commit(&source.root)?.context("the source checkout has no base commit")?;
    Ok(Provision {
        root,
        branch,
        base_commit,
    })
}

fn normalize_root(path: &Path) -> Result<PathBuf> {
    let path =
        normalize_path(path).context("choose an absolute local path for the worktree root")?;
    if let Some(info) = plain_metadata(&path)? {
        ensure!(
            info.is_dir(),
            "the configured worktree root is not a directory"
        );
        return Ok(dunce::canonicalize(path)?);
    }
    Ok(path)
}

pub(super) fn load_worktree(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<Option<(ManagedWorktree, String, String)>> {
    Ok(db
        .query_row(
            "SELECT c.id, c.root, m.branch, m.base_commit, c.git_dir, p.git_common_dir
        FROM managed_task_worktrees m JOIN checkouts c ON c.id = m.checkout_id
        JOIN projects p ON p.id = m.project_id WHERE m.project_id = ?1 AND m.task_id = ?2",
            params![project, task],
            |row| {
                Ok((
                    ManagedWorktree {
                        project_id: project.into(),
                        owner_task_id: task.into(),
                        checkout_id: row.get(0)?,
                        root: PathBuf::from(row.get::<_, String>(1)?),
                        branch: row.get(2)?,
                        base_commit: row.get(3)?,
                    },
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?)
}

fn branch_head(git: &Path, root: &Path, branch: &str) -> Result<Option<String>> {
    let output = git_command(git, root)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}^{{commit}}"),
        ])
        .output()?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8(check(output, "resolve destination branch")?)?
            .trim()
            .into(),
    ))
}

fn materialize(git: &Path, source: &Source, provision: &Provision) -> Result<CheckoutPaths> {
    // The source may have changed while the provision intent was being saved.
    validate_source(source)?;
    let root = &provision.root;
    if plain_metadata(root)?.is_none() {
        let existing_ref = branch_head(git, &source.root, &provision.branch)?;
        let mut command = git_command(git, &source.root);
        let empty_hooks = tempfile::tempdir()?;
        command
            .arg("-c")
            .arg(format!("core.hooksPath={}", empty_hooks.path().display()))
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "maintenance.auto=false",
                "-c",
                "gc.auto=0",
                "worktree",
                "add",
                "--no-checkout",
                "--no-guess-remote",
            ]);
        if let Some(head) = existing_ref {
            ensure!(
                head == provision.base_commit,
                "the reserved branch changed before its worktree was created"
            );
            command.arg("--").arg(root).arg(&provision.branch);
        } else {
            command
                .args(["--no-track", "-b", &provision.branch, "--"])
                .arg(root)
                .arg(&provision.base_commit);
        }
        check(command.output()?, "create the managed worktree")?;
    }
    ensure!(
        plain_metadata(root)?.is_some_and(|entry| entry.is_dir()),
        "managed worktree path is unavailable"
    );
    let checkout = inspect(root)?;
    ensure!(
        Path::new(&checkout.checkout_root) == root
            && checkout.git_common_dir == source.common
            && checkout.git_dir != checkout.git_common_dir,
        "the destination is not a linked checkout of the selected repository"
    );
    check_branch(root, git, &provision.branch)?;
    ensure!(
        base_commit(root)?.as_deref() == Some(&provision.base_commit),
        "the reserved worktree moved away from its pinned base"
    );
    let git_dir = Path::new(&checkout.git_dir);
    let index = git_dir.join("index");
    let view = IsolatedGit::open(&checkout, &source.root, &provision.base_commit)?;
    if plain_metadata(&index)?.is_none() {
        ensure!(
            fs::read_dir(root)?.all(|entry| entry.is_ok_and(|entry| entry.file_name() == ".git")),
            "the unfinished worktree contains files that need review before materialization"
        );
        view.materialize(&index, &provision.base_commit)?;
    }
    view.copy_index(&checkout)?;
    ensure!(
        read_status(&view)?.is_empty(),
        "the new worktree contains unreviewed changes"
    );
    check_branch(root, git, &provision.branch)?;
    ensure!(
        base_commit(root)?.as_deref() == Some(&provision.base_commit),
        "the worktree changed during materialization"
    );
    Ok(checkout)
}

fn check_branch(root: &Path, git: &Path, branch: &str) -> Result<()> {
    let actual = git_query(git, root, &["symbolic-ref", "--quiet", "HEAD"])?;
    ensure!(
        actual.trim_end() == format!("refs/heads/{branch}"),
        "the managed worktree branch changed"
    );
    Ok(())
}

pub(super) fn verify_managed(
    worktree: &ManagedWorktree,
    git_dir: &str,
    common: &str,
) -> Result<CheckoutPaths> {
    ensure!(
        plain_metadata(&worktree.root)?.is_some_and(|entry| entry.is_dir()),
        "managed worktree path is unavailable"
    );
    let checkout = inspect(&worktree.root)?;
    ensure!(
        Path::new(&checkout.checkout_root) == worktree.root
            && checkout.git_dir == git_dir
            && checkout.git_common_dir == common,
        "the registered managed worktree identity changed"
    );
    check_branch(&worktree.root, &git_path()?, &worktree.branch)?;
    Ok(checkout)
}

pub(crate) async fn verify_managed_recovery_checkout(
    store: &Store,
    project: String,
    owner: String,
    checkout_id: String,
) -> Result<()> {
    let (worktree, git_dir, common) = store
        .execute(move |db| load_worktree(db, &project, &owner))
        .await?
        .context("The managed worktree is unavailable")?;
    ensure!(
        worktree.checkout_id == checkout_id,
        "The task's managed worktree changed"
    );
    tokio::task::spawn_blocking(move || verify_managed(&worktree, &git_dir, &common).map(|_| ()))
        .await?
}

pub(crate) async fn capture_worktree_start(store: &Store, run_id: String) -> Result<bool> {
    let identity = store.execute({
        let run = run_id.clone();
        move |db| {
            let info: Option<(String, String, bool)> = db.query_row("SELECT j.project_id, j.checkout_id, j.mode = 'approved_write'
                FROM job_attempts a JOIN jobs j ON j.id = a.job_id WHERE a.run_id = ?1", [&run],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
            let Some((project, checkout, writes)) = info else { return Ok(None) };
            let task: Option<String> = db.query_row("SELECT task_id FROM managed_task_worktrees WHERE project_id = ?1 AND checkout_id = ?2",
                params![project, checkout], |row| row.get(0)).optional()?;
            let Some(task) = task else { ensure!(!writes, "writer job lost its registered worktree"); return Ok(None) };
            if writes {
                let holder: bool = db.query_row("SELECT EXISTS (SELECT 1 FROM job_writer_reservations WHERE checkout_id = ?1 AND run_id = ?2)",
                    params![checkout, run], |row| row.get(0))?;
                ensure!(holder, "writer job does not hold its worktree reservation");
            }
            let checkpoint = if writes {
                let goal_checkpoint = db.query_row(
                    "SELECT c.evidence, c.capture_confirmed FROM goal_writer_attempts g
                     JOIN job_worktree_candidates c ON c.run_id = g.checkpoint_run_id WHERE g.run_id = ?1",
                    [&run], |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, Option<bool>>(1)?)),
                ).optional()?;
                if goal_checkpoint.is_some() {
                    goal_checkpoint
                } else {
                    db.query_row(
                    "WITH RECURSIVE lineage(run_id) AS (
                         SELECT ?1
                         UNION
                         SELECT previous.run_id FROM lineage current
                         JOIN job_attempts a ON a.run_id = current.run_id
                         JOIN job_attempts previous ON previous.run_id = a.previous_run_id
                         WHERE previous.process_launch_requested = 0
                           AND NOT EXISTS (SELECT 1 FROM structured_question_continuations
                                           WHERE continuation_run_id = current.run_id)
                     )
                     SELECT c.evidence, c.capture_confirmed
                     FROM structured_question_continuations l
                     JOIN lineage ON lineage.run_id = l.continuation_run_id
                     JOIN agent_questions q ON q.request_event_seq = l.request_event_seq
                     LEFT JOIN job_worktree_candidates c ON c.run_id = q.run_id
                     ORDER BY l.request_event_seq LIMIT 1",
                    [&run], |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, Option<bool>>(1)?)),
                    ).optional()?
                }
            } else {
                None
            };
            Ok(load_worktree(db, &project, &task)?.map(|identity| (identity, writes, checkpoint)))
        }
    }).await?;
    let Some(((worktree, git_dir, common), writes, checkpoint)) = identity else {
        return Ok(false);
    };
    let saved = tokio::task::spawn_blocking(move || {
        let paths = verify_managed(&worktree, &git_dir, &common)?;
        let starting = match checkpoint {
            Some((Some(payload), Some(true))) => {
                super::candidates::recheck_candidate(&worktree, &git_dir, &common, &payload)?
            }
            Some(_) => anyhow::bail!("The continuation has no confirmed worktree candidate"),
            None => capture_git(&worktree, &git_dir, &common, None)?.state,
        };
        Ok::<_, anyhow::Error>(WorktreeStart {
            worktree,
            repository_root: PathBuf::from(paths.repository_root),
            git_common_dir: PathBuf::from(common),
            starting,
        })
    })
    .await??;
    let state = protection::protect(&serde_json::to_vec(&saved)?)?;
    store
        .execute(move |db| {
            db.execute(
                "INSERT INTO job_worktree_starts VALUES (?1, ?2, ?3)",
                params![run_id, saved.worktree.checkout_id, state],
            )?;
            Ok(())
        })
        .await?;
    Ok(writes)
}

pub(crate) async fn verify_managed_recovery_candidate(
    store: &Store,
    project: String,
    owner: String,
    checkout_id: String,
    run: String,
) -> Result<()> {
    let (worktree, git_dir, common, candidate) = store.execute(move |db| {
        let (worktree, git_dir, common) = load_worktree(db, &project, &owner)?
            .context("The managed worktree is unavailable")?;
        ensure!(worktree.checkout_id == checkout_id, "The task's managed worktree changed");
        let candidate: Vec<u8> = db.query_row(
            "SELECT c.evidence FROM job_worktree_candidates c JOIN job_worktree_starts s ON s.run_id = c.run_id
             WHERE c.run_id = ?1 AND c.capture_confirmed AND s.checkout_id = ?2",
            params![run, checkout_id], |row| row.get(0),
        ).optional()?.context("The paused goal has no confirmed worktree candidate")?;
        Ok((worktree, git_dir, common, candidate))
    }).await?;
    tokio::task::spawn_blocking(move || {
        super::candidates::recheck_candidate(&worktree, &git_dir, &common, &candidate).map(|_| ())
    })
    .await?
}
