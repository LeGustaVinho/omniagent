use super::*;
use crate::repository::register;

fn git(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(passive(root, args)?)?
        .trim_end()
        .to_owned())
}

#[tokio::test]
async fn reviews_real_worktrees_and_updates_only_the_selected_index_path() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("main checkout");
    let linked = temporary.path().join("linked checkout");
    fs::create_dir_all(root.join("docs"))?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    let changed = "docs/[résumé].txt";
    let other = "docs/other.txt";
    fs::write(root.join(changed), "original\n")?;
    fs::write(root.join(other), "other baseline\n")?;
    git(&root, &["add", "."])?;
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "Initial review source",
        ],
    )?;
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    )?;

    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let main = register(&store, root.clone()).await?;
    let alternate = register(&store, linked.clone()).await?;
    fs::write(root.join(changed), "review candidate\n")?;
    fs::write(root.join(other), "unrelated staging\n")?;
    git(&root, &["add", "--", other])?;
    let review = store.git_checkout_review(main.checkout_id.clone()).await?;
    assert_eq!(review.revision.branch.as_deref(), Some("refs/heads/main"));
    assert_eq!(
        review.changes,
        [
            GitPathStatus {
                path: changed.into(),
                staged: None,
                working: Some('M')
            },
            GitPathStatus {
                path: other.into(),
                staged: Some('M'),
                working: None
            },
        ]
    );
    assert!(
        review
            .worktrees
            .iter()
            .any(
                |tree| tree.checkout_id.as_deref() == Some(&alternate.checkout_id)
                    && tree.branch.as_deref() == Some("linked")
            )
    );
    assert!(
        store
            .git_checkout_review(alternate.checkout_id.clone())
            .await?
            .changes
            .is_empty()
    );
    let commits = store
        .git_checkout_history(alternate.checkout_id, Some("refs/heads/main".into()))
        .await?;
    assert_eq!(commits[0].subject, "Initial review source");
    assert_eq!(Some(&commits[0].sha), review.revision.head.as_ref());
    assert!(
        store
            .git_checkout_history(main.checkout_id.clone(), Some("HEAD~1".into()))
            .await
            .is_err()
    );

    // A local Git clean filter is executable configuration and must not run as
    // a side effect of reviewing or staging a path through this workspace.
    fs::write(root.join(".gitattributes"), "*.txt filter=unapproved\n")?;
    let revision = review.revision.clone();
    git(&root, &["add", "--", ".gitattributes"])?;
    git(
        &root,
        &[
            "config",
            "filter.unapproved.clean",
            "echo executed > filter-ran; cat",
        ],
    )?;

    assert!(
        store
            .set_git_path_staged(main.checkout_id.clone(), changed.into(), true, revision)
            .await
            .is_err()
    );
    let revision = store
        .git_checkout_review(main.checkout_id.clone())
        .await?
        .revision;
    assert!(!root.join("filter-ran").exists());
    store
        .set_git_path_staged(main.checkout_id.clone(), changed.into(), true, revision)
        .await?;
    assert!(!root.join("filter-ran").exists());
    fs::write(root.join(changed), "second unsaved version\n")?;
    let staged = store
        .git_checkout_diff(
            main.checkout_id.clone(),
            changed.into(),
            GitDiffSource::Staged,
        )
        .await?;
    assert_eq!(
        staged.original,
        GitFileSide::Text {
            content: "original\n".into()
        }
    );
    assert_eq!(
        staged.modified,
        GitFileSide::Text {
            content: "review candidate\n".into()
        }
    );
    let live = store
        .git_checkout_diff(
            main.checkout_id.clone(),
            changed.into(),
            GitDiffSource::Working,
        )
        .await?;
    assert_eq!(live.original, staged.modified);
    assert_eq!(
        live.modified,
        GitFileSide::Text {
            content: "second unsaved version\n".into()
        }
    );
    let revision = store
        .git_checkout_review(main.checkout_id.clone())
        .await?
        .revision;
    store
        .set_git_path_staged(main.checkout_id.clone(), changed.into(), false, revision)
        .await?;
    assert_eq!(
        git(&root, &["show", &format!(":{other}")])?,
        "unrelated staging"
    );
    assert_eq!(git(&root, &["show", &format!(":{changed}")])?, "original");
    assert_eq!(
        fs::read_to_string(root.join(changed))?,
        "second unsaved version\n"
    );
    assert!(store.git_saved_attempts(main.project_id).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn reviews_and_stages_an_unborn_checkout_without_fabricating_a_base() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("first checkout");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    fs::write(root.join("README.md"), "first file\n")?;
    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let checkout = register(&store, root).await?.checkout_id;
    let review = store.git_checkout_review(checkout.clone()).await?;
    assert!(review.revision.head.is_none());
    assert_eq!(review.changes[0].working, Some('?'));
    assert!(
        store
            .git_checkout_history(checkout.clone(), None)
            .await?
            .is_empty()
    );
    store
        .set_git_path_staged(checkout.clone(), "README.md".into(), true, review.revision)
        .await?;
    let diff = store
        .git_checkout_diff(checkout.clone(), "README.md".into(), GitDiffSource::Staged)
        .await?;
    assert_eq!(diff.original, GitFileSide::Missing);
    assert_eq!(
        diff.modified,
        GitFileSide::Text {
            content: "first file\n".into()
        }
    );
    let review = store.git_checkout_review(checkout.clone()).await?;
    store
        .set_git_path_staged(checkout.clone(), "README.md".into(), false, review.revision)
        .await?;
    assert_eq!(
        store.git_checkout_review(checkout).await?.changes[0].working,
        Some('?')
    );
    Ok(())
}

