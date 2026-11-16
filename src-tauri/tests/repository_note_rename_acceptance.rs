use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use omniagent::{
    repository::{
        self,
        documents::{RepositoryDocument, RepositoryDocumentOpen},
        markdown_index::IndexedNoteRelationships,
        markdown_links::MarkdownLinkResolution,
        markdown_renames::{
            MarkdownRenameFileResult, MarkdownRenameOutcome, MarkdownRenamePreview,
        },
    },
    store::Store,
};

struct Checkout {
    store: Store,
    id: String,
    root: PathBuf,
    _directory: tempfile::TempDir,
}

impl Checkout {
    async fn new(files: &[(&str, &str)]) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("repository");
        fs::create_dir(&root)?;
        git(&root, &["init", "--quiet"])?;
        fs::write(root.join(".gitattributes"), "* -text\n")?;
        for (path, contents) in files {
            let absolute = root.join(path);
            fs::create_dir_all(absolute.parent().context("fixture file has no parent")?)?;
            fs::write(absolute, contents)?;
        }
        git(&root, &["add", "--force", "--all"])?;
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=.git/no-hooks",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "Repository note fixture",
            ],
        )?;
        let store = Store::open(directory.path().join("profile.sqlite"))?;
        let id = repository::register(&store, root.clone())
            .await?
            .checkout_id;
        Ok(Self {
            store,
            id,
            root,
            _directory: directory,
        })
    }

    async fn open(&self, path: &str) -> Result<RepositoryDocument> {
        let RepositoryDocumentOpen::Document { document } = self
            .store
            .open_repository_document(self.id.clone(), path.into())
            .await?
        else {
            bail!("fixture note could not be opened: {path}");
        };
        Ok(document)
    }

    async fn preview(
        &self,
        source: &RepositoryDocument,
        destination: &str,
    ) -> Result<MarkdownRenamePreview> {
        self.store
            .preview_markdown_rename(
                self.id.clone(),
                source.revision.relative_path.clone().into(),
                destination.into(),
                source.revision.id.clone(),
            )
            .await
    }

    async fn reconcile(&self) -> Result<Vec<String>> {
        Ok(self
            .store
            .reconcile_repository_note_index(self.id.clone(), None)
            .await?
            .into_iter()
            .map(|note| note.path)
            .collect())
    }

    async fn search(&self, query: &str) -> Result<Vec<String>> {
        Ok(self
            .store
            .search_repository_note_index(self.id.clone(), query.into(), 10)
            .await?
            .into_iter()
            .map(|hit| hit.path)
            .collect())
    }

    async fn relationships(&self, path: &str) -> Result<IndexedNoteRelationships> {
        self.store
            .repository_note_relationships(self.id.clone(), path.into())
            .await
    }

    async fn edges(&self) -> Result<Vec<(String, String)>> {
        Ok(self
            .store
            .repository_note_edges(self.id.clone())
            .await?
            .into_iter()
            .map(|edge| (edge.source_path, edge.target_path))
            .collect())
    }
}

