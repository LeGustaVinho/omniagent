use std::{fs, process::Command};

use super::replacement::ReplacementStep;
use super::*;
use crate::repository::{git_path, git_query, provision_task_worktree, register};

const OLD: &str = "\u{feff}first\r\nkeep LF\nlast CR\rфинал";
const EDIT: &str = "new first\nkeep LF\nnew last\nфинал";
const RESULT: &str = "\u{feff}new first\r\nkeep LF\nnew last\rфинал";

#[tokio::test]
async fn reviews_byte_exact_source_conversion_and_keeps_ordinary_saves_and_history_compatible()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("format.sqlite");
    let store = Store::open(profile.clone())?;
    let root = init_repository(temp.path())?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let path = "format.txt";
    let file = root.join(path);
    let original = "\u{feff}A\r\né\r😀\n";
    let converted_bytes = [
        0xfe, 0xff, 0, 0x41, 0, 0x0a, 0, 0xe9, 0, 0x0a, 0xd8, 0x3d, 0xde, 0x00, 0, 0x0a,
    ];
    fs::write(&file, original)?;
    let RepositoryDocumentOpen::Document { document: first } = store
        .open_repository_source_file(checkout.clone(), path.into())
        .await?
    else {
        anyhow::bail!("UTF-8 source did not open")
    };
    assert_eq!(first.encoding, TextEncoding::Utf8Bom);
    assert_eq!(first.line_endings, LineEndings::Mixed);
    let change = SourceFormatChange {
        encoding: Some(TextEncoding::Utf16Be),
        line_endings: Some(LineEndings::Lf),
    };
    let preview = store
        .preview_repository_source_format(
            checkout.clone(),
            path.into(),
            first.revision.id.clone(),
            change,
        )
        .await?;
    assert_eq!(preview.original_encoding, TextEncoding::Utf8Bom);
    assert_eq!(preview.original_line_endings, LineEndings::Mixed);
    assert_eq!(preview.encoding, TextEncoding::Utf16Be);
    assert_eq!(preview.line_endings, LineEndings::Lf);
    assert_eq!(preview.original_bytes, original.len());
    assert_eq!(preview.converted_bytes, converted_bytes.len());
    assert_eq!(
        preview.converted_prefix,
        "FE FF 00 41 00 0A 00 E9 00 0A D8 3D DE 00 00 0A"
    );
    assert_eq!(preview.sha256, hash(&converted_bytes));
    assert_eq!(fs::read(&file)?, original.as_bytes());
    assert!(
        store
            .save_repository_source_format(
                checkout.clone(),
                path.into(),
                first.revision.id.clone(),
                change,
                "unreviewed".into()
            )
            .await
            .is_err()
    );
    let RepositoryDocumentSave::Saved {
        document: converted,
    } = store
        .save_repository_source_format(
            checkout.clone(),
            path.into(),
            first.revision.id,
            change,
            preview.sha256,
        )
        .await?
    else {
        anyhow::bail!("conversion failed")
    };
    assert_eq!(fs::read(&file)?, converted_bytes);
    assert_eq!(converted.revision.sha256, hash(&converted_bytes));
    assert_eq!(converted.markdown, first.markdown);
    drop(store);

    let store = Store::open(profile)?;
    let RepositoryDocumentOpen::Document { document: reopened } = store
        .open_repository_source_file(checkout.clone(), path.into())
        .await?
    else {
        anyhow::bail!("UTF-16 source did not reopen")
    };
    assert_eq!(reopened, converted);
    let RepositoryDocumentSave::Saved { document: edited } = store
        .save_repository_source_file(
            checkout.clone(),
            path.into(),
            reopened.revision.id,
            "B\né\n😀\n".into(),
        )
        .await?
    else {
        anyhow::bail!("ordinary UTF-16 save failed")
    };
    let mut edited_bytes = converted_bytes;
    edited_bytes[3] = 0x42;
    assert_eq!(fs::read(&file)?, edited_bytes);
    assert_eq!(edited.encoding, TextEncoding::Utf16Be);
    assert_eq!(
        store
            .repository_source_revision(checkout.clone(), edited.revision.id)
            .await?
            .unwrap()
            .markdown,
        "\u{feff}B\né\n😀\n"
    );

    let history = store
        .checkout_document_history(checkout.clone(), path.into(), None)
        .await?;
    assert_eq!(history.entries.len(), 3);
    for (index, encoding, endings, text) in [
        (0, TextEncoding::Utf16Be, LineEndings::Lf, "B\né\n😀\n"),
        (1, TextEncoding::Utf16Be, LineEndings::Lf, "A\né\n😀\n"),
        (2, TextEncoding::Utf8Bom, LineEndings::Mixed, "A\né\n😀\n"),
    ] {
        let entry = store
            .checkout_document_history_text(
                checkout.clone(),
                path.into(),
                history.entries[index].position,
            )
            .await?;
        assert_eq!(
            (entry.encoding, entry.line_endings, entry.text.as_str()),
            (encoding, endings, text)
        );
    }
    Ok(())
}

