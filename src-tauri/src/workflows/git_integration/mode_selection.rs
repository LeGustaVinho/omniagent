use super::{commit_intent::load_intent, *};

#[tokio::test]
async fn a_reverted_selected_mode_blocks_commit_and_replay_until_the_approved_mode_returns()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("mode-selection.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temporary.path()).await?;
    let unit = fixture.units.first().context("missing logical unit")?;
    let worktree = store
        .live_managed_worktree(fixture.project.clone(), unit.task_id.clone())
        .await?
        .context("missing worktree")?
        .worktree;
    let root = &worktree.root;
    git(root, &["config", "core.filemode", "false"])?;
    let base = git(root, &["rev-parse", "HEAD"])?;
    assert!(git(root, &["ls-tree", "HEAD", "--", "first.txt"])?.starts_with("100644 "));

    let implementation = queue_implementation(&store, &fixture, &worktree, unit).await?;
    let writer = claim_fixture_job(&store, implementation, fixture_admission()).await?;
    let run = writer.run.id;
    store.mark_agent_running(run.clone()).await?;
    ensure!(repository::capture_worktree_start(&store, run.clone()).await?);
    git(root, &["update-index", "--chmod=+x", "--", "first.txt"])?;
    assert!(
        git(root, &["diff", "--cached", "--summary", "--", "first.txt"])?
            .contains("100644 => 100755")
    );
    repository::save_worktree_candidate(&store, run.clone()).await?;
    store
        .finish_agent_attempt(run.clone(), RunStatus::Succeeded, None)
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(fixture.project.clone(), run.clone())
        .await?
    else {
        anyhow::bail!("missing captured mode change");
    };
    let candidate = select_candidate(
        &store,
        &fixture,
        CandidateTarget {
            project_id: fixture.project.clone(),
            task_id: unit.task_id.clone(),
            unit_id: unit.unit.id.clone(),
            run_id: run,
            capture_revision: artifact.revision,
        },
        &["first.txt"],
    )
    .await?;
    let review = review_candidate(
        &store,
        &fixture,
        &worktree,
        unit,
        &candidate,
        &first_reviewer(),
    )
    .await?;
    let checks = verify_candidate(&store, &fixture, unit, &candidate).await?;
    let acceptance = accept_candidate(&store, &fixture, unit, &candidate, &review, &checks).await?;
    let approved = AcceptedUnit {
        candidate,
        acceptance,
    };

    git(root, &["update-index", "--chmod=-x", "--", "first.txt"])?;
    fs::write(root.join("second.txt"), "unrelated staged user change\n")?;
    git(root, &["add", "--", "second.txt"])?;
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"])?,
        "second.txt"
    );

    assert_stale_mode_refused(&store, &fixture, unit, root, &base, &approved).await?;
    let intended_sha = load_intent(&store, unit)
        .await?
        .context("missing prepared commit")?
        .sha;
    drop(store);
    let store = Store::open(database)?;
    assert_stale_mode_refused(&store, &fixture, unit, root, &base, &approved).await?;
    assert_eq!(
        load_intent(&store, unit)
            .await?
            .context("missing retained commit intent")?
            .sha,
        intended_sha
    );

    git(root, &["update-index", "--chmod=+x", "--", "first.txt"])?;
    let outcome = commit(&store, &fixture, unit, &approved, None).await?;
    let receipt = verified(&store, &fixture, unit, outcome.clone()).await?;
    assert_eq!(receipt.sha, intended_sha);
    assert_eq!(receipt.source, CommitSource::Automatic);
    assert_eq!(receipt.base_commit, base);
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, receipt.sha);
    assert!(git(root, &["ls-tree", "HEAD", "--", "first.txt"])?.starts_with("100755 "));
    assert_eq!(git(root, &["show", "HEAD:first.txt"])?, "original first");
    assert_eq!(git(root, &["show", "HEAD:second.txt"])?, "original second");
    assert_eq!(
        git(root, &["show", ":second.txt"])?,
        "unrelated staged user change"
    );
    assert!(receipt.pending_changes_retained);
    assert_eq!(
        commit(&store, &fixture, unit, &approved, None).await?,
        outcome
    );
    Ok(())
}

async fn assert_stale_mode_refused(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    unit: &WorkflowDeliveryUnit,
    root: &Path,
    base: &str,
    approved: &AcceptedUnit,
) -> Result<()> {
    let error = commit(store, fixture, unit, approved, None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("selected file mode changed in the index: first.txt"));
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, base);
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"])?,
        "second.txt"
    );
    assert!(
        store
            .workflow_delivery_units(
                fixture.project.clone(),
                unit.task_id.clone(),
                fixture.run.id.clone()
            )
            .await?
            .first()
            .is_some_and(|unit| unit.commit_evidence_id.is_none())
    );
    Ok(())
}
