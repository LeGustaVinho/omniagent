use std::{fs, path::Path, process::Command};

use anyhow::{Result, ensure};
use omniagent::{
    repository::{self, documents::RepositoryDocumentOpen},
    store::Store,
};

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    ensure!(
        output.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[tokio::test]
async fn catalog_and_embed_reads_reject_a_tracked_path_redirected_through_an_external_junction()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let checkout = fixture.path().join("checkout");
    let outside = fixture.path().join("outside");
    let redirected = checkout.join("escape");
    fs::create_dir_all(checkout.join("docs"))?;
    fs::create_dir(&outside)?;
    fs::create_dir(&redirected)?;
    let start = "![[./inside]]\n\n![[../escape/private.md]]\n\n![[/escape/private.md]]\n\n![[/escape/private.png]]\n";
    fs::write(checkout.join("docs/start.md"), start)?;
    fs::write(checkout.join("docs/inside.md"), "In-checkout embed text")?;
    fs::write(redirected.join("private.md"), "Previously in the checkout")?;
    git(&checkout, &["init", "--quiet"])?;
    git(&checkout, &["add", "--", "escape/private.md"])?;
    fs::remove_file(redirected.join("private.md"))?;
    fs::remove_dir(&redirected)?;
    let private_note = "---\naliases: [Outside alias]\n---\nOutside private text";
    fs::write(outside.join("private.md"), private_note)?;
    fs::write(outside.join("private.png"), b"outside attachment bytes")?;
    let link = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&redirected)
        .arg(&outside)
        .output()?;
    ensure!(
        link.status.success(),
        "fixture junction failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert_eq!(
        fs::read_to_string(redirected.join("private.md"))?,
        private_note
    );
    assert_eq!(
        git(
            &checkout,
            &["ls-files", "--cached", "--", "escape/private.md"]
        )?
        .trim(),
        "escape/private.md"
    );

    let store = Store::open(fixture.path().join("profile.sqlite"))?;
    let selected = repository::register(&store, checkout).await?.checkout_id;
    let catalog = store.repository_note_catalog(selected.clone()).await?;
    let paths = catalog
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["docs/inside.md", "docs/start.md"]);
    let opening = store
        .open_repository_document(selected.clone(), "docs/start.md".into())
        .await?;
    let RepositoryDocumentOpen::Document { document } = opening else {
        anyhow::bail!("fixture note did not open: {opening:?}");
    };
    assert_eq!(document.markdown, start);
    let opening = store
        .open_repository_document(selected.clone(), "docs/inside.md".into())
        .await?;
    let RepositoryDocumentOpen::Document { document } = opening else {
        anyhow::bail!("in-checkout embed did not open: {opening:?}");
    };
    assert_eq!(document.markdown, "In-checkout embed text");

    assert!(
        store
            .open_repository_document(selected.clone(), "escape/private.md".into())
            .await
            .is_err()
    );
    assert!(
        store
            .read_repository_attachment(selected.clone(), "escape/private.png".into())
            .await
            .is_err()
    );
    let revisions = store.repository_markdown_revisions(selected).await?;
    assert_eq!(revisions.len(), 2);
    assert!(
        revisions
            .iter()
            .all(|revision| revision.relative_path.starts_with("docs/"))
    );
    Ok(())
}
