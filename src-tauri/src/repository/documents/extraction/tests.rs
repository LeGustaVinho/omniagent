use std::{fs, process::Command};

use anyhow::{Context, Result};

use crate::repository::{self, documents::RepositoryDocumentOpen};

use super::*;

const NOTE: &str = "notes/Source.md";
const OTHER: &str = "notes/Already exists.md";
const CLEAN: &str = "\u{feff}# Cobertura\r\n\r\n🙂 primeiro\r\n\n## Final\r\n";
const DIRTY: &str = "# Cobertura\n\n🙂 texto\n🙂 texto\n\n## Final\n";

async fn fixture() -> Result<(tempfile::TempDir, Store, repository::ProjectCheckout)> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("notes"))?;
    anyhow::ensure!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    fs::write(root.join(NOTE), CLEAN)?;
    fs::write(root.join(OTHER), "Existing file.\r\n")?;
    let store = Store::open(temp.path().join("app.sqlite"))?;
    let checkout = repository::register(&store, root).await?;
    Ok((temp, store, checkout))
}

async fn request(
    store: &Store,
    checkout: &str,
    selected_text: &str,
    before: &str,
) -> Result<ExtractRepositoryNote> {
    let RepositoryDocumentOpen::Document { document } = store
        .open_repository_document(checkout.into(), NOTE.into())
        .await?
    else {
        anyhow::bail!("Fixture is not editable")
    };
    let from = before.encode_utf16().count();
    Ok(ExtractRepositoryNote {
        path: NOTE.into(),
        base_revision: document.revision.id,
        file_identity: document.file_identity,
        expected_draft_revision: None,
        from,
        to: from + selected_text.encode_utf16().count(),
        selected_text: selected_text.into(),
        relative_directory: String::new(),
        name: "Detalhes [QA] #1.md".into(),
        replace_with_link: false,
    })
}

#[tokio::test]
async fn extracts_exact_clean_markdown_bytes_without_touching_source() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let root = Path::new(&checkout.checkout_root);
    let picked = "🙂 primeiro\n\n";
    let request = request(&store, &checkout.checkout_id, picked, "# Cobertura\n\n").await?;
    let output = store
        .extract_repository_note_selection(checkout.checkout_id.clone(), request)
        .await?;
    assert_eq!(output.path, "Detalhes [QA] #1.md");
    assert!(output.replacement.is_none());
    assert_eq!(
        fs::read_to_string(root.join(&output.path))?,
        "🙂 primeiro\r\n\n"
    );
    assert_eq!(fs::read_to_string(root.join(NOTE))?, CLEAN);
    assert!(
        store
            .repository_markdown_draft(checkout.checkout_id, NOTE.into())
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn replaces_only_the_selected_duplicate_and_preserves_the_dirty_source_draft() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let root = Path::new(&checkout.checkout_root);
    let picked = "🙂 texto\n";
    let mut request = request(
        &store,
        &checkout.checkout_id,
        picked,
        "# Cobertura\n\n🙂 texto\n",
    )
    .await?;
    let draft = store
        .save_repository_markdown_draft(
            checkout.checkout_id.clone(),
            request.base_revision.clone(),
            None,
            DIRTY.into(),
        )
        .await?;
    request.expected_draft_revision = Some(draft.revision);
    request.replace_with_link = true;
    let output = store
        .extract_repository_note_selection(checkout.checkout_id.clone(), request)
        .await?;
    assert_eq!(fs::read_to_string(root.join(&output.path))?, "🙂 texto\r\n");
    let replacement = output.replacement.context("Replacement was omitted")?;
    assert_eq!(
        replacement.markdown,
        "# Cobertura\n\n🙂 texto\n[Detalhes \\[QA\\] \\#1](../Detalhes%20%5BQA%5D%20%231.md)\n\n## Final\n"
    );
    let persisted = store
        .repository_markdown_draft(checkout.checkout_id, NOTE.into())
        .await?
        .context("Replacement was not persisted")?;
    assert_eq!(persisted.markdown, replacement.markdown);
    assert_eq!(Some(persisted.revision), replacement.draft_revision);
    assert_eq!(fs::read_to_string(root.join(NOTE))?, CLEAN);
    Ok(())
}

#[tokio::test]
async fn rejects_collision_and_stale_draft_without_touching_source_or_destination() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let root = Path::new(&checkout.checkout_root);
    let picked = "🙂 texto\n";
    let mut collision = request(&store, &checkout.checkout_id, picked, "# Cobertura\n\n").await?;
    let draft = store
        .save_repository_markdown_draft(
            checkout.checkout_id.clone(),
            collision.base_revision.clone(),
            None,
            DIRTY.into(),
        )
        .await?;
    collision.expected_draft_revision = Some(draft.revision.clone());
    collision.relative_directory = "notes".into();
    collision.name = "Already exists.md".into();
    collision.replace_with_link = true;
    let error = store
        .extract_repository_note_selection(checkout.checkout_id.clone(), collision)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("without replacing an existing item"));
    assert_eq!(fs::read_to_string(root.join(OTHER))?, "Existing file.\r\n");

    let mut stale = request(&store, &checkout.checkout_id, picked, "# Cobertura\n\n").await?;
    stale.replace_with_link = true;
    let new_note = root.join(&stale.name);
    let error = store
        .extract_repository_note_selection(checkout.checkout_id.clone(), stale)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("source draft changed"));
    assert!(!new_note.exists());
    assert_eq!(fs::read_to_string(root.join(NOTE))?, CLEAN);
    assert_eq!(
        store
            .repository_markdown_draft(checkout.checkout_id, NOTE.into())
            .await?,
        Some(draft)
    );
    Ok(())
}

#[tokio::test]
async fn rolls_back_the_created_file_on_transaction_commit_failure() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let root = Path::new(&checkout.checkout_root);
    let mut request = request(
        &store,
        &checkout.checkout_id,
        "🙂 primeiro",
        "# Cobertura\n\n",
    )
    .await?;
    request.replace_with_link = true;
    let target = root.join(&request.name);
    store.execute(|db| {
        db.execute_batch(
            "CREATE TABLE extraction_guard (id INTEGER PRIMARY KEY);
             CREATE TABLE extraction_deferred (id INTEGER REFERENCES extraction_guard(id) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER reject_extraction_commit AFTER INSERT ON drafts
             WHEN new.kind = 'checkout_file'
             BEGIN INSERT INTO extraction_deferred VALUES (1); END;"
        )?;
        Ok(())
    }).await?;
    let error = store
        .extract_repository_note_selection(checkout.checkout_id.clone(), request)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("Your original selection and draft were preserved"));
    assert!(!target.exists());
    assert_eq!(fs::read_to_string(root.join(NOTE))?, CLEAN);
    assert!(
        store
            .repository_markdown_draft(checkout.checkout_id, NOTE.into())
            .await?
            .is_none()
    );
    Ok(())
}
