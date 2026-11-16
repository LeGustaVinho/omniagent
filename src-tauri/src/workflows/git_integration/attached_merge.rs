use crate::{
    repository::verified_merge::{DeliveryMergeOutcome, DeliveryMergeRequest},
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
};
use rusqlite::params;

use super::*;

#[tokio::test]
async fn automatic_merge_updates_the_clean_primary_checkout() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = Store::open(temporary.path().join("attached-automatic.sqlite"))?;
    let fixture = clean_delivery_fixture(&store, temporary.path()).await?;
    let root = Path::new(&fixture.checkout.repository_root);
    let branch = git(root, &["symbolic-ref", "--short", "HEAD"])?;
    fs::write(
        root.join("destination.txt"),
        b"independent destination work\n",
    )?;
    let previous = user_commit(
        root,
        "destination.txt",
        "Record independent destination work",
    )?;
    store
        .set_delivery_git_policy(
            fixture.project.clone(),
            DeliveryGitPolicy::AutomaticLocalCommitAndMerge,
        )
        .await?;
    let (worktree, outcome) = commit_units(&store, &fixture).await?;
    let WorkflowUnitOutcome::CommitWithMerge { artifact: _, merge } = outcome else {
        anyhow::bail!("the final commit did not start automatic integration");
    };
    let WorkflowUnitOutcome::Artifact { artifact } = *merge else {
        anyhow::bail!("automatic integration did not complete: {merge:?}");
    };
    let ArtifactValue::Merge(reference) = artifact.value else {
        anyhow::bail!("expected typed merge evidence");
    };
    let last = fixture.units.last().context("missing final unit")?;
    let receipt = store
        .verified_delivery_merge(
            fixture.project.clone(),
            last.task_id.clone(),
            reference.commit_evidence_id,
        )
        .await?
        .context("missing merge receipt")?;
    assert_eq!(receipt.destination_branch, branch);
    assert_eq!(receipt.previous_destination_sha, previous);
    assert_ne!(receipt.integration_sha, receipt.source_sha);
    assert_eq!(
        git(root, &["show", "-s", "--format=%P", "HEAD"])?,
        format!(
            "{} {}",
            receipt.previous_destination_sha, receipt.source_sha
        )
    );
    assert_eq!(
        fs::read(root.join("destination.txt"))?,
        b"independent destination work\n"
    );
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        receipt.source_sha
    );
    assert_checkout(root, &receipt.integration_sha)?;
    Ok(())
}

#[tokio::test]
async fn attached_linked_checkout_preserves_dirty_and_reserved_user_work() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = Store::open(temporary.path().join("attached-linked.sqlite"))?;
    let fixture = clean_delivery_fixture(&store, temporary.path()).await?;
    let root = Path::new(&fixture.checkout.repository_root);
    let previous = git(root, &["rev-parse", "HEAD"])?;
    let primary = repository::register(&store, root.to_owned()).await?;
    let target = store
        .create_project_task(fixture.project.clone(), "Destination owner".into())
        .await?;
    let managed = repository::provision_task_worktree(
        &store,
        temporary.path().join("linked destination"),
        fixture.project.clone(),
        target.id.clone(),
        primary.checkout_id,
    )
    .await?;
    let linked = managed.root.clone();
    store
        .set_delivery_milestone_policy(
            fixture.project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedCommit,
                destination_branch: Some(managed.branch),
            },
        )
        .await?;
    let checkout_id = managed.checkout_id;
    let (worktree, outcome) = commit_units(&store, &fixture).await?;
    let request = merge_request(&store, &fixture, outcome).await?;

    store
        .execute({
            let (checkout, project, task) =
                (checkout_id.clone(), fixture.project.clone(), target.id);
            move |db| {
                db.execute(
                    "INSERT INTO app_worktree_writers VALUES (?1, 'fixture-writer', ?2, ?3)",
                    params![checkout, project, task],
                )?;
                Ok(())
            }
        })
        .await?;
    let reserved = store
        .verify_delivery_merge(request.clone())
        .await
        .unwrap_err();
    assert!(format!("{reserved:#}").contains("reserved by app operation fixture-writer"));
    store.execute(move |db| { db.execute("DELETE FROM app_worktree_writers WHERE checkout_id = ?1 AND operation_id = 'fixture-writer'", [&checkout_id])?; Ok(()) }).await?;
    assert_checkout(&linked, &previous)?;

    let tracked = linked.join("first.txt");
    let pristine = fs::read(&tracked)?;
    fs::write(&tracked, b"unreviewed user changes\n")?;
    let dirty = store.verify_delivery_merge(request.clone()).await;
    assert!(!matches!(dirty, Ok(DeliveryMergeOutcome::Verified { .. })));
    assert_eq!(git(&linked, &["rev-parse", "HEAD"])?, previous);
    assert_eq!(git(&linked, &["show", ":first.txt"])?, "original first");
    assert_eq!(fs::read(&tracked)?, b"unreviewed user changes\n");
    fs::write(&tracked, pristine)?;

    let DeliveryMergeOutcome::Verified { evidence } =
        store.verify_delivery_merge(request.clone()).await?
    else {
        anyhow::bail!("clean linked checkout did not integrate");
    };
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, previous);
    assert_eq!(
        git(&worktree.root, &["rev-parse", "HEAD"])?,
        request.expected_source_sha
    );
    assert_checkout(&linked, &evidence.integration_sha)?;
    Ok(())
}

