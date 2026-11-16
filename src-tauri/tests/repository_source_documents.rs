use std::{fs, path::Path, process::Command, time::Duration};

use anyhow::{Result, bail, ensure};
use omniagent::{
    repository::{
        self,
        documents::{
            LineEndings, RepositoryDocument, RepositoryDocumentOpen, RepositoryDocumentSave,
        },
        provision_task_worktree, with_managed_worktree_writer,
    },
    store::Store,
};

const PATH: &str = "src/review.rs";
const ORIGINAL: &str = "\u{feff}fn first() {}\r\n// Café 😀\nfn last() {}\r\n";
const EXTERNAL: &str = "\u{feff}fn first() {}\r\n// Café externo\nfn last() {}\r\n";
const EDITED: &str = "fn updated() {}\n// Café 😀\nfn last() {}\n";
const SAVED: &str = "\u{feff}fn updated() {}\r\n// Café 😀\nfn last() {}\r\n";

#[tokio::test]
async fn checkout_source_drafts_and_writes_share_the_exact_revision_and_writer_scope() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let primary = temp.path().join("repository");
    fs::create_dir_all(primary.join("src"))?;
    git(&primary, &["init", "--quiet"])?;
    fs::write(primary.join(".gitignore"), "ignored.txt\n")?;
    fs::write(primary.join(".gitattributes"), "*.rs -text\n")?;
    fs::write(primary.join(PATH), ORIGINAL)?;
    fs::write(primary.join("notes.md"), "# Notes only\n")?;
    git(&primary, &["add", "."])?;
    git(
        &primary,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--no-gpg-sign",
            "--quiet",
            "-m",
            "Add a source document",
        ],
    )?;
    fs::write(primary.join("ignored.txt"), "excluded")?;
    fs::write(primary.join("binary.dat"), b"\xff\x00")?;

    let database = temp.path().join("profile.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = repository::register(&store, primary.clone()).await?;
    assert_eq!(
        store
            .list_repository_source_files(checkout.checkout_id.clone())
            .await?,
        [
            ".gitattributes",
            ".gitignore",
            "binary.dat",
            "notes.md",
            PATH,
        ]
    );
    let opened = open(&store, &checkout.checkout_id, "SRC/REVIEW.RS").await?;
    assert_eq!(opened.revision.relative_path, PATH);
    assert!(opened.utf8_bom);
    assert_eq!(opened.line_endings, LineEndings::Mixed);
    assert!(matches!(
        store
            .open_repository_source_file(checkout.checkout_id.clone(), "binary.dat".into())
            .await?,
        RepositoryDocumentOpen::UnsupportedEncoding
    ));
    assert!(
        store
            .open_repository_source_file(checkout.checkout_id.clone(), "notes.md".into())
            .await
            .is_err()
    );
    assert!(
        store
            .open_repository_source_file(checkout.checkout_id.clone(), "../outside.rs".into())
            .await
            .is_err()
    );
    let draft = store
        .save_repository_source_draft(
            checkout.checkout_id.clone(),
            opened.revision.id.clone(),
            None,
            EDITED.into(),
        )
        .await?;
    assert!(
        store
            .repository_markdown(checkout.checkout_id.clone(), opened.revision.id.clone())
            .await?
            .is_none()
    );
    assert!(
        store
            .repository_markdown_revisions(checkout.checkout_id.clone())
            .await?
            .is_empty()
    );
    assert!(
        store
            .repository_markdown_draft_paths(checkout.checkout_id.clone())
            .await?
            .is_empty()
    );
    assert!(
        store
            .save_repository_markdown_draft(
                checkout.checkout_id.clone(),
                opened.revision.id.clone(),
                None,
                "Wrong editor".into()
            )
            .await
            .is_err()
    );
    assert_eq!(fs::read(primary.join(PATH))?, ORIGINAL.as_bytes());

    let (send, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    let watcher = store
        .watch_repository_source(
            checkout.checkout_id.clone(),
            opened.revision.id.clone(),
            move |notice| {
                let _ = send.send(notice);
            },
        )
        .await?;
    fs::write(primary.join(PATH), EXTERNAL)?;
    let notice = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await?
        .expect("watcher closed");
    assert_eq!(notice.checkout_id, checkout.checkout_id);
    assert_eq!(notice.relative_path, PATH);
    drop(watcher);
    drop(store);

    let store = Store::open(database)?;
    assert_eq!(
        store
            .repository_source_draft_paths(checkout.checkout_id.clone())
            .await?,
        [PATH]
    );
    assert_eq!(
        store
            .repository_source_draft(checkout.checkout_id.clone(), PATH.into())
            .await?,
        Some(draft.clone())
    );
    assert_eq!(
        store
            .repository_source_revision(checkout.checkout_id.clone(), draft.base.id.clone())
            .await?
            .unwrap()
            .markdown,
        ORIGINAL
    );
    let RepositoryDocumentSave::Conflict { recovery } = store
        .save_repository_source_file(
            checkout.checkout_id.clone(),
            PATH.into(),
            draft.base.id,
            EDITED.into(),
        )
        .await?
    else {
        bail!("stale source content did not cause a conflict")
    };
    assert_eq!(fs::read(primary.join(PATH))?, EXTERNAL.as_bytes());
    assert_eq!(
        fs::read(recovery.edited_path.expect("missing staged edit"))?,
        SAVED.as_bytes()
    );
    let external = open(&store, &checkout.checkout_id, PATH).await?;
    let repaired = store
        .save_repository_source_file(
            checkout.checkout_id.clone(),
            PATH.into(),
            external.revision.id,
            EDITED.into(),
        )
        .await?;
    assert!(
        matches!(repaired, RepositoryDocumentSave::Saved { .. }),
        "{repaired:?}"
    );
    assert_eq!(fs::read(primary.join(PATH))?, SAVED.as_bytes());

    let task = store
        .create_project_task(checkout.project_id.clone(), "Fixture review".into())
        .await?;
    let managed = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        checkout.project_id.clone(),
        task.id.clone(),
        checkout.checkout_id.clone(),
    )
    .await?;
    let scoped = open(&store, &managed.checkout_id, PATH).await?;
    assert_ne!(scoped.revision.id, opened.revision.id);
    assert!(
        store
            .save_repository_source_file(
                managed.checkout_id.clone(),
                PATH.into(),
                opened.revision.id,
                EDITED.into(),
            )
            .await
            .is_err()
    );
    let (ready_sender, ready) = tokio::sync::oneshot::channel();
    let (release, release_receiver) = std::sync::mpsc::channel();
    let owner = tokio::spawn({
        let store = store.clone();
        let checkout_id = managed.checkout_id.clone();
        async move {
            with_managed_worktree_writer(
                &store,
                checkout.project_id,
                checkout_id,
                task.id,
                move |_| {
                    let _ = ready_sender.send(());
                    release_receiver.recv()?;
                    Ok(())
                },
            )
            .await
        }
    });
    ready.await?;
    let result = store
        .save_repository_source_file(
            managed.checkout_id.clone(),
            PATH.into(),
            scoped.revision.id.clone(),
            EDITED.into(),
        )
        .await;
    release.send(())?;
    owner.await??;
    assert!(result.is_err(), "source editor bypassed the active writer");
    assert_eq!(fs::read(managed.root.join(PATH))?, ORIGINAL.as_bytes());
    let result = store
        .save_repository_source_file(
            managed.checkout_id,
            PATH.into(),
            scoped.revision.id,
            EDITED.into(),
        )
        .await?;
    assert!(
        matches!(result, RepositoryDocumentSave::Saved { .. }),
        "{result:?}"
    );
    assert_eq!(fs::read(managed.root.join(PATH))?, SAVED.as_bytes());
    Ok(())
}

async fn open(store: &Store, checkout: &str, path: &str) -> Result<RepositoryDocument> {
    match store
        .open_repository_source_file(checkout.into(), path.into())
        .await?
    {
        RepositoryDocumentOpen::Document { document } => Ok(document),
        result => bail!("could not open {path}: {result:?}"),
    }
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let result = Command::new("git.exe")
        .args(args)
        .current_dir(root)
        .output()?;
    ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
