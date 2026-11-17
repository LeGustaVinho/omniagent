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

pub(super) mod conversation;
pub(super) mod manual;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_entries_digest: Option<String>,
}

impl WorktreeStart {
    pub(crate) fn restored_by(
        &self,
        observed: &GitStartingState,
        index_entries: Option<&str>,
    ) -> bool {
        fn untracked(state: &GitStartingState) -> impl Iterator<Item = (&str, &str)> {
            state.changes.iter().flat_map(|changes| {
                changes
                    .untracked
                    .iter()
                    .map(|entry| (entry.path.as_str(), entry.sha256.as_str()))
            })
        }
        let index_matches = match self.index_entries_digest.as_deref() {
            Some(expected) => Some(expected) == index_entries,
            None => self.starting.index_digest == observed.index_digest,
        };
        index_matches
            && self.starting.head == observed.head
            && self.starting.status == observed.status
            && self.starting.staged_diff_digest == observed.staged_diff_digest
            && self.starting.unstaged_diff_digest == observed.unstaged_diff_digest
            && untracked(&self.starting).eq(untracked(observed))
    }
}

pub(crate) fn observe_interrupted_writer(
    db: &Connection,
    project: &str,
    owner: &str,
    start: &WorktreeStart,
) -> Result<(GitStartingState, Option<String>)> {
    let (worktree, git_dir, common) = load_worktree(db, project, owner)?
        .context("The writer's managed worktree is unavailable")?;
    ensure!(
        worktree == start.worktree && start.git_common_dir == Path::new(&common),
        "The managed worktree identity changed since this writer started"
    );
    let observed = super::candidates::capture_git_before_writer(&worktree, &git_dir, &common)?;
    Ok((observed.state, observed.index_entries_digest))
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
    provision_task_worktree_at(
        store,
        default_root,
        project_id,
        task_id,
        source_checkout_id,
        None,
    )
    .await
}

pub(crate) struct IntegratedWorktreeBase {
    pub branch: String,
    pub prerequisite_commits: Vec<String>,
    pub verified_tip: Option<String>,
}

impl IntegratedWorktreeBase {
    pub(crate) fn registered_tip(
        &self,
        db: &Connection,
        project: &str,
        checkout: &str,
    ) -> Result<String> {
        let source = db
            .query_row(
                "SELECT c.root, c.git_dir, p.git_common_dir
             FROM checkouts c JOIN projects p ON c.project_id = p.id
             WHERE p.id = ?1 AND c.id = ?2",
                params![project, checkout],
                |row| {
                    Ok(Source {
                        root: PathBuf::from(row.get::<_, String>(0)?),
                        git_dir: row.get(1)?,
                        common: row.get(2)?,
                        override_root: None,
                    })
                },
            )
            .context("The integration checkout is unavailable")?;
        validate_source(&source)?;
        self.verify(&git_path()?, &source, None)
    }

    fn verify(&self, git: &Path, source: &Source, reserved: Option<&str>) -> Result<String> {
        let head = match &self.verified_tip {
            Some(head) => head.clone(),
            None => branch_head(git, &source.root, &self.branch)?
                .context("The pinned integration branch is unavailable in the task repository")?,
        };
        let base = reserved.unwrap_or(&head);
        require_ancestor(git, &source.root, base, &head)
            .context("The saved worktree base is outside the current integration branch")?;
        for prerequisite in &self.prerequisite_commits {
            require_ancestor(git, &source.root, prerequisite, base)
                .context("The worktree base does not contain every verified prerequisite merge")?;
        }
        Ok(base.into())
    }
}

fn require_ancestor(git: &Path, source: &Path, ancestor: &str, tip: &str) -> Result<()> {
    super::git_review::validate_oid(ancestor)?;
    super::git_review::validate_oid(tip)?;
    let output = git_command(git, source)
        .args(["merge-base", "--is-ancestor", ancestor, tip])
        .output()?;
    check(output, "verify worktree base ancestry")?;
    Ok(())
}