#[tokio::test]
async fn reconciling_a_partially_applied_rename_indexes_a_later_external_edit() -> Result<()> {
    const SOURCE: &str = "notes/Guide.md";
    const MOVED: &str = "archive/Published guide.md";
    const NEIGHBOR: &str = "notes/Neighbor.md";
    const ALTERNATE: &str = "notes/Alternate.md";
    const APPROVED: &str = "reports/Reviewed.md";
    const RACED: &str = "reports/Edited.md";
    const SOURCE_TEXT: &str =
        "# Ready\n\npublicationoriginsignal\n\n[[./Neighbor.md#Scope|Read scope]]\n";
    const APPROVED_TEXT: &str =
        "# Reviewed notes\n\n[[/notes/Guide.md#Ready|Approved reference]]\n";
    const RACED_TEXT: &str = "# Notes waiting for an edit\n\noldeditorsignal\n\n[[/notes/Guide.md#Ready|Review draft]]\n";
    const EXTERNAL_EDIT: &str = "# Edited directly\n\nneweditorsignal\n\n[[/notes/Guide.md#Ready|Old path retained]]\n[[/notes/Alternate.md#Decision|New external decision]]\n";

    let checkout = Checkout::new(&[
        (SOURCE, SOURCE_TEXT),
        (NEIGHBOR, "# Scope\n\nNeighbouring reference\n"),
        (ALTERNATE, "# Decision\n\nExternal alternative\n"),
        (APPROVED, APPROVED_TEXT),
        (RACED, RACED_TEXT),
        ("archive/.gitkeep", ""),
    ])
    .await?;
    checkout.reconcile().await?;
    let source_hits = checkout.search("publicationoriginsignal").await?;
    assert_eq!(source_hits, [SOURCE]);
    assert_eq!(checkout.search("oldeditorsignal").await?, [RACED]);
    let source = checkout.open(&source_hits[0]).await?;
    assert_eq!(source.markdown, SOURCE_TEXT);
    assert_eq!(
        checkout.edges().await?,
        [
            (SOURCE.to_owned(), NEIGHBOR.to_owned()),
            (RACED.to_owned(), SOURCE.to_owned()),
            (APPROVED.to_owned(), SOURCE.to_owned()),
        ]
    );
    let old_backlinks = checkout.relationships(SOURCE).await?.backlinks;
    assert_eq!(old_backlinks.len(), 2);
    let preview = checkout.preview(&source, MOVED).await?;
    assert_eq!(preview.source_revision_id, source.revision.id);
    assert_eq!(
        preview
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [SOURCE, RACED, APPROVED]
    );
    assert!(preview.warnings.is_empty());

    fs::write(checkout.root.join(RACED), EXTERNAL_EDIT)?;
    let report = checkout
        .store
        .apply_markdown_rename(checkout.id.clone(), preview.id)
        .await?;
    assert!(matches!(
        report.outcome,
        MarkdownRenameOutcome::Moved { .. }
    ));
    assert!(report.partial);
    assert!(
        matches!(report.files.as_slice(), [
        MarkdownRenameFileResult::Rewritten { path: moved, count: 1, .. },
        MarkdownRenameFileResult::Changed { path: raced, .. },
        MarkdownRenameFileResult::Rewritten { path: approved, count: 1, .. },
    ] if moved == MOVED && raced == RACED && approved == APPROVED),
        "{report:?}"
    );
    assert!(!checkout.root.join(SOURCE).exists());
    assert_eq!(
        fs::read(checkout.root.join(RACED))?,
        EXTERNAL_EDIT.as_bytes()
    );
    assert_eq!(
        fs::read_to_string(checkout.root.join(APPROVED))?,
        APPROVED_TEXT.replace("/notes/Guide.md", "/archive/Published guide.md"),
    );

    // Re-read the checkout as the runtime does after rename, including notes that
    // changed outside the rename plan. Every indexed view uses the disk revision.
    let paths = checkout.reconcile().await?;
    assert!(paths.iter().any(|path| path == MOVED));
    assert!(paths.iter().all(|path| path != SOURCE));
    assert_eq!(checkout.search("publicationoriginsignal").await?, [MOVED]);
    assert!(checkout.search("oldeditorsignal").await?.is_empty());
    assert_eq!(checkout.search("neweditorsignal").await?, [RACED]);
    assert_eq!(
        checkout.edges().await?,
        [
            (MOVED.to_owned(), NEIGHBOR.to_owned()),
            (RACED.to_owned(), ALTERNATE.to_owned()),
            (APPROVED.to_owned(), MOVED.to_owned()),
        ]
    );
    let raced = checkout.relationships(RACED).await?;
    assert_eq!(raced.outgoing.len(), 2);
    assert_eq!(
        raced.outgoing[0].resolution,
        MarkdownLinkResolution::Missing
    );
    assert!(
        matches!(&raced.outgoing[1].resolution, MarkdownLinkResolution::Resolved { location }
        if location.path == ALTERNATE && location.anchor.as_deref() == Some("Decision"))
    );
    let moved = checkout.relationships(MOVED).await?;
    assert!(
        matches!(&moved.outgoing[0].resolution, MarkdownLinkResolution::Resolved { location }
        if location.path == NEIGHBOR && location.anchor.as_deref() == Some("Scope"))
    );
    assert_eq!(moved.backlinks.len(), 1);
    assert_eq!(moved.backlinks[0].path, APPROVED);
    assert_eq!(
        moved.backlinks[0].label.as_deref(),
        Some("Approved reference")
    );
    assert_eq!(
        checkout.relationships(ALTERNATE).await?.backlinks[0].path,
        RACED
    );
    assert!(checkout.relationships(SOURCE).await?.backlinks.is_empty());
    let old = checkout
        .store
        .repository_markdown(checkout.id.clone(), source.revision.id)
        .await?
        .context("the rename removed the reviewed source revision")?;
    assert_eq!(old.revision.relative_path, SOURCE);
    assert_eq!(old.markdown, SOURCE_TEXT);
    Ok(())
}