#[tokio::test]
async fn format_conversion_preserves_drafts_and_rejects_changed_disk_or_unsupported_bytes()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("profile.sqlite"))?;
    let root = init_repository(temp.path())?;
    let checkout = register(&store, root.clone()).await?.checkout_id;
    let path = "format.txt";
    let file = root.join(path);
    fs::write(&file, b"left\r\nright\n")?;
    let RepositoryDocumentOpen::Document { document: first } = store
        .open_repository_source_file(checkout.clone(), path.into())
        .await?
    else {
        anyhow::bail!("source did not open")
    };
    let change = SourceFormatChange {
        encoding: Some(TextEncoding::Utf16Le),
        line_endings: None,
    };
    let preview = store
        .preview_repository_source_format(
            checkout.clone(),
            path.into(),
            first.revision.id.clone(),
            change,
        )
        .await?;
    assert_eq!(
        preview.converted_prefix,
        "FF FE 6C 00 65 00 66 00 74 00 0D 00 0A 00 72 00 69 00 67 00 68 00 74 00"
    );
    assert_eq!(preview.line_endings, LineEndings::Mixed);
    let draft = store
        .save_repository_source_draft(
            checkout.clone(),
            first.revision.id.clone(),
            None,
            "draft".into(),
        )
        .await?;
    assert!(
        store
            .save_repository_source_format(
                checkout.clone(),
                path.into(),
                first.revision.id.clone(),
                change,
                preview.sha256.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .repository_source_draft(checkout.clone(), path.into())
            .await?
            .unwrap()
            .markdown,
        "draft"
    );
    assert_eq!(fs::read(&file)?, b"left\r\nright\n");
    assert!(
        store
            .discard_repository_source_draft(checkout.clone(), path.into(), draft.revision)
            .await?
    );
    fs::write(&file, b"third-party edit")?;
    assert!(
        store
            .preview_repository_source_format(
                checkout.clone(),
                path.into(),
                first.revision.id.clone(),
                change
            )
            .await
            .is_err()
    );
    let stale = store
        .save_repository_source_format(
            checkout.clone(),
            path.into(),
            first.revision.id,
            change,
            preview.sha256,
        )
        .await?;
    assert!(matches!(stale, RepositoryDocumentSave::Conflict { .. }));
    assert_eq!(fs::read(&file)?, b"third-party edit");

    for malformed in [
        b"\xff\xfeA".as_slice(),
        b"\xfe\xff\xd8\x00".as_slice(),
        b"\xff\xfe\0\0".as_slice(),
        b"\x80".as_slice(),
    ] {
        fs::write(&file, malformed)?;
        assert_eq!(
            store
                .open_repository_source_file(checkout.clone(), path.into())
                .await?,
            RepositoryDocumentOpen::UnsupportedEncoding
        );
    }
    Ok(())
}

