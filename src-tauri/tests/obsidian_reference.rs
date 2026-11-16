use std::{fs, process::Command};

use anyhow::{Result, bail, ensure};
use omniagent::{
    markdown_analysis::{MarkdownKind, analyze_markdown},
    repository::{
        self,
        documents::{RepositoryDocumentOpen, RepositoryDocumentSave},
    },
    store::Store,
};

const SOURCE: &str = include_str!("../../tests/fixtures/obsidian-reference/reference.md");
const TARGET: &str = include_str!("../../tests/fixtures/obsidian-reference/details.md");

#[tokio::test]
async fn analyzes_and_roundtrips_an_obsidian_reference_fixture_in_a_git_checkout() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("reference");
    fs::create_dir_all(root.join("notes"))?;
    ensure!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    fs::write(root.join("notes/reference.md"), SOURCE)?;
    fs::write(root.join("notes/details.md"), TARGET)?;

    let store = Store::open(directory.path().join("profile.sqlite"))?;
    let checkout = repository::register(&store, root.clone())
        .await?
        .checkout_id;
    let RepositoryDocumentOpen::Document { document } = store
        .open_repository_document(checkout.clone(), "notes/reference.md".into())
        .await?
    else {
        bail!("reference fixture did not open");
    };
    assert_eq!(document.markdown, SOURCE.replace("\r\n", "\n"));
    assert_eq!(
        fs::read(root.join("notes/reference.md"))?,
        SOURCE.as_bytes()
    );
    let analysis = analyze_markdown(&document.markdown, document.revision.id.clone());
    let view_text: Vec<_> = document.markdown.encode_utf16().collect();
    let spans = analysis
        .nodes
        .iter()
        .map(|node| {
            (
                String::from_utf16(&view_text[node.range.from..node.range.to]).unwrap(),
                &node.kind,
            )
        })
        .collect::<Vec<_>>();
    assert!(spans.iter().any(|(span, kind)| span == "[[./details#Evidence|open the reference]]"
        && matches!(kind, MarkdownKind::Wikilink { alias: Some(alias), .. } if alias == "open the reference")));
    assert!(
        spans
            .iter()
            .any(|(span, kind)| span == "**the fixture**" && matches!(kind, MarkdownKind::Strong))
    );
    assert!(spans.iter().any(|(span, kind)| span == "[ ]"
        && matches!(kind, MarkdownKind::TaskMarker { status: ' ', .. })));
    assert!(spans.iter().any(|(span, kind)| span == "[/]"
        && matches!(kind, MarkdownKind::TaskMarker { status: '/', .. })));
    assert!(
        spans
            .iter()
            .any(|(_, kind)| matches!(kind, MarkdownKind::Table))
    );
    assert!(spans.iter().any(
        |(_, kind)| matches!(kind, MarkdownKind::Callout { title, .. } if title == "Calibration")
    ));
    for display in [false, true] {
        assert!(spans.iter().any(
            |(_, kind)| matches!(kind, MarkdownKind::Math { display: form, .. } if *form == display)
        ));
    }
    assert!(spans.iter().any(
        |(_, kind)| matches!(kind, MarkdownKind::CodeBlock { language, text }
        if language == "mermaid" && text.contains("A[Draft] --> B[Review]"))
    ));
    assert!(spans.iter().any(|(span, kind)| span == "![[./details#Evidence]]"
        && matches!(kind, MarkdownKind::Embed { target, anchor: Some(anchor), .. } if target == "./details" && anchor == "Evidence")));
    for definition in [false, true] {
        assert!(spans.iter().any(
            |(_, kind)| matches!(kind, MarkdownKind::Footnote { name, definition: form }
            if name == "tolerance" && *form == definition)
        ));
    }

    let modified = document
        .markdown
        .replacen("| Start | Pending |", "| Start | Ready |", 1)
        .replacen("[ ] Review the table", "[x] Review the table", 1);
    let RepositoryDocumentSave::Saved { document: saved } = store
        .save_repository_document(
            checkout,
            "notes/reference.md".into(),
            document.revision.id,
            modified.clone(),
        )
        .await?
    else {
        bail!("reference fixture edit was not saved");
    };
    assert_eq!(saved.markdown, modified);
    assert_eq!(
        fs::read_to_string(root.join("notes/reference.md"))?,
        SOURCE
            .replacen("| Start | Pending |", "| Start | Ready |", 1)
            .replacen("[ ] Review the table", "[x] Review the table", 1)
    );
    assert_eq!(fs::read(root.join("notes/details.md"))?, TARGET.as_bytes());
    Ok(())
}