#[tokio::test]
async fn keeps_canvas_and_base_ordinary_and_rewrites_only_the_reviewed_plugin_note_target()
-> Result<()> {
    const SOURCE: &str = "notes/Guide.md";
    const MOVED: &str = "archive/Guide archived.md";
    const BOARD: &str = "notes/Board.md";
    const CANVAS: &str = "notes/Layout.canvas";
    const BASE: &str = "notes/Catalog.base";
    const CANVAS_TEXT: &str = r#"{"nodes":[{"id":"fixture","type":"text","text":"canvasprivatesignal [[/notes/Guide.md]]"}],"edges":[]}"#;
    const BASE_TEXT: &str =
        "formulas:\r\n  example: 'baseprivatesignal [[/notes/Guide.md]]'\r\nviews: []\r\n";
    const BOARD_TEXT: &str = concat!(
        "\u{feff}---\r\nkanban-plugin: board # preserve this comment\r\n",
        "unknown-setting: '[[/notes/Guide.md]]'\r\n---\r\n\r\n",
        "## Waiting 📌\r\n\r\n- [/] Preserve the state 🔁 every week 📅 2026-11-30\r\n\r\n",
        "Owner:: Ana\r\n\r\nOpen [[/notes/Guide.md#Ready|Approved guide]].\r\n\r\n",
        "```dataview\r\nTABLE Owner\r\nFROM [[/notes/Guide.md]]\r\n```\r\n\r\n",
        "%% kanban:settings\r\n```\r\n{\"kanban-plugin\":\"board\",\"fixture\":\"[[/notes/Guide.md]]\"}\r\n```\r\n%%\r\n",
    );

    let checkout = Checkout::new(&[
        (SOURCE, "# Ready\n\nMarkdown only\n"),
        (BOARD, BOARD_TEXT),
        (CANVAS, CANVAS_TEXT),
        (BASE, BASE_TEXT),
        ("archive/.gitkeep", ""),
    ])
    .await?;
    let tracked = git(&checkout.root, &["ls-files", "--", CANVAS, BASE])?;
    assert_eq!(tracked.lines().collect::<Vec<_>>(), [BASE, CANVAS]);
    assert_eq!(checkout.reconcile().await?, [BOARD, SOURCE]);
    assert!(checkout.search("canvasprivatesignal").await?.is_empty());
    assert!(checkout.search("baseprivatesignal").await?.is_empty());
    assert_eq!(
        checkout.edges().await?,
        [(BOARD.to_owned(), SOURCE.to_owned())]
    );
    let indexed = checkout.relationships(BOARD).await?;
    assert_eq!(indexed.outgoing.len(), 1);
    assert_eq!(
        indexed.outgoing[0].link.alias.as_deref(),
        Some("Approved guide")
    );

    let source = checkout.open(SOURCE).await?;
    let board = checkout.open(BOARD).await?;
    let preview = checkout.preview(&source, MOVED).await?;
    let [file] = preview.files.as_slice() else {
        bail!("expected only the board in the rewrite preview");
    };
    let [edit] = file.edits.as_slice() else {
        bail!("expected exactly one reviewed link");
    };
    assert_eq!(file.path, BOARD);
    assert_eq!(file.revision_id, board.revision.id);
    assert_eq!(edit.before, "/notes/Guide.md");
    assert_eq!(edit.after, "/archive/Guide archived.md");
    assert_eq!(&board.markdown[edit.byte_start..edit.byte_end], edit.before);
    assert_eq!(
        &board.markdown[edit.byte_end..edit.byte_end + "#Ready|".len()],
        "#Ready|"
    );

    let report = checkout
        .store
        .apply_markdown_rename(checkout.id.clone(), preview.id)
        .await?;
    assert!(matches!(
        report.outcome,
        MarkdownRenameOutcome::Moved { .. }
    ));
    assert!(!report.partial, "{report:?}");
    let expected = BOARD_TEXT.replacen(
        "[[/notes/Guide.md#Ready|Approved guide]]",
        "[[/archive/Guide archived.md#Ready|Approved guide]]",
        1,
    );
    assert_eq!(fs::read(checkout.root.join(BOARD))?, expected.as_bytes());
    for (path, text) in [(CANVAS, CANVAS_TEXT), (BASE, BASE_TEXT)] {
        assert_eq!(fs::read(checkout.root.join(path))?, text.as_bytes());
        let file = checkout
            .store
            .read_repository_attachment(checkout.id.clone(), path.into())
            .await?
            .context("ordinary repository file is unavailable")?;
        assert_eq!(file.relative_path, path);
        assert_eq!(STANDARD.decode(file.content_base64)?, text.as_bytes());
    }
    assert!(git(&checkout.root, &["diff", "--name-only", "--", CANVAS, BASE])?.is_empty());
    assert_eq!(checkout.reconcile().await?, [MOVED, BOARD]);
    assert_eq!(
        checkout.edges().await?,
        [(BOARD.to_owned(), MOVED.to_owned())]
    );
    let outgoing = checkout.relationships(BOARD).await?.outgoing;
    assert!(
        matches!(&outgoing[0].resolution, MarkdownLinkResolution::Resolved { location }
        if location.path == MOVED && location.anchor.as_deref() == Some("Ready"))
    );
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git.exe")
        .current_dir(root)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}