#[tokio::test]
async fn records_local_saves_and_keeps_original_and_returned_versions_across_restart() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("history.sqlite");
    let checkout;
    let other;
    {
        let store = Store::open(profile.clone())?;
        let root = init_repository(temp.path())?;
        checkout = register(&store, root.clone()).await?.checkout_id;
        let other_root = temp.path().join("alternate");
        git_query(
            &git_path()?,
            &root,
            &["worktree", "add", "--detach", other_root.to_str().unwrap()],
        )?;
        other = register(&store, other_root).await?.checkout_id;
        let first = open_document(&store, &checkout, "Guide.md").await?;
        let RepositoryDocumentSave::Saved { document: edited } = store
            .save_repository_document(
                checkout.clone(),
                "Guide.md".into(),
                first.revision.id.clone(),
                EDIT.into(),
            )
            .await?
        else {
            anyhow::bail!("initial edit failed");
        };
        let RepositoryDocumentSave::Saved { document: restored } = store
            .save_repository_document(
                checkout.clone(),
                "Guide.md".into(),
                edited.revision.id.clone(),
                first.markdown.clone(),
            )
            .await?
        else {
            anyhow::bail!("restoring original text failed");
        };
        assert_eq!(restored.revision.id, first.revision.id);
        let noop = store
            .save_repository_document(
                checkout.clone(),
                "Guide.md".into(),
                restored.revision.id,
                first.markdown,
            )
            .await?;
        assert!(matches!(noop, RepositoryDocumentSave::Saved { .. }));

        fs::write(root.join("Guide.md"), "External edit\n")?;
        let conflict = store
            .save_repository_document(
                checkout.clone(),
                "Guide.md".into(),
                edited.revision.id,
                "stale edit".into(),
            )
            .await?;
        assert!(matches!(conflict, RepositoryDocumentSave::Conflict { .. }));
        let external = open_document(&store, &checkout, "Guide.md").await?;
        let saved = store
            .save_repository_document(
                checkout.clone(),
                "Guide.md".into(),
                external.revision.id,
                "After external edit\n".into(),
            )
            .await?;
        assert!(matches!(saved, RepositoryDocumentSave::Saved { .. }));

        fs::write(root.join("example.rs"), "fn before() {}\n")?;
        let RepositoryDocumentOpen::Document { document: source } = store
            .open_repository_source_file(checkout.clone(), "example.rs".into())
            .await?
        else {
            anyhow::bail!("source did not open");
        };
        let saved = store
            .save_repository_source_file(
                checkout.clone(),
                "example.rs".into(),
                source.revision.id,
                "fn after() {}\n".into(),
            )
            .await?;
        assert!(matches!(saved, RepositoryDocumentSave::Saved { .. }));
    }

    let store = Store::open(profile)?;
    let history = store
        .checkout_document_history(checkout.clone(), "Guide.md".into(), None)
        .await?;
    assert_eq!(
        history
            .entries
            .iter()
            .map(|entry| entry.origin.as_str())
            .collect::<Vec<_>>(),
        ["saved", "before_save", "saved", "saved", "before_save"]
    );
    assert_eq!(
        history.entries[2].revision_id,
        history.entries[4].revision_id
    );
    let initial = history.entries[4].position;
    let text = store
        .checkout_document_history_text(checkout.clone(), "Guide.md".into(), initial)
        .await?;
    assert_eq!(text.text, "first\nkeep LF\nlast CR\nфинал");
    assert!(text.utf8_bom);
    assert_eq!(text.line_endings, LineEndings::Mixed);
    assert_eq!(
        store
            .checkout_document_history(checkout.clone(), "example.rs".into(), None)
            .await?
            .entries
            .len(),
        2
    );
    assert!(
        store
            .checkout_document_history_text(checkout.clone(), "example.rs".into(), initial)
            .await
            .is_err()
    );
    assert!(
        store
            .checkout_document_history_text(other, "Guide.md".into(), initial)
            .await
            .is_err()
    );
    assert!(
        store
            .checkout_document_history(checkout, "../Guide.md".into(), None)
            .await
            .is_err()
    );
    Ok(())
}

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

