use std::{fs, path::PathBuf, process::Command};

use anyhow::{Result, ensure};

use crate::repository::{git_path, register};

use super::*;

#[test]
fn note_previews_skip_frontmatter_and_the_body_of_obsidian_comments() {
    let source = "\u{feff}---\r\ntitle: Preview\r\n---\r\n\r\n# Heading\r\n%%\r\nInternal source comment\r\n%%\r\nUser-visible summary. %% Omitted %%";
    assert_eq!(
        note_preview(source, true).as_deref(),
        Some("User-visible summary.")
    );
    assert!(note_preview("%%\nunfinished comment", false).is_none());
}

const SOURCE: &str = "\u{feff}---\r\ntitle: Revisão de lançamento\r\naliases: [\"Runbook, release\", Manual]\r\ntags: [release, checklist]\r\nkanban-plugin: board # keep\r\n---\r\n\r\n# Janela aprovada\r\n\r\n🚀 The deployment window covers Pacífico. #ops/windows ^readiness\r\n\r\n[[Manual#^done|follow]] and [notes](./Notes.md#details).\r\n\r\n```dataview\r\nLIST FROM [[Ignore-query]]\r\n```\r\n\r\n%% [[Ignore-hidden]] %%\r\n";

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new(git_path()?)
        .current_dir(root)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn fixture() -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let repo = temp.path().join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    fs::create_dir(repo.join("docs"))?;
    fs::write(repo.join("docs/Release.md"), SOURCE)?;
    fs::write(
        repo.join("docs/Notes.md"),
        "# Details\n\nOnly project notes are searchable.",
    )?;
    fs::write(
        repo.join("docs/Board.canvas"),
        "must not appear in Markdown search",
    )?;
    fs::write(
        repo.join("docs/Board.base"),
        "must not appear in Markdown search",
    )?;
    fs::write(repo.join("docs/Bad.md"), [0xff, 0xfe, 0, 0])?;
    git(&repo, &["add", "."])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-m",
            "Add index fixture",
        ],
    )?;
    Ok((temp, repo))
}

#[tokio::test]
async fn indexes_checkout_scoped_search_and_canonical_metadata_without_mutating_sources()
-> Result<()> {
    let (temp, repo) = fixture()?;
    let database = temp.path().join("storage/app.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = register(&store, repo.clone()).await?.checkout_id;
    let catalog = store
        .rebuild_repository_note_index(checkout.clone())
        .await?;
    assert_eq!(
        catalog
            .iter()
            .map(|note| note.path.as_str())
            .collect::<Vec<_>>(),
        ["docs/Notes.md", "docs/Release.md"]
    );
    let found = store
        .search_repository_note_index(checkout.clone(), "revisao lanc".into(), 10)
        .await?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "Revisão de lançamento");
    let found = store
        .search_repository_note_index(checkout.clone(), "deployment pacifico".into(), 10)
        .await?;
    assert_eq!(found.len(), 1);
    assert!(found[0].snippet.contains("deployment window"));
    assert!(
        store
            .search_repository_note_index(checkout.clone(), "must not appear".into(), 10)
            .await?
            .is_empty()
    );
    assert!(
        store
            .search_repository_note_index(checkout.clone(), "\"\")*".into(), 10)
            .await?
            .is_empty()
    );

    let details = store
        .repository_indexed_note(checkout.clone(), "docs/Release.md".into())
        .await?
        .context("find indexed note")?;
    assert_eq!(details.aliases, ["Runbook, release", "Manual"]);
    assert!(
        details
            .properties
            .iter()
            .any(|entry| entry.name == "kanban-plugin" && entry.source == "board")
    );
    assert_eq!(details.headings[0].line, 8);
    assert_eq!(details.blocks[0].id, "readiness");
    assert_eq!(details.blocks[0].line, 10);
    assert_eq!(
        details.note.tags.into_iter().collect::<Vec<_>>(),
        ["checklist", "ops/windows", "release"]
    );
    assert_eq!(
        details
            .links
            .iter()
            .map(|link| link.target.as_str())
            .collect::<Vec<_>>(),
        ["Manual", "./Notes.md#details"]
    );
    assert_eq!(details.links[0].line, 12);
    let range = details.links[0].range;
    assert_eq!(
        &SOURCE[range.byte_start..range.byte_end],
        "[[Manual#^done|follow]]"
    );
    assert_eq!(fs::read(repo.join("docs/Release.md"))?, SOURCE.as_bytes());

    let linked = temp.path().join("linked");
    git(
        &repo,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    )?;
    fs::write(
        linked.join("docs/Release.md"),
        "# Worktree\n\nOnly the alternate checkout has this heading.",
    )?;
    let other = register(&store, linked).await?.checkout_id;
    let linked_catalog = store.rebuild_repository_note_index(other.clone()).await?;
    assert_ne!(catalog[1].id, linked_catalog[1].id);
    assert!(
        store
            .search_repository_note_index(other, "pacifico".into(), 10)
            .await?
            .is_empty()
    );
    drop(store);

    let reopened = Store::open(database)?;
    let result = reopened
        .search_repository_note_index(checkout.clone(), "pacifico".into(), 10)
        .await?;
    assert_eq!(result[0].path, "docs/Release.md");
    let scanned = reopened.rebuild_repository_note_index(checkout).await?;
    assert_eq!(scanned[1].id, catalog[1].id);
    Ok(())
}
