use super::*;
use crate::repository::register;

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = passive_command(root)?.args(args).output()?;
    Ok(String::from_utf8(check(output, args[0])?)?
        .trim_end()
        .to_owned())
}

fn commit_fixture(root: &Path, message: &str) -> Result<String> {
    git(root, &["add", "--all"])?;
    git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            message,
        ],
    )?;
    git(root, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn reviews_real_conflict_stages_and_only_stages_the_confirmed_working_result() -> Result<()> {
    use crate::store::DraftKey;

    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("conflict worktree");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    let path = "conflict [literal].txt";
    let deleted = "deleted-incoming.txt";
    fs::write(root.join(path), "base\n")?;
    fs::write(root.join(deleted), "before deletion\n")?;
    fs::write(root.join("unrelated.txt"), "unrelated base\n")?;
    commit_fixture(&root, "Base")?;
    git(&root, &["checkout", "--quiet", "-b", "incoming"])?;
    fs::write(root.join(path), "incoming\n")?;
    fs::remove_file(root.join(deleted))?;
    commit_fixture(&root, "Incoming")?;
    git(&root, &["checkout", "--quiet", "main"])?;
    fs::write(root.join(path), "local\n")?;
    fs::write(root.join(deleted), "updated locally\n")?;
    commit_fixture(&root, "Local")?;
    assert!(
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "merge",
                "--no-edit",
                "incoming"
            ]
        )
        .is_err()
    );

    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let original = store
        .git_conflict_review(checkout.clone(), path.into())
        .await?;
    let details = serde_json::to_value(&original)?;
    assert_eq!(details["base"]["file"]["content"], "base\n");
    assert_eq!(details["local"]["file"]["content"], "local\n");
    assert_eq!(details["incoming"]["file"]["content"], "incoming\n");
    assert!(
        details["working"]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("<<<<<<<")
    );
    assert!(original.resolution_blocked.is_some());
    let deleted_before = git(&root, &["ls-files", "--unmerged", "--", deleted])?;
    let missing = serde_json::to_value(
        store
            .git_conflict_review(checkout.clone(), deleted.into())
            .await?,
    )?;
    assert_eq!(missing["incoming"], serde_json::Value::Null);
    assert_eq!(missing["local"]["file"]["content"], "updated locally\n");
    assert!(
        store
            .git_conflict_review(checkout.clone(), "unrelated.txt".into())
            .await
            .is_err()
    );
    assert!(
        store
            .set_git_path_staged(
                checkout.clone(),
                path.into(),
                true,
                original.revision.clone()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .set_git_path_staged(
                checkout.clone(),
                path.into(),
                false,
                original.revision.clone()
            )
            .await
            .is_err()
    );
    let original_snapshot = original.snapshot.context("snapshot of unedited conflict")?;
    assert!(
        store
            .git_mark_conflict_resolved(checkout.clone(), path.into(), original_snapshot.clone())
            .await
            .is_err()
    );

    let merged = "checked and combined\n";
    fs::write(root.join(path), merged)?;
    assert!(
        store
            .git_mark_conflict_resolved(checkout.clone(), path.into(), original_snapshot)
            .await
            .is_err()
    );
    let ready = store
        .git_conflict_review(checkout.clone(), path.into())
        .await?;
    assert!(ready.resolution_blocked.is_none());
    let snapshot = ready.snapshot.context("reviewed working resolution")?;
    let draft = || DraftKey::CheckoutFile {
        checkout_id: checkout.clone(),
        relative_path: path.to_uppercase(),
    };
    store
        .save_draft(draft(), b"unsaved edit".to_vec())
        .await?;
    assert!(
        store
            .git_conflict_review(checkout.clone(), path.into())
            .await?
            .resolution_blocked
            .is_some()
    );
    assert!(
        store
            .git_mark_conflict_resolved(checkout.clone(), path.into(), snapshot.clone())
            .await
            .is_err()
    );
    assert_eq!(
        store.load_draft(draft()).await?,
        Some(b"unsaved edit".to_vec())
    );
    store.delete_draft(draft()).await?;

    fs::write(root.join("unrelated.txt"), "separately staged change\n")?;
    git(&root, &["add", "--", "unrelated.txt"])?;
    assert!(
        store
            .git_mark_conflict_resolved(checkout.clone(), path.into(), snapshot)
            .await
            .is_err()
    );
    let fresh = store
        .git_conflict_review(checkout.clone(), path.into())
        .await?;
    store
        .git_mark_conflict_resolved(
            checkout.clone(),
            path.into(),
            fresh.snapshot.context("refreshed conflict review")?,
        )
        .await?;
    assert!(
        store
            .git_conflict_review(checkout, path.into())
            .await
            .is_err()
    );
    assert_eq!(
        git(&root, &["show", &format!(":{path}")])?,
        merged.trim_end()
    );
    assert_eq!(
        git(&root, &["show", ":unrelated.txt"])?,
        "separately staged change"
    );
    assert_eq!(
        git(&root, &["ls-files", "--unmerged", "--", deleted])?,
        deleted_before
    );
    assert_eq!(fs::read_to_string(root.join(path))?, merged);
    Ok(())
}

