use std::{fs, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    markdown_analysis::{MarkdownKind, analyze_markdown},
    markdown_properties::replace_markdown_property,
    repository::{
        self,
        documents::{RepositoryDocument, RepositoryDocumentOpen, RepositoryDocumentSave},
    },
    store::Store,
};
use sha2::{Digest, Sha256};

const DATAVIEW: &str = concat!(
    "\u{feff}---\r\nowner: Draft # preserve this comment\r\n---\r\n\r\n",
    "# Research log\r\n\r\nReviewer:: João\n\nCheck-in (status:: pending).\r\n\r\n",
    "```dataview\r\nTABLE file.link, Reviewer\r\nFROM #research/release\r\n",
    "WHERE status != \"complete\"\r\nSORT file.mtime DESC\r\n```\r\n",
);
const TASKS: &str = concat!(
    "# Work items\r\n\r\n- [ ] Check release notes\r\n",
    "- [/] Localize captions 👤 Ana ⏫ 🔁 every week 🛫 2026-11-25 📅 2026-11-27 #release/docs\r\n",
    "- [x] Publish draft ✅ 2026-11-10 🆔 subtitles ⛔ copy-review",
);
const KANBAN: &str = concat!(
    "\u{feff}---\r\nkanban-plugin: board # preserve board type\r\n",
    "unknown-key: {columns: ['Later', 'Doing']}\r\n---\r\n\r\n",
    "## Later\r\n\r\n- [ ] Draft brief\r\n\r\n## Doing\n\n- [ ] Pair on release\n\n",
    "%% kanban:settings\n```\n{\"kanban-plugin\":\"board\",\"list-collapse\":[false,true]}\n```\n%%",
);

#[tokio::test]
async fn repository_roundtrip_preserves_dataview_tasks_and_kanban_source() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let checkout_root = temp.path().join("repo");
    fs::create_dir(&checkout_root)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout_root)
            .status()?
            .success(),
        "Git repository initialization failed"
    );
    let database = temp.path().join("profile.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = repository::register(&store, checkout_root.clone())
        .await?
        .checkout_id;
    let notes = [
        ("research.md", DATAVIEW),
        ("tasks.md", TASKS),
        ("board.md", KANBAN),
    ];

    for (path, original) in notes {
        fs::write(checkout_root.join(path), original.as_bytes())?;
    }

    let mut saved = Vec::new();
    for (path, original) in notes {
        let opened = open_document(&store, &checkout, path).await?;
        assert_eq!(
            opened.revision.sha256,
            format!("{:x}", Sha256::digest(original.as_bytes()))
        );
        let analysis = analyze_markdown(&opened.markdown, opened.revision.id.clone());
        assert_eq!(
            fs::read(checkout_root.join(path))?,
            original.as_bytes(),
            "opening and analyzing {path}"
        );

        let (edited, expected) = match path {
            "research.md" => {
                assert!(analysis.nodes.iter().any(|node| matches!(&node.kind,
                    MarkdownKind::CodeBlock { language, text } if language == "dataview" && text.contains("FROM #research/release")
                )));
                (
                    replace_markdown_property(&opened.markdown, "owner", "Reviewed")?,
                    original.replacen("owner: Draft", "owner: Reviewed", 1),
                )
            }
            "tasks.md" => {
                assert!(
                    analysis.nodes.iter().any(|node| matches!(
                        node.kind,
                        MarkdownKind::TaskMarker { status: '/', .. }
                    ))
                );
                let value = analysis
                    .nodes
                    .iter()
                    .find_map(|node| match node.kind {
                        MarkdownKind::TaskMarker {
                            status: ' ',
                            value_range,
                            ..
                        } => Some(value_range),
                        _ => None,
                    })
                    .context("The neighboring checkbox was not detected")?;
                let mut edited = opened.markdown.clone();
                edited.replace_range(value.byte_start..value.byte_end, "x");
                (
                    edited,
                    original.replacen("[ ] Check release notes", "[x] Check release notes", 1),
                )
            }
            "board.md" => {
                assert!(
                    analysis
                        .nodes
                        .iter()
                        .any(|node| matches!(node.kind, MarkdownKind::Frontmatter { .. }))
                );
                assert!(
                    analysis
                        .nodes
                        .iter()
                        .any(|node| matches!(node.kind, MarkdownKind::Comment))
                );
                (
                    opened.markdown.replacen("Draft brief", "Publish brief", 1),
                    original.replacen("Draft brief", "Publish brief", 1),
                )
            }
            _ => unreachable!(),
        };
        let RepositoryDocumentSave::Saved { document } = store
            .save_repository_document(
                checkout.clone(),
                PathBuf::from(path),
                opened.revision.id.clone(),
                edited,
            )
            .await?
        else {
            bail!("Could not save the updated {path}")
        };
        assert_ne!(document.revision.id, opened.revision.id);
        assert_eq!(
            fs::read(checkout_root.join(path))?,
            expected.as_bytes(),
            "saving {path}"
        );
        saved.push((path, document, expected));
    }

    drop(store);
    let reopened = Store::open(database)?;
    for (path, document, expected) in saved {
        assert_eq!(open_document(&reopened, &checkout, path).await?, document);
        assert_eq!(
            fs::read(checkout_root.join(path))?,
            expected.as_bytes(),
            "reopening {path}"
        );
    }
    Ok(())
}

async fn open_document(store: &Store, checkout: &str, path: &str) -> Result<RepositoryDocument> {
    match store
        .open_repository_document(checkout.into(), path.into())
        .await?
    {
        RepositoryDocumentOpen::Document { document } => Ok(document),
        other => bail!("Expected a repository document for {path}, got {other:?}"),
    }
}