#[test]
fn detects_a_post_precheck_change_and_preserves_both_versions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    let destination = root.join("Document.md");
    fs::write(&destination, b"old")?;
    let result = replace(
        &root,
        &destination,
        &hash(b"old"),
        b"edited",
        1024,
        |step| {
            if step == ReplacementStep::BeforeDisplacement {
                fs::write(&destination, b"concurrent writer")?;
            }
            Ok(())
        },
    )?;
    let FileSave::Conflict(recovery) = result else {
        anyhow::bail!("the displaced bytes did not trigger conflict recovery");
    };
    assert_eq!(
        fs::read(recovery.displaced_path.unwrap())?,
        b"concurrent writer"
    );
    assert_eq!(fs::read(recovery.edited_path.unwrap())?, b"edited");
    assert_eq!(fs::read(destination)?, b"concurrent writer");
    Ok(())
}

#[test]
fn retains_recoverable_edit_when_replacement_cannot_be_performed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    let destination = root.join("Document.md");
    fs::write(&destination, b"old")?;
    let blocker = open_locked_local_file(&destination)?;
    let FileSave::RecoveryRequired(recovery) =
        replace(&root, &destination, &hash(b"old"), b"edited", 1024, |_| {
            Ok(())
        })?
    else {
        anyhow::bail!("a write-sharing violation was not reported");
    };
    assert_eq!(fs::read(recovery.edited_path.unwrap())?, b"edited");
    assert!(recovery.displaced_path.is_none());
    assert_eq!(fs::read(&destination)?, b"old");
    drop(blocker);
    Ok(())
}

#[test]
fn preserves_both_backups_if_another_writer_takes_the_destination() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    let folder = root.join("notes");
    fs::create_dir(&folder)?;
    let destination = folder.join("Document.md");
    fs::write(&destination, b"old")?;
    let result = replace(
        &root,
        &destination,
        &hash(b"old"),
        b"edited",
        1024,
        |step| {
            assert!(fs::rename(&folder, root.join("moved")).is_err());
            if step == ReplacementStep::BeforeInstall {
                fs::write(&destination, b"other writer")?;
            }
            Ok(())
        },
    )?;
    let FileSave::RecoveryRequired(recovery) = result else {
        anyhow::bail!("the newly created destination was overwritten");
    };
    assert_eq!(fs::read(destination)?, b"other writer");
    assert_eq!(fs::read(recovery.displaced_path.unwrap())?, b"old");
    assert_eq!(fs::read(recovery.edited_path.unwrap())?, b"edited");
    Ok(())
}

#[tokio::test]
async fn managed_checkout_respects_its_existing_writer_reservation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("profile.sqlite"))?;
    let project = register(&store, init_repository(temp.path())?).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Guide edits".into())
        .await?;
    let worktree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        task.id.clone(),
        project.checkout_id,
    )
    .await?;
    let checkout = worktree.checkout_id.clone();
    let document = open_document(&store, &checkout, "Guide.md").await?;
    let (entered_signal, entered) = tokio::sync::oneshot::channel();
    let (release, release_signal) = std::sync::mpsc::channel();
    let blocker = tokio::spawn({
        let (store, checkout) = (store.clone(), checkout.clone());
        async move {
            with_managed_worktree_writer(&store, project.project_id, checkout, task.id, move |_| {
                entered_signal
                    .send(())
                    .map_err(|()| anyhow::anyhow!("writer observer closed"))?;
                release_signal.recv()?;
                Ok(())
            })
            .await
        }
    });
    entered.await?;
    let result = store
        .save_repository_document(
            checkout.clone(),
            "Guide.md".into(),
            document.revision.id.clone(),
            EDIT.into(),
        )
        .await;
    release.send(())?;
    blocker.await??;
    assert!(result.is_err(), "editor bypassed the reserved writer");
    let before = fs::read(worktree.root.join("Guide.md"))?;
    assert_eq!(before, OLD.as_bytes());
    let result = store
        .save_repository_document(
            checkout,
            "Guide.md".into(),
            document.revision.id,
            EDIT.into(),
        )
        .await?;
    assert!(
        matches!(result, RepositoryDocumentSave::Saved { .. }),
        "{result:?}"
    );
    assert_eq!(fs::read(worktree.root.join("Guide.md"))?, RESULT.as_bytes());
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