pub(crate) async fn provision_task_worktree_at(
    store: &Store,
    default_root: PathBuf,
    project_id: String,
    task_id: String,
    source_checkout_id: String,
    integration: Option<IntegratedWorktreeBase>,
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
    ensure!(
        integration.is_none() || scope == task_id,
        "A backlog delivery requires a dedicated task worktree"
    );
    if let Some((worktree, git_dir, common)) = existing {
        let (_gate, worktree) = tokio::task::spawn_blocking(move || {
            verify_managed(&worktree, &git_dir, &common)?;
            if let Some(integration) = integration {
                validate_source(&source)?;
                integration.verify(&git_path()?, &source, Some(&worktree.base_commit))?;
            }
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
    let (gate, source, git, provision, integration) = tokio::task::spawn_blocking({
        let (project, task) = (project_id.clone(), task_id.clone());
        move || {
            let git = git_path()?;
            validate_source(&source)?;
            let integration_base = integration
                .as_ref()
                .map(|integration| {
                    integration.verify(
                        &git,
                        &source,
                        pending.as_ref().map(|saved| saved.base_commit.as_str()),
                    )
                })
                .transpose()?;
            let provision = match pending {
                Some(saved) => saved,
                None => prepare_named_provision_at(
                    &source,
                    &git,
                    &default_root,
                    &project,
                    &task,
                    "task",
                    integration_base,
                )?,
            };
            Ok::<_, anyhow::Error>((gate, source, git, provision, integration))
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
            if let Some(integration) = integration {
                integration.verify(&git, &source, Some(&provision.base_commit))?;
            }
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

fn prepare_named_provision(
    source: &Source,
    git: &Path,
    default_root: &Path,
    project_id: &str,
    reservation_id: &str,
    branch_kind: &str,
) -> Result<Provision> {
    prepare_named_provision_at(
        source,
        git,
        default_root,
        project_id,
        reservation_id,
        branch_kind,
        None,
    )
}

fn prepare_named_provision_at(
    source: &Source,
    git: &Path,
    default_root: &Path,
    project_id: &str,
    reservation_id: &str,
    branch_kind: &str,
    pinned_base: Option<String>,
) -> Result<Provision> {
    let project_component = Uuid::parse_str(project_id)?.to_string();
    let task_component = Uuid::parse_str(reservation_id)?.to_string();
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
    let branch = format!("codex/{branch_kind}-{task_component}");
    ensure!(
        plain_metadata(&root)?.is_none(),
        "the requested worktree path is already occupied"
    );
    ensure!(
        branch_head(git, &source.root, &branch)?.is_none(),
        "the requested worktree branch already exists"
    );
    let base_commit = match pinned_base {
        Some(base) => base,
        None => base_commit(&source.root)?.context("the source checkout has no base commit")?,
    };
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
    let (checkout, status) = materialize_base(git, source, provision)?;
    ensure!(
        status.is_empty(),
        "the new worktree contains unreviewed changes"
    );
    Ok(checkout)
}

fn materialize_base(
    git: &Path,
    source: &Source,
    provision: &Provision,
) -> Result<(CheckoutPaths, Vec<String>)> {
    // The source may have changed while the provision intent was being saved.
    validate_source(source)?;
    let root = &provision.root;
    if plain_metadata(root)?.is_none() {
        let existing_ref = branch_head(git, &source.root, &provision.branch)?;
        if let Some(head) = &existing_ref {
            ensure!(
                head == &provision.base_commit,
                "the reserved branch changed before its worktree was created"
            );
        }
        add_linked_worktree(git, source, provision, existing_ref.is_some())?;
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
    let status = read_status(&view)?;
    check_branch(root, git, &provision.branch)?;
    ensure!(
        base_commit(root)?.as_deref() == Some(&provision.base_commit),
        "the worktree changed during materialization"
    );
    Ok((checkout, status))
}

fn add_linked_worktree(
    git: &Path,
    source: &Source,
    provision: &Provision,
    existing: bool,
) -> Result<()> {
    let empty_hooks = tempfile::tempdir()?;
    let mut command = git_command(git, &source.root);
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
    if existing {
        command
            .arg("--")
            .arg(&provision.root)
            .arg(&provision.branch);
    } else {
        command
            .args(["--no-track", "-b", &provision.branch, "--"])
            .arg(&provision.root)
            .arg(&provision.base_commit);
    }
    check(command.output()?, "create the linked worktree").map(|_| ())
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
            let reconciled = if writes {
                crate::jobs::load_reconciled_writer_start(db, &run)?
            } else {
                None
            };
            Ok(load_worktree(db, &project, &task)?.map(|identity| (identity, writes, checkpoint, reconciled)))
        }
    }).await?;
    let Some(((worktree, git_dir, common), writes, checkpoint, reconciled)) = identity else {
        return Ok(false);
    };
    let saved = tokio::task::spawn_blocking(move || {
        let paths = verify_managed(&worktree, &git_dir, &common)?;
        ensure!(
            checkpoint.is_none() || reconciled.is_none(),
            "The reconciled writer cannot use a different worktree checkpoint"
        );
        let (starting, index_entries_digest) = match checkpoint {
            Some((Some(payload), Some(true))) => (
                super::candidates::recheck_candidate(&worktree, &git_dir, &common, &payload)?,
                None,
            ),
            Some(_) => anyhow::bail!("The continuation has no confirmed worktree candidate"),
            None if writes => {
                let observed =
                    super::candidates::capture_git_before_writer(&worktree, &git_dir, &common)?;
                if let Some(previous) = reconciled {
                    ensure!(
                        previous.worktree == worktree
                            && previous.git_common_dir == Path::new(&common)
                            && previous.restored_by(
                                &observed.state,
                                observed.index_entries_digest.as_deref()
                            ),
                        "The reconciled writer checkout changed before retry"
                    );
                }
                (observed.state, observed.index_entries_digest)
            }
            None => (capture_git(&worktree, &git_dir, &common, None)?.state, None),
        };
        Ok::<_, anyhow::Error>(WorktreeStart {
            worktree,
            repository_root: PathBuf::from(paths.repository_root),
            git_common_dir: PathBuf::from(common),
            starting,
            index_entries_digest,
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
