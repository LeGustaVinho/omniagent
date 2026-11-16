use rusqlite::{OptionalExtension, params};
use serde::Deserialize;

use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct PreparedIntent {
    pub(super) sha: String,
    acceptance_artifact_id: Option<String>,
}

#[tokio::test]
async fn resumes_a_prepared_commit_and_reconciles_publication_after_restart() -> Result<()> {
    exercise_restart(false).await?;
    exercise_restart(true).await
}

async fn exercise_restart(ref_already_published: bool) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("commit-intent.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temporary.path()).await?;
    let unit = fixture
        .units
        .first()
        .context("missing first logical unit")?;
    let worktree = store
        .live_managed_worktree(fixture.project.clone(), unit.task_id.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;
    assert_eq!(
        store.delivery_git_policy(fixture.project.clone()).await?,
        DeliveryGitPolicy::AutomaticCommitUserMerge
    );
    let accepted = capture_and_review(&store, &fixture, &worktree, unit, "first.txt").await?;
    let branch = format!("refs/heads/{}", worktree.branch);
    let original_second = git(&worktree.root, &["show", "HEAD:second.txt"])?;
    let primary_tip = git(
        Path::new(&fixture.checkout.repository_root),
        &["rev-parse", "HEAD"],
    )?;

    fs::write(
        worktree.root.join("second.txt"),
        "unrelated staged change\n",
    )?;
    git(&worktree.root, &["add", "--", "second.txt"])?;
    fs::write(
        worktree.root.join("untracked.txt"),
        "unrelated working change\n",
    )?;
    let git_dir = git(&worktree.root, &["rev-parse", "--absolute-git-dir"])?;
    let index_lock = Path::new(&git_dir).join("index.lock");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&index_lock)?;
    let interrupted = commit(&store, &fixture, unit, &accepted, None)
        .await
        .unwrap_err();
    assert!(format!("{interrupted:#}").contains("Git operation in progress"));
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        worktree.base_commit
    );
    let intent = load_intent(&store, unit)
        .await?
        .context("commit intent was not persisted")?;
    assert_eq!(
        intent.acceptance_artifact_id.as_deref(),
        Some(accepted.acceptance.id.as_str())
    );
    assert_eq!(
        git(&worktree.root, &["rev-parse", &format!("{}^", intent.sha)])?,
        worktree.base_commit
    );
    assert_eq!(settled_commit_count(&store, unit).await?, 0);

    fs::remove_file(index_lock)?;
    if ref_already_published {
        // Reproduce the on-disk state if the app exits after Git publishes the
        // prepared SHA but before the SQLite receipt transaction commits.
        git(
            &worktree.root,
            &["update-ref", &branch, &intent.sha, &worktree.base_commit],
        )?;
    }
    drop(store);
    let reopened = Store::open(database)?;
    assert_eq!(load_intent(&reopened, unit).await?.unwrap().sha, intent.sha);
    let outcome = commit(&reopened, &fixture, unit, &accepted, None).await?;
    let receipt = verified(&reopened, &fixture, unit, outcome.clone()).await?;
    assert_eq!(receipt.source, CommitSource::Automatic);
    assert_eq!(receipt.sha, intent.sha);
    assert!(receipt.pending_changes_retained);
    assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, intent.sha);
    assert_eq!(
        git(
            Path::new(&fixture.checkout.repository_root),
            &["rev-parse", "HEAD"]
        )?,
        primary_tip
    );
    assert_eq!(
        git(&worktree.root, &["show", "HEAD:second.txt"])?,
        original_second
    );
    assert_eq!(
        git(&worktree.root, &["show", ":second.txt"])?,
        "unrelated staged change"
    );
    assert_eq!(
        fs::read_to_string(worktree.root.join("untracked.txt"))?,
        "unrelated working change\n"
    );
    assert!(git(&worktree.root, &["show", "HEAD:untracked.txt"]).is_err());
    assert!(load_intent(&reopened, unit).await?.is_none());
    assert_eq!(settled_commit_count(&reopened, unit).await?, 1);
    assert_eq!(
        commit(&reopened, &fixture, unit, &accepted, None).await?,
        outcome
    );
    Ok(())
}

pub(super) async fn load_intent(
    store: &Store,
    unit: &WorkflowDeliveryUnit,
) -> Result<Option<PreparedIntent>> {
    let (project, task, unit) = (
        unit.project_id.clone(),
        unit.task_id.clone(),
        unit.unit.id.clone(),
    );
    store.execute(move |db| {
        let bytes: Option<Vec<u8>> = db.query_row(
            "SELECT payload FROM delivery_commit_intents WHERE project_id = ?1 AND task_id = ?2 AND unit_id = ?3",
            params![project, task, unit],
            |row| row.get(0),
        ).optional()?;
        bytes.map(|payload| {
            let bytes = crate::protection::unprotect(&payload)?;
            Ok(serde_json::from_slice(&bytes)?)
        }).transpose()
    }).await
}

async fn settled_commit_count(store: &Store, unit: &WorkflowDeliveryUnit) -> Result<i64> {
    let (project, task, unit) = (
        unit.project_id.clone(),
        unit.task_id.clone(),
        unit.unit.id.clone(),
    );
    store.execute(move |db| Ok(db.query_row(
        "SELECT count(*) FROM verified_delivery_commits WHERE project_id = ?1 AND task_id = ?2 AND unit_id = ?3",
        params![project, task, unit],
        |row| row.get(0),
    )?)).await
}