#[tokio::test]
async fn commits_only_the_reviewed_index_in_an_unmanaged_linked_checkout() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("project");
    let linked = temporary.path().join("linked");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "user.name", "Fixture"])?;
    git(&root, &["config", "user.email", "fixture@example.invalid"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    fs::write(root.join("tracked.txt"), "initial\n")?;
    git(&root, &["add", "--", "tracked.txt"])?;

    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let main = register(&store, root.clone()).await?.checkout_id;
    let revision = store.git_checkout_review(main.clone()).await?.revision;
    let original = store
        .commit_git_index(main.clone(), "Initial import".into(), revision)
        .await?;
    assert_eq!(git(&root, &["rev-parse", "HEAD"])?, original);
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    )?;
    let alternate = register(&store, linked.clone()).await?.checkout_id;

    fs::write(linked.join("tracked.txt"), "staged version\n")?;
    git(&linked, &["add", "--", "tracked.txt"])?;
    let stale = store.git_checkout_review(alternate.clone()).await?.revision;
    fs::write(linked.join("included.txt"), "explicitly staged\n")?;
    git(&linked, &["add", "--", "included.txt"])?;
    fs::write(linked.join("tracked.txt"), "unstaged version\n")?;
    fs::write(linked.join("excluded.txt"), "untracked file\n")?;
    assert!(
        store
            .commit_git_index(alternate.clone(), "Stale staged state".into(), stale)
            .await
            .is_err()
    );
    let review = store.git_checkout_review(alternate.clone()).await?;
    let head_lock = PathBuf::from(inspect(&linked)?.git_dir).join("HEAD.lock");
    let lock = lock_git_file(&head_lock)?;
    assert!(
        store
            .commit_git_index(
                alternate.clone(),
                "Concurrent branch change".into(),
                review.revision.clone()
            )
            .await
            .is_err()
    );
    drop(lock);
    assert_eq!(git(&linked, &["rev-parse", "HEAD"])?, original);

    let expected_index = review.revision.index.clone();
    let sha = store
        .commit_git_index(
            alternate.clone(),
            "Review staged changes".into(),
            review.revision,
        )
        .await?;
    assert_eq!(git(&linked, &["rev-parse", "HEAD"])?, sha);
    assert_eq!(git(&linked, &["rev-parse", "HEAD^"])?, original);
    assert_eq!(git(&root, &["rev-parse", "HEAD"])?, original);
    assert_eq!(
        git(&linked, &["show", "HEAD:tracked.txt"])?,
        "staged version"
    );
    assert_eq!(
        git(&linked, &["show", "HEAD:included.txt"])?,
        "explicitly staged"
    );
    assert!(git(&linked, &["show", "HEAD:excluded.txt"]).is_err());
    let review = store.git_checkout_review(alternate.clone()).await?;
    assert_eq!(review.revision.index, expected_index);
    assert_eq!(
        review
            .changes
            .iter()
            .map(|file| (file.path.as_str(), file.working))
            .collect::<Vec<_>>(),
        [("excluded.txt", Some('?')), ("tracked.txt", Some('M'))]
    );
    assert!(review.changes.iter().all(|entry| entry.staged.is_none()));
    assert!(
        store
            .commit_git_index(alternate, "No staged changes".into(), review.revision)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(linked.join("tracked.txt"))?,
        "unstaged version\n"
    );
    assert_eq!(
        git(&linked, &["reflog", "show", "--format=%gs", "feature"])?
            .lines()
            .next(),
        Some("OmniAgent staged index commit")
    );
    Ok(())
}