#[tokio::test]
async fn persists_reviewed_line_ranges_and_rejects_stale_or_cross_checkout_anchors() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("comment-repo");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    let path = "example.txt";
    fs::write(root.join(path), "original\nbase second\n")?;
    commit_fixture(&root, "Base")?;
    let raw = "first\r\nsecond α\r\nthird β\rfourth\n";
    fs::write(root.join(path), raw)?;

    let database = temporary.path().join("review.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let scope = store
        .git_checkout_diff(checkout.clone(), path.into(), GitDiffSource::Working)
        .await?
        .comment_scope
        .context("commentable working comparison")?;
    let request = |side: &str, start: u64, end: u64| {
        serde_json::from_value::<GitReviewCommentRequest>(serde_json::json!({
            "path": path, "source": &scope.source, "snapshot": &scope.snapshot, "side": side,
            "startLine": start, "endLine": end, "body": "Explain the original behavior",
        }))
    };
    let comment = serde_json::to_value(
        store
            .git_add_review_comment(checkout.clone(), request("modified", 2, 4)?)
            .await?,
    )?;
    assert_eq!(comment["excerpt"], "second α\r\nthird β\rfourth\n");
    assert_eq!(comment["scope"]["snapshot"], scope.snapshot);
    let id = comment["id"]
        .as_str()
        .context("stored review comment ID")?
        .to_owned();
    assert!(
        store
            .git_add_review_comment(checkout.clone(), request("modified", 5, 5)?)
            .await
            .is_err()
    );
    let original = serde_json::to_value(
        store
            .git_add_review_comment(checkout.clone(), request("original", 2, 2)?)
            .await?,
    )?;
    assert_eq!(original["excerpt"], "base second\n");

    let worktree = temporary.path().join("linked");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &worktree.to_string_lossy(),
        ],
    )?;
    fs::write(worktree.join(path), raw)?;
    let linked = register(&store, worktree).await?.checkout_id;
    assert_ne!(linked, checkout);
    assert!(
        store
            .git_add_review_comment(linked.clone(), request("modified", 2, 4)?)
            .await
            .is_err()
    );
    assert!(
        store
            .git_set_review_comment_resolved(linked.clone(), id.clone(), true)
            .await
            .is_err()
    );
    let payload_is_protected = store
        .execute(|db| {
            Ok(db.query_row(
                "SELECT count(*) = 0 FROM git_review_comments
         WHERE instr(payload, CAST('Explain the original behavior' AS BLOB)) > 0",
                [],
                |row| row.get::<_, bool>(0),
            )?)
        })
        .await?;
    assert!(payload_is_protected);
    drop(store);

    let store = Store::open(database)?;
    fs::write(
        root.join(path),
        raw.replace("second α", "different contents"),
    )?;
    assert!(
        store
            .git_add_review_comment(checkout.clone(), request("modified", 2, 4)?)
            .await
            .is_err()
    );
    let revised = store
        .git_checkout_diff(checkout.clone(), path.into(), GitDiffSource::Working)
        .await?
        .comment_scope
        .context("updated worktree comparator")?;
    assert_ne!(scope.snapshot, revised.snapshot);
    store
        .git_set_review_comment_resolved(checkout.clone(), id.clone(), true)
        .await?;
    let saved = serde_json::to_value(
        store
            .git_review_comments(
                checkout.clone(),
                GitReviewCommentSurface::Working,
                Some(path.into()),
            )
            .await?,
    )?;
    let retained = saved
        .as_array()
        .context("persisted comments")?
        .iter()
        .find(|entry| entry["id"] == id)
        .context("retained review comment")?;
    assert_eq!(retained["excerpt"], comment["excerpt"]);
    assert_eq!(retained["scope"], comment["scope"]);
    assert_eq!(retained["resolved"], true);
    assert!(
        store
            .git_review_comments(linked, GitReviewCommentSurface::Working, None)
            .await?
            .is_empty()
    );
    assert!(
        store
            .git_review_comments(checkout, GitReviewCommentSurface::Staged, None)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn follows_file_renames_and_keeps_disk_index_and_commits_distinct() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("file-history");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    let old_path = "previous name.md";
    let path = "read me [a].md";
    fs::write(root.join(old_path), "original\n")?;
    let original = commit_fixture(&root, "Create the note")?;
    git(&root, &["mv", old_path, path])?;
    let renamed = commit_fixture(&root, "Rename the note")?;
    fs::write(root.join(path), "staged\n")?;
    git(&root, &["--literal-pathspecs", "add", "--", path])?;
    fs::write(root.join(path), "unsaved to Git\n")?;

    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let page = store
        .git_checkout_file_history(checkout.clone(), path.into(), None, None, 0)
        .await?;
    assert_eq!(page.head.as_deref(), Some(renamed.as_str()));
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].status, 'R');
    assert_eq!(page.entries[0].previous_path.as_deref(), Some(old_path));
    assert_eq!(page.entries[1].sha, original);
    assert_eq!(page.entries[1].path, old_path);
    assert_eq!(page.entries[1].parent, None);

    let next = store
        .git_checkout_file_history(checkout.clone(), path.into(), None, page.head, 1)
        .await?;
    assert_eq!(next.entries.len(), 1);
    assert_eq!(next.entries[0].path, old_path);
    let staged = store
        .git_compare_file_versions(
            checkout.clone(),
            path.into(),
            Some(GitFileVersion::Commit {
                sha: original.clone(),
                path: old_path.into(),
            }),
            GitFileVersion::Index,
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
            content: "staged\n".into()
        }
    );
    let disk = store
        .git_compare_file_versions(
            checkout.clone(),
            path.into(),
            Some(GitFileVersion::Head),
            GitFileVersion::Worktree,
        )
        .await?;
    assert_eq!(disk.original, staged.original);
    assert_eq!(
        disk.modified,
        GitFileSide::Text {
            content: "unsaved to Git\n".into()
        }
    );
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:some-owner/example.git",
        ],
    )?;
    let line = |text: &str| GitPermalinkLines {
        start_line: 1,
        end_line: 1,
        text: text.to_owned(),
    };
    assert_eq!(
        store
            .git_file_permalink(checkout.clone(), path.into(), None, Some(line("original")))
            .await?,
        format!(
            "https://github.com/some-owner/example/blob/{renamed}/read%20me%20%5Ba%5D.md?plain=1#L1"
        )
    );
    assert!(
        store
            .git_file_permalink(
                checkout.clone(),
                path.into(),
                None,
                Some(line("unsaved to Git"))
            )
            .await
            .is_err()
    );
    git(
        &root,
        &[
            "remote",
            "set-url",
            "origin",
            "https://test-token@gitlab.com/subgroup/example.git",
        ],
    )?;
    assert_eq!(
        store
            .git_file_permalink(checkout.clone(), old_path.into(), Some(original), None)
            .await?,
        format!(
            "https://gitlab.com/subgroup/example/-/blob/{}/previous%20name.md",
            next.entries[0].sha
        )
    );
    assert!(
        store
            .git_compare_file_versions(
                checkout,
                path.into(),
                None,
                GitFileVersion::Commit {
                    sha: "main".into(),
                    path: old_path.into(),
                }
            )
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn applies_only_the_reviewed_git_hunk_and_preserves_worktree_changes() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("hunks");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    let path = "example text.txt";
    let base = (1..=36)
        .map(|line| format!("Line {line}\n"))
        .collect::<String>();
    fs::write(root.join(path), &base)?;
    commit_fixture(&root, "Base content")?;
    let first = base.replace("Line 3\n", "First change\n");
    let both = first.replace("Line 31\n", "Second change\n");
    fs::write(root.join(path), &both)?;
    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let working = store
        .git_checkout_diff(checkout.clone(), path.into(), GitDiffSource::Working)
        .await?;
    assert_eq!(working.hunks.len(), 2);
    assert!(working.hunks[0].patch.contains("+First change"));
    assert!(!working.hunks[0].patch.contains("+Second change"));

    fs::write(
        root.join(path),
        both.replace("First change", "Updated after review"),
    )?;
    assert!(
        store
            .set_git_hunk_staged(
                checkout.clone(),
                path.into(),
                GitDiffSource::Working,
                working.revision.clone(),
                working.hunks[0].id.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(git(&root, &["show", ":example text.txt"])?, base.trim_end());

    fs::write(root.join(path), &both)?;
    let working = store
        .git_checkout_diff(checkout.clone(), path.into(), GitDiffSource::Working)
        .await?;
    store
        .set_git_hunk_staged(
            checkout.clone(),
            path.into(),
            GitDiffSource::Working,
            working.revision.clone(),
            working.hunks[0].id.clone(),
        )
        .await?;
    assert_eq!(
        git(&root, &["show", ":example text.txt"])?,
        first.trim_end()
    );
    assert_eq!(fs::read_to_string(root.join(path))?, both);
    assert!(
        store
            .set_git_hunk_staged(
                checkout.clone(),
                path.into(),
                GitDiffSource::Working,
                working.revision,
                working.hunks[1].id.clone()
            )
            .await
            .is_err()
    );

    git(&root, &["add", "--", path])?;
    let staged = store
        .git_checkout_diff(checkout.clone(), path.into(), GitDiffSource::Staged)
        .await?;
    assert_eq!(staged.hunks.len(), 2);
    store
        .set_git_hunk_staged(
            checkout,
            path.into(),
            GitDiffSource::Staged,
            staged.revision,
            staged.hunks[1].id.clone(),
        )
        .await?;
    assert_eq!(
        git(&root, &["show", ":example text.txt"])?,
        first.trim_end()
    );
    assert_eq!(fs::read_to_string(root.join(path))?, both);
    Ok(())
}

#[tokio::test]
async fn compares_pinned_branch_versions_without_reading_dirty_worktree_content() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("comparison");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    fs::write(root.join("README.md"), "common base\n")?;
    let ancestor = commit_fixture(&root, "Common ancestor")?;
    git(&root, &["switch", "--quiet", "-c", "topic"])?;
    fs::write(root.join("README.md"), "committed topic text\n")?;
    let feature = commit_fixture(&root, "Topic change")?;
    git(&root, &["switch", "--quiet", "main"])?;
    fs::write(root.join("main-only.txt"), "other side of the fork\n")?;
    let base = commit_fixture(&root, "Divergent base change")?;
    git(&root, &["update-ref", "refs/remotes/origin/main", &base])?;
    git(
        &root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    fs::write(root.join("README.md"), "dirty worktree text\n")?;

    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let review = store.git_checkout_review(checkout.clone()).await?;
    assert!(
        review
            .branches
            .iter()
            .any(|branch| branch.reference == "refs/remotes/origin/main")
    );
    assert!(
        !review
            .branches
            .iter()
            .any(|branch| branch.reference == "refs/remotes/origin/HEAD")
    );
    let range = || GitBranchRange {
        base_reference: "refs/remotes/origin/main".into(),
        head_reference: Some("refs/heads/topic".into()),
    };
    let expected = || GitBranchRevisions {
        base: base.clone(),
        head: feature.clone(),
        merge_base: ancestor.clone(),
    };
    let branches = store
        .git_branch_comparison(checkout.clone(), range())
        .await?;
    assert_eq!(branches.revisions, expected());
    assert_eq!(branches.changes.len(), 1);
    assert_eq!(branches.changes[0].path, "README.md");
    assert_eq!(branches.changes[0].status, 'M');
    let diff = store
        .git_branch_comparison_diff(checkout.clone(), range(), expected(), "README.md".into())
        .await?;
    assert_eq!(
        diff.original,
        GitFileSide::Text {
            content: "common base\n".into()
        }
    );
    assert_eq!(
        diff.modified,
        GitFileSide::Text {
            content: "committed topic text\n".into()
        }
    );
    let scope = diff
        .comment_scope
        .context("commentable branch comparison")?;
    let request = || {
        serde_json::from_value::<GitReviewCommentRequest>(serde_json::json!({
            "path": "README.md", "source": &scope.source, "snapshot": &scope.snapshot,
            "side": "modified", "startLine": 1, "endLine": 1, "body": "Check this committed wording",
        }))
    };
    let comment = serde_json::to_value(
        store
            .git_add_review_comment(checkout.clone(), request()?)
            .await?,
    )?;
    assert_eq!(comment["excerpt"], "committed topic text\n");
    assert_eq!(comment["scope"]["source"]["revisions"]["base"], base);
    assert_eq!(comment["scope"]["source"]["revisions"]["head"], feature);
    assert_eq!(
        comment["scope"]["source"]["revisions"]["mergeBase"],
        ancestor
    );
    git(&root, &["update-ref", "refs/heads/topic", &base, &feature])?;
    assert!(
        store
            .git_add_review_comment(checkout.clone(), request()?)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .git_review_comments(checkout.clone(), GitReviewCommentSurface::Branch, None)
            .await?
            .len(),
        1
    );
    let stale = store
        .git_branch_comparison_diff(checkout, range(), expected(), "README.md".into())
        .await;
    assert!(format!("{:#}", stale.unwrap_err()).contains("selected branches changed"));
    Ok(())
}

#[tokio::test]
async fn creates_and_registers_manual_worktrees_without_touching_the_source_or_occupied_paths()
-> Result<()> {
    use crate::repository::{ManualGitWorktree, create_git_worktree};

    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("source worktree");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet", "-b", "main"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    fs::write(root.join("README.md"), "old revision\n")?;
    let first = commit_fixture(&root, "First source revision")?;
    fs::write(root.join("README.md"), "new revision\n")?;
    let head = commit_fixture(&root, "Second source revision")?;
    git(&root, &["branch", "existing"])?;
    fs::write(root.join("README.md"), "unsaved source edit\n")?;
    let default = temporary.path().join("worktrees");
    let new = temporary.path().join("new worktree");
    let occupied = temporary.path().join("already occupied");
    fs::create_dir(&occupied)?;
    fs::write(occupied.join("keep.txt"), "keep me\n")?;
    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let source = register(&store, root.clone()).await?;
    let make = |request| {
        create_git_worktree(
            &store,
            default.clone(),
            source.project_id.clone(),
            source.checkout_id.clone(),
            request,
        )
    };

    let request = |branch: &str, path: &Path| ManualGitWorktree {
        branch: branch.into(),
        existing: false,
        base_reference: Some(first.clone()),
        expected_head: first.clone(),
        path: Some(path.to_owned()),
    };
    assert!(
        make(request("will-not-be-created", &occupied))
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(occupied.join("keep.txt"))?, "keep me\n");
    assert!(
        make(request("inside-source", &root.join("nested")))
            .await
            .is_err()
    );
    let created = make(request("manual/topic", &new)).await?;
    assert_eq!(created.project_id, source.project_id);
    assert_eq!(git(&new, &["branch", "--show-current"])?, "manual/topic");
    assert_eq!(fs::read_to_string(new.join("README.md"))?, "old revision\n");
    assert_eq!(
        fs::read_to_string(root.join("README.md"))?,
        "unsaved source edit\n"
    );
    assert!(
        store
            .git_checkout_review(source.checkout_id.clone())
            .await?
            .worktrees
            .iter()
            .any(|tree| tree.checkout_id.as_deref() == Some(&created.checkout_id) && !tree.managed)
    );

    let existing = make(ManualGitWorktree {
        branch: "existing".into(),
        existing: true,
        base_reference: None,
        expected_head: head.clone(),
        path: None,
    })
    .await?;
    assert!(Path::new(&existing.checkout_root).starts_with(&default));
    assert_eq!(
        git(
            Path::new(&existing.checkout_root),
            &["branch", "--show-current"]
        )?,
        "existing"
    );
    assert_eq!(
        fs::read_to_string(Path::new(&existing.checkout_root).join("README.md"))?,
        "new revision\n"
    );

    store.execute(|db| {
        db.execute_batch("CREATE TRIGGER fail_worktree_registration BEFORE INSERT ON checkouts BEGIN SELECT RAISE(ABORT, 'fixture refused registration'); END;")?;
        Ok(())
    }).await?;
    let failed = temporary.path().join("rolled back worktree");
    let error = make(request("failed-registration", &failed))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("fixture refused registration"));
    assert!(!failed.exists());
    assert!(
        git(
            &root,
            &["show-ref", "--verify", "refs/heads/failed-registration"]
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn reviews_checkout_trusted_by_global_git_config() -> Result<()> {
    const FIXTURE: &str = "OMNIAGENT_TEST_GIT_TRUST_FIXTURE";
    if let Some(fixture) = std::env::var_os(FIXTURE) {
        let fixture = PathBuf::from(fixture);
        let store = Store::open(fixture.join("store.sqlite"))?;
        let rejected = register(&store, fixture.join("other checkout"))
            .await
            .unwrap_err();
        assert!(rejected.to_string().contains("dubious ownership"));

        let root = fixture.join("checkout aceito");
        let checkout = register(&store, root).await?;
        let review = store
            .git_checkout_review(checkout.checkout_id.clone())
            .await?;
        assert!(
            review
                .branches
                .iter()
                .any(|branch| branch.reference == "refs/heads/main")
        );
        assert_eq!(
            review.worktrees[0].checkout_id.as_deref(),
            Some(checkout.checkout_id.as_str())
        );
        let history = store
            .git_checkout_history(checkout.checkout_id, Some("refs/heads/main".into()))
            .await?;
        assert_eq!(history[0].subject, "Trusted checkout");
        return Ok(());
    }

    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("checkout aceito");
    let other = fixture.path().join("other checkout");
    for path in [&root, &other] {
        fs::create_dir(path)?;
        git(path, &["init", "--quiet", "-b", "main"])?;
    }
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "Trusted checkout",
        ],
    )?;
    let config = fixture.path().join("gitconfig");
    check(
        git_command(&git_path()?, &root)
            .args(["config", "--file"])
            .arg(&config)
            .args(["--add", "safe.directory"])
            .arg(dunce::canonicalize(&root)?)
            .output()?,
        "write the fixture's safe directory",
    )?;

    // Use a subprocess so the Git configuration and simulated owner are
    // confined to this test and its Git commands.
    let output = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "repository::git_review::tests::reviews_checkout_trusted_by_global_git_config",
        ])
        .env(FIXTURE, fixture.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", config)
        .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")
        .output()?;
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
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
