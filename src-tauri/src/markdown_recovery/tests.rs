use std::{env, fs, path::Path, process::Command, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::sync::watch;

use crate::{
    repository::{
        self,
        documents::{RepositoryDocument, RepositoryDocumentOpen, RepositoryDocumentSave},
    },
    store::Store,
};

use super::RepositoryMarkdownInvalidation;

const NOTE_PATH: &str = "notes/note.md";
const BASE_TEXT: &str = "Original revision ☕\r\n";
const BUFFER: &str = "Equipe — rascunho 📝\n";
const EXTERNAL: &str = "External disk revision\r\n";

fn create_checkout(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("notes"))?;
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()?
            .success()
    );
    fs::write(root.join(NOTE_PATH), BASE_TEXT)?;
    Ok(())
}

async fn open_note(store: &Store, checkout: &str) -> Result<RepositoryDocument> {
    match store
        .open_repository_document(checkout.into(), NOTE_PATH.into())
        .await?
    {
        RepositoryDocumentOpen::Document { document } => Ok(document),
        other => bail!("the fixture note was not editable: {other:?}"),
    }
}

#[tokio::test]
async fn recovers_persisted_buffer_and_pinned_base_after_unexpected_process_exit() -> Result<()> {
    const CHILD_FIXTURE: &str = "OMNIAGENT_TEST_MARKDOWN_RECOVERY_ROOT";
    const CHILD_EXIT: i32 = 71;
    if let Some(root) = env::var_os(CHILD_FIXTURE) {
        let fixture = std::path::PathBuf::from(root);
        let checkout = fixture.join("checkout");
        let store = Store::open(fixture.join("app.sqlite"))?;
        let registration = repository::register(&store, checkout.clone()).await?;
        let original = open_note(&store, &registration.checkout_id).await?;
        store
            .save_repository_markdown_draft(
                registration.checkout_id,
                original.revision.id,
                None,
                BUFFER.into(),
            )
            .await?;
        // Exit without unwinding or giving the SQLite writer a chance to shut down cleanly.
        std::process::exit(CHILD_EXIT);
    }

    let fixture = tempfile::tempdir()?;
    let path = fixture.path().join("checkout");
    create_checkout(&path)?;
    let child = Command::new(env::current_exe()?)
        .args(["--exact", "markdown_recovery::tests::recovers_persisted_buffer_and_pinned_base_after_unexpected_process_exit", "--nocapture"])
        .env(CHILD_FIXTURE, fixture.path())
        .output()?;
    assert_eq!(
        child.status.code(),
        Some(CHILD_EXIT),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );

    fs::write(path.join(NOTE_PATH), EXTERNAL)?;
    let store = Store::open(fixture.path().join("app.sqlite"))?;
    let checkout = repository::register(&store, path.clone())
        .await?
        .checkout_id;
    let draft = store
        .repository_markdown_draft(checkout.clone(), NOTE_PATH.into())
        .await?
        .context("the dirty buffer was lost after restart")?;
    assert_eq!(draft.markdown, BUFFER);
    assert_eq!(
        store
            .repository_markdown_draft_paths(checkout.clone())
            .await?,
        [NOTE_PATH]
    );
    let external = open_note(&store, &checkout).await?;
    assert_ne!(draft.base.sha256, external.revision.sha256);
    assert_eq!(external.markdown, EXTERNAL.replace("\r\n", "\n"));
    let original = store
        .repository_markdown(checkout.clone(), draft.base.id.clone())
        .await?
        .context("the original Markdown revision was lost")?;
    assert_eq!(original.markdown, BASE_TEXT);
    assert_eq!(draft.base, original.revision);

    let protected: Vec<u8> = store
        .execute(move |db| {
            Ok(db.query_row(
                "SELECT payload FROM drafts WHERE kind = 'checkout_file'",
                [],
                |row| row.get(0),
            )?)
        })
        .await?;
    let plaintext = BUFFER.trim_end().as_bytes();
    assert!(
        !protected
            .windows(plaintext.len())
            .any(|window| window == plaintext)
    );

    let save = store
        .save_repository_document(
            checkout.clone(),
            NOTE_PATH.into(),
            draft.base.id.clone(),
            draft.markdown.clone(),
        )
        .await?;
    let RepositoryDocumentSave::Conflict { recovery } = save else {
        bail!("the old base unexpectedly permitted a write: {save:?}");
    };
    let edited_path = recovery
        .edited_path
        .context("the conflict did not keep the edited bytes")?;
    let encoded = BUFFER.replace('\n', "\r\n");
    assert_eq!(fs::read_to_string(edited_path)?, encoded);
    assert_eq!(fs::read_to_string(path.join(NOTE_PATH))?, EXTERNAL);
    assert_eq!(
        store
            .repository_markdown_draft(checkout.clone(), NOTE_PATH.into())
            .await?,
        Some(draft.clone())
    );

    let save = store
        .save_repository_document(
            checkout.clone(),
            NOTE_PATH.into(),
            external.revision.id.clone(),
            draft.markdown.clone(),
        )
        .await?;
    let RepositoryDocumentSave::Saved { document } = save else {
        bail!("saving against the inspected external revision failed: {save:?}");
    };
    assert_eq!(document.markdown, BUFFER);
    assert_eq!(fs::read_to_string(path.join(NOTE_PATH))?, encoded);
    assert!(
        store
            .discard_repository_markdown_draft(checkout.clone(), NOTE_PATH.into(), draft.revision)
            .await?
    );
    assert_eq!(
        store
            .repository_markdown(checkout.clone(), draft.base.id)
            .await?
            .unwrap()
            .markdown,
        BASE_TEXT
    );
    assert_eq!(
        store
            .repository_markdown(checkout, external.revision.id)
            .await?
            .unwrap()
            .markdown,
        EXTERNAL
    );
    Ok(())
}

