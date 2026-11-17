use std::fs;

use super::*;
use crate::repository::{git_path, git_query, register};

#[tokio::test]
async fn discards_only_reviewed_worktree_bytes_and_keeps_staged_and_dirty_data() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("checkout");
    fs::create_dir(&root)?;
    let git = git_path()?;
    let command = |args: &[&str]| git_query(&git, &root, args);
    command(&["init", "--quiet"])?;
    command(&["config", "core.autocrlf", "true"])?;
    fs::write(root.join(".gitattributes"), "*.txt text eol=crlf\n")?;
    let path = "literal [file].txt";
    let tail = (0..20)
        .map(|line| format!("line {line}\r\n"))
        .collect::<String>();
    let file = |top: &str, bottom: &str| format!("{top}\r\n{tail}{bottom}");
    fs::write(root.join(path), file("committed", "last"))?;
    fs::write(root.join("image.bin"), [1, 0, 2, 255])?;
    command(&["add", "."])?;
    command(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ])?;
    let store = Store::open(temp.path().join("profile.sqlite"))?;
    let checkout = register(&store, root.clone()).await?.checkout_id;

    fs::write(root.join(path), file("from index", "last"))?;
    command(&["--literal-pathspecs", "add", "--", path])?;
    fs::write(root.join(path), file("first edit", "second edit"))?;
    let diff = store
        .git_checkout_diff(checkout.clone(), path.into(), GitDiffSource::Working)
        .await?;
    assert_eq!(diff.hunks.len(), 2);
    let first = diff.hunks[0].id.clone();
    let preview = store
        .git_discard_preview(checkout.clone(), path.into(), Some(first.clone()))
        .await?;
    assert!(
        matches!(preview.after, PreviewFile::Text { content } if content == file("from index", "second edit"))
    );
    let index_before = fs::read(root.join(".git/index"))?;
    store
        .git_discard_changes(checkout.clone(), path.into(), Some(first), preview.snapshot)
        .await?;
    assert_eq!(
        fs::read_to_string(root.join(path))?,
        file("from index", "second edit")
    );
    assert_eq!(fs::read(root.join(".git/index"))?, index_before);

    let preview = store
        .git_discard_preview(checkout.clone(), path.into(), None)
        .await?;
    fs::write(root.join(path), file("new external edit", "second edit"))?;
    assert!(
        store
            .git_discard_changes(checkout.clone(), path.into(), None, preview.snapshot)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(root.join(path))?,
        file("new external edit", "second edit")
    );
    let preview = store
        .git_discard_preview(checkout.clone(), path.into(), None)
        .await?;
    let draft_checkout = checkout.clone();
    store.execute(move |db| {
        db.execute("INSERT INTO drafts(kind,owner_id,document,payload) VALUES ('checkout_file',?1,?2,X'00')", params![draft_checkout, path])?;
        Ok(())
    }).await?;
    assert!(
        store
            .git_discard_changes(
                checkout.clone(),
                path.into(),
                None,
                preview.snapshot.clone()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("protected editor draft")
    );
    assert_eq!(
        fs::read_to_string(root.join(path))?,
        file("new external edit", "second edit")
    );
    let draft_checkout = checkout.clone();
    store
        .execute(move |db| {
            db.execute(
                "DELETE FROM drafts WHERE kind='checkout_file' AND owner_id=?1 AND document=?2",
                params![draft_checkout, path],
            )?;
            Ok(())
        })
        .await?;
    store
        .git_discard_changes(checkout.clone(), path.into(), None, preview.snapshot)
        .await?;
    assert_eq!(
        fs::read_to_string(root.join(path))?,
        file("from index", "last")
    );

    fs::remove_file(root.join("image.bin"))?;
    let preview = store
        .git_discard_preview(checkout.clone(), "image.bin".into(), None)
        .await?;
    assert!(matches!(preview.before, PreviewFile::Missing));
    assert!(matches!(preview.after, PreviewFile::Bytes { size: 4, .. }));
    store
        .git_discard_changes(checkout, "image.bin".into(), None, preview.snapshot)
        .await?;
    assert_eq!(fs::read(root.join("image.bin"))?, [1, 0, 2, 255]);
    assert_eq!(fs::read(root.join(".git/index"))?, index_before);
    Ok(())
}