#[tokio::test]
async fn attached_merge_recovers_the_checkout_and_keeps_one_receipt_after_restart() -> Result<()> {
    for published in [false, true] {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("attached-restart.sqlite");
        let store = Store::open(database.clone())?;
        let fixture = clean_delivery_fixture(&store, temporary.path()).await?;
        let linked = temporary.path().join("linked recovery destination");
        let previous = create_linked_destination(&store, &fixture, &linked).await?;
        let (_, outcome) = commit_units(&store, &fixture).await?;
        let request = merge_request(&store, &fixture, outcome).await?;
        let target = store
            .suspend_test_attached_merge(request.clone(), published)
            .await?;
        assert_eq!(
            git(&linked, &["rev-parse", "HEAD"])?,
            if published {
                target.as_str()
            } else {
                previous.as_str()
            }
        );
        assert_eq!(
            git(&linked, &["show", ":first.txt"])?,
            git(&linked, &["show", &format!("{target}:first.txt")])?
        );
        assert!(
            store
                .verified_delivery_merge(
                    request.project_id.clone(),
                    request.task_id.clone(),
                    request.commit_evidence_id.clone()
                )
                .await?
                .is_none()
        );
        drop(store);

        let reopened = Store::open(database)?;
        if !published {
            let file = linked.join("first.txt");
            let prepared = fs::read(&file)?;
            fs::write(&file, b"concurrent user edit after interruption\n")?;
            let blocked = reopened.verify_delivery_merge(request.clone()).await?;
            let DeliveryMergeOutcome::RecoveryRequired {
                recovery_directory, ..
            } = blocked
            else {
                anyhow::bail!("unknown worktree bytes must require recovery: {blocked:?}");
            };
            assert!(Path::new(&recovery_directory).is_dir());
            assert_eq!(
                fs::read(&file)?,
                b"concurrent user edit after interruption\n"
            );
            assert_eq!(git(&linked, &["rev-parse", "HEAD"])?, previous);
            fs::write(&file, prepared)?;
        }
        let outcome = reopened.verify_delivery_merge(request.clone()).await?;
        let DeliveryMergeOutcome::Verified { evidence } = &outcome else {
            anyhow::bail!("attached integration did not recover: {outcome:?}");
        };
        assert_eq!(evidence.integration_sha, target);
        assert_eq!(evidence.previous_destination_sha, previous);
        assert_checkout(&linked, &target)?;
        assert_eq!(reopened.verify_delivery_merge(request).await?, outcome);
    }
    Ok(())
}

async fn clean_delivery_fixture(store: &Store, path: &Path) -> Result<ApprovedDeliveryFixture> {
    let fixture = approved_delivery_fixture(store, path).await?;
    let root = Path::new(&fixture.checkout.repository_root);
    git(root, &["config", "core.autocrlf", "false"])?;
    let git = PathBuf::from(git(root, &["rev-parse", "--absolute-git-dir"])?);
    fs::create_dir_all(git.join("info"))?;
    fs::write(git.join("info/exclude"), b"codex.exe\n")?;
    Ok(fixture)
}

async fn create_linked_destination(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    linked: &Path,
) -> Result<String> {
    let root = Path::new(&fixture.checkout.repository_root);
    let previous = git(root, &["rev-parse", "HEAD"])?;
    git(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "integration",
            linked.to_str().context("missing linked path")?,
            &previous,
        ],
    )?;
    store
        .set_delivery_milestone_policy(
            fixture.project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedCommit,
                destination_branch: Some("integration".into()),
            },
        )
        .await?;
    Ok(previous)
}

async fn commit_units(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
) -> Result<(ManagedWorktree, WorkflowUnitOutcome)> {
    let [first, last] = fixture.units.as_slice() else {
        anyhow::bail!("expected two logical units");
    };
    let worktree = store
        .live_managed_worktree(fixture.project.clone(), first.task_id.clone())
        .await?
        .context("missing managed worktree")?
        .worktree;
    let acceptance = capture_and_review(store, fixture, &worktree, first, "first.txt").await?;
    commit(store, fixture, first, &acceptance, None).await?;
    let acceptance = capture_and_review(store, fixture, &worktree, last, "second.txt").await?;
    let outcome = commit(store, fixture, last, &acceptance, None).await?;
    Ok((worktree, outcome))
}

async fn merge_request(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    outcome: WorkflowUnitOutcome,
) -> Result<DeliveryMergeRequest> {
    let WorkflowUnitOutcome::Artifact { artifact } = outcome else {
        anyhow::bail!("missing verified final commit");
    };
    let ArtifactValue::Git(reference) = artifact.value else {
        anyhow::bail!("missing final Git reference");
    };
    let unit = fixture.units.last().context("missing final unit")?;
    let preview = store
        .delivery_merge_preview(
            fixture.project.clone(),
            unit.task_id.clone(),
            fixture.run.id.clone(),
            unit.unit.id.clone(),
            reference.id.clone(),
        )
        .await?;
    Ok(DeliveryMergeRequest {
        project_id: fixture.project.clone(),
        task_id: unit.task_id.clone(),
        workflow_run_id: fixture.run.id.clone(),
        unit_id: unit.unit.id.clone(),
        commit_evidence_id: reference.id,
        expected_source_branch: preview.source_branch,
        expected_source_sha: preview.source_sha,
        destination_branch: preview.destination_branch,
        expected_destination_sha: preview.expected_destination_sha,
        external_sha: None,
        drift_acceptance_id: None,
        prepare_drift: false,
        perform: true,
    })
}

fn assert_checkout(root: &Path, sha: &str) -> Result<()> {
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, sha);
    assert!(
        git(
            root,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain=v1",
                "--untracked-files=all"
            ]
        )?
        .is_empty()
    );
    for path in ["first.txt", "second.txt"] {
        let committed = git(root, &["show", &format!("HEAD:{path}")])?;
        assert_eq!(git(root, &["show", &format!(":{path}")])?, committed);
        assert_eq!(fs::read_to_string(root.join(path))?.trim_end(), committed);
    }
    Ok(())
}
