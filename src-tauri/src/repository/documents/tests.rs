use std::{fs, process::Command};

use super::*;
use crate::repository::{git_path, git_query, register};

const OLD: &str = "\u{feff}first\r\nkeep LF\nlast CR\rфинал";
const EDIT: &str = "new first\nkeep LF\nnew last\nфинал";
const RESULT: &str = "\u{feff}new first\r\nkeep LF\nnew last\rфинал";

#[tokio::test]
async fn opens_and_saves_canonical_utf8_revisions_without_changing_untouched_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("profile.sqlite"))?;
    let root = init_repository(temp.path())?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let document = open_document(&store, &checkout, "Guide.md").await?;
    assert_eq!(document.canonical_path, root.join("Guide.md"));
    assert!(document.utf8_bom);
    assert_eq!(document.line_endings, LineEndings::Mixed);
    assert_eq!(document.markdown, "first\nkeep LF\nlast CR\nфинал");
    let repeated = open_document(&store, &checkout, "Guide.md").await?;
    assert_eq!(repeated, document);

    let RepositoryDocumentSave::Saved { document: saved } = store
        .save_repository_document(
            checkout.clone(),
            "Guide.md".into(),
            document.revision.id.clone(),
            EDIT.into(),
        )
        .await?
    else {
        anyhow::bail!("replacement failed");
    };
    assert_eq!(fs::read(root.join("Guide.md"))?, RESULT.as_bytes());
    assert_eq!(saved.markdown, EDIT);
    assert_ne!(saved.revision.id, document.revision.id);
    assert_ne!(saved.file_identity, document.file_identity);
    assert_eq!(
        store
            .repository_markdown(checkout.clone(), document.revision.id.clone())
            .await?
            .unwrap()
            .markdown,
        OLD
    );
    assert_eq!(open_document(&store, &checkout, "Guide.md").await?, saved);
    assert!(fs::read_dir(&root)?.all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".omniagent-markdown-")
    }));

    fs::write(root.join("legacy.md"), b"\xff\xfeh\0i\0")?;
    assert_eq!(
        store
            .open_repository_document(checkout.clone(), "legacy.md".into())
            .await?,
        RepositoryDocumentOpen::UnsupportedEncoding
    );
    assert_eq!(
        store
            .open_repository_document(checkout.clone(), "missing.md".into())
            .await?,
        RepositoryDocumentOpen::Missing
    );
    assert!(
        store
            .save_repository_document(
                checkout.clone(),
                "other.md".into(),
                document.revision.id.clone(),
                EDIT.into()
            )
            .await
            .is_err()
    );
    let blocker = root.join("Guide.md");
    let stored = open_document(&store, &checkout, "Guide.md").await?;
    fs::write(&blocker, b"outside edit\n")?;
    let RepositoryDocumentSave::Conflict { recovery } = store
        .save_repository_document(
            checkout,
            "Guide.md".into(),
            stored.revision.id,
            "buffer".into(),
        )
        .await?
    else {
        anyhow::bail!("stale revision was not reported as a conflict");
    };
    assert_eq!(fs::read(blocker)?, b"outside edit\n");
    assert_eq!(
        fs::read(recovery.edited_path.unwrap())?,
        b"\xef\xbb\xbfbuffer"
    );
    assert!(recovery.displaced_path.is_none());
    Ok(())
}

#[tokio::test]
async fn resolves_in_checkout_junctions_and_rejects_external_documents_and_attachments()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("profile.sqlite"))?;
    let root = init_repository(temp.path())?;
    let inside = root.join("docs");
    let outside = temp.path().join("outside");
    fs::create_dir(&inside)?;
    fs::create_dir(&outside)?;
    fs::write(inside.join("inner.md"), b"inside")?;
    let pdf = b"%PDF-1.5\r\n";
    fs::write(inside.join("reference.pdf"), pdf)?;
    fs::write(outside.join("private.md"), b"outside")?;
    fs::write(outside.join("private.pdf"), pdf)?;
    junction(&root.join("alias"), &inside)?;
    junction(&root.join("escape"), &outside)?;
    let checkout = register(&store, root.clone()).await?.checkout_id;

    let document = open_document(&store, &checkout, "alias/inner.md").await?;
    assert_eq!(document.revision.relative_path, "docs/inner.md");
    assert_eq!(document.canonical_path, inside.join("inner.md"));
    let attachment = store
        .read_repository_attachment(checkout.clone(), "alias/reference.pdf".into())
        .await?
        .unwrap();
    assert_eq!(attachment.relative_path, "docs/reference.pdf");
    assert_eq!(attachment.mime_type, "application/pdf");
    assert_eq!(STANDARD.decode(attachment.content_base64)?, pdf);
    assert!(
        store
            .open_repository_document(checkout.clone(), "escape/private.md".into())
            .await
            .is_err()
    );
    assert!(
        store
            .read_repository_attachment(checkout.clone(), "escape/private.pdf".into())
            .await
            .is_err()
    );
    assert!(
        store
            .read_repository_attachment(checkout.clone(), ".git/config".into())
            .await
            .is_err()
    );
    assert!(
        store
            .read_repository_attachment(checkout.clone(), "../outside/private.pdf".into())
            .await
            .is_err()
    );
    assert!(
        store
            .read_repository_attachment(checkout.clone(), "not-yet.pdf".into())
            .await?
            .is_none()
    );
    assert!(
        store
            .save_repository_document(
                checkout,
                "escape/private.md".into(),
                document.revision.id,
                "replacement".into()
            )
            .await
            .is_err()
    );
    assert_eq!(fs::read(outside.join("private.md"))?, b"outside");
    Ok(())
}

fn init_repository(parent: &Path) -> Result<PathBuf> {
    let root = parent.join("repository");
    fs::create_dir(&root)?;
    let root = dunce::canonicalize(root)?;
    let git = git_path()?;
    git_query(&git, &root, &["init", "--quiet"])?;
    fs::write(root.join(".gitattributes"), "Guide.md -text\n")?;
    fs::write(root.join("Guide.md"), OLD)?;
    git_query(&git, &root, &["add", "."])?;
    git_query(
        &git,
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "Published guide",
        ],
    )?;
    Ok(root)
}

async fn open_document(store: &Store, checkout: &str, path: &str) -> Result<RepositoryDocument> {
    let RepositoryDocumentOpen::Document { document } = store
        .open_repository_document(checkout.into(), path.into())
        .await?
    else {
        anyhow::bail!("document {path} did not open");
    };
    Ok(document)
}

fn junction(link: &Path, target: &Path) -> Result<()> {
    let output = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()?;
    ensure!(
        output.status.success(),
        "could not create fixture junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