#[tokio::test]
async fn isolates_drafts_by_checkout_and_guards_stale_updates_and_discard() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let path = fixture.path().join("checkout");
    let other = fixture.path().join("other");
    create_checkout(&path)?;
    create_checkout(&other)?;
    let store = Store::open(fixture.path().join("app.sqlite"))?;
    let checkout = repository::register(&store, path.clone())
        .await?
        .checkout_id;
    let other_checkout = repository::register(&store, other).await?.checkout_id;
    let base = store
        .capture_repository_markdown(checkout.clone(), path.join(NOTE_PATH))
        .await?
        .revision;
    let first = store
        .save_repository_markdown_draft(checkout.clone(), base.id.clone(), None, "".into())
        .await?;
    assert_eq!(first.markdown, "");
    assert!(
        store
            .repository_markdown_draft_paths(other_checkout.clone())
            .await?
            .is_empty()
    );
    assert!(
        store
            .save_repository_markdown_draft(
                other_checkout.clone(),
                base.id.clone(),
                None,
                BUFFER.into()
            )
            .await
            .is_err()
    );
    assert!(
        !store
            .discard_repository_markdown_draft(
                other_checkout,
                NOTE_PATH.into(),
                first.revision.clone()
            )
            .await?
    );
    let second = store
        .save_repository_markdown_draft(
            checkout.clone(),
            base.id.clone(),
            Some(first.revision.clone()),
            BUFFER.into(),
        )
        .await?;
    assert_ne!(second.revision, first.revision);
    assert!(
        store
            .save_repository_markdown_draft(
                checkout.clone(),
                base.id,
                Some(first.revision.clone()),
                "stale write".into()
            )
            .await
            .is_err()
    );
    assert!(
        !store
            .discard_repository_markdown_draft(checkout.clone(), NOTE_PATH.into(), first.revision)
            .await?
    );
    assert_eq!(
        store
            .repository_markdown_draft(checkout.clone(), NOTE_PATH.into())
            .await?,
        Some(second.clone())
    );
    assert!(
        store
            .discard_repository_markdown_draft(checkout.clone(), NOTE_PATH.into(), second.revision)
            .await?
    );
    assert!(
        store
            .repository_markdown_draft_paths(checkout)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn observes_external_replacement_and_ancestor_rename_for_a_pinned_document() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let path = fixture.path().join("checkout");
    create_checkout(&path)?;
    let store = Store::open(fixture.path().join("app.sqlite"))?;
    let checkout = repository::register(&store, path.clone())
        .await?
        .checkout_id;
    let base = open_note(&store, &checkout).await?.revision;
    let (signal, mut changes) = watch::channel(None);
    let _watcher = store
        .watch_repository_markdown(checkout.clone(), base.id.clone(), move |event| {
            signal.send_replace(Some(event));
        })
        .await?;

    let destination = path.join(NOTE_PATH);
    let stage = tempfile::NamedTempFile::new_in(destination.parent().unwrap())?;
    fs::write(stage.path(), EXTERNAL)?;
    stage.persist(&destination)?;
    tokio::time::timeout(Duration::from_secs(10), changes.changed()).await??;
    let expected = Some(RepositoryMarkdownInvalidation {
        checkout_id: checkout.clone(),
        relative_path: NOTE_PATH.into(),
    });
    assert_eq!(*changes.borrow_and_update(), expected);
    let current = open_note(&store, &checkout).await?;
    assert_ne!(current.revision.sha256, base.sha256);
    assert_eq!(current.markdown, EXTERNAL.replace("\r\n", "\n"));

    fs::rename(path.join("notes"), path.join("moved"))?;
    tokio::time::timeout(Duration::from_secs(10), changes.changed()).await??;
    assert_eq!(*changes.borrow(), expected);
    assert!(!destination.exists());
    assert!(matches!(
        store
            .open_repository_document(checkout.clone(), NOTE_PATH.into())
            .await?,
        RepositoryDocumentOpen::Missing
    ));
    assert_eq!(
        store
            .repository_markdown(checkout, base.id)
            .await?
            .unwrap()
            .markdown,
        BASE_TEXT
    );
    Ok(())
}
