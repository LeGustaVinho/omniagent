use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    markdown_analysis::{MarkdownKind, MarkdownRange, analyze_markdown},
    markdown_properties::replace_markdown_property,
    repository::{
        self,
        documents::{LineEndings, RepositoryDocumentOpen, RepositoryDocumentSave},
    },
    store::Store,
};
use sha2::{Digest, Sha256};

const NOTE_PATH: &str = "notes/Guide.md";
const WIKILINK: &str = "[[docs/Guia#Seção|Leia à tarde]]";
const ORIGINAL: &str = concat!(
    "\u{feff}---\r\n",
    "# Ownership for the next handoff\r\n",
    "owner: \"Ana\"  # manter comentário 🔎\r\n",
    "tags: ['release', 'café']\r\n",
    "summary: |-\r\n  Revisão sem alterações.  \r\n",
    "---\r\n\r\n",
    "# Visão 🌐\r\n\r\n",
    "Café 😀 [[docs/Guia#Seção|Leia à tarde]] e **revisão**.\n\n",
    "- [ ] Publicar anotação\r\n",
    "Última linha sem quebra.",
);

#[tokio::test]
async fn reading_leaves_the_checkout_intact_and_saving_changes_one_yaml_value() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("checkout");
    fs::create_dir_all(root.join("notes"))?;
    let root = dunce::canonicalize(root)?;
    let path = root.join(NOTE_PATH);
    git(&root, &["init", "--quiet"])?;
    fs::write(root.join(".gitattributes"), "*.md -text\n")?;
    fs::write(&path, ORIGINAL)?;
    git(&root, &["add", "."])?;
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Publish property fixture",
        ],
    )?;

    let database = fixture.path().join("profile.sqlite");
    let store = Store::open(database)?;
    let checkout = repository::register(&store, root).await?.checkout_id;
    let before = fs::read(&path)?;
    let before_hash = digest(&before);
    let RepositoryDocumentOpen::Document { document: opened } = store
        .open_repository_document(checkout.clone(), path.clone())
        .await?
    else {
        bail!("the fixture note could not be opened")
    };
    assert_eq!(opened.canonical_path, path);
    assert_eq!(opened.revision.relative_path, NOTE_PATH);
    assert_eq!(opened.revision.sha256, before_hash);
    assert!(opened.utf8_bom);
    assert_eq!(opened.line_endings, LineEndings::Mixed);

    let analysis = analyze_markdown(&opened.markdown, opened.revision.id.clone());
    assert_eq!(analysis.buffer_revision, opened.revision.id);
    assert!(
        analysis
            .nodes
            .iter()
            .any(|node| { matches!(node.kind, MarkdownKind::Frontmatter { .. }) })
    );
    assert_wikilink(&opened.markdown, &analysis.buffer_revision)?;
    let archived = store
        .repository_markdown(checkout.clone(), opened.revision.id.clone())
        .await?
        .context("the retained source revision could not be read")?;
    assert_eq!(archived.markdown.as_bytes(), before);
    assert_eq!(fs::read(&path)?, before);
    assert_eq!(digest(&fs::read(&path)?), before_hash);

    let edited = replace_markdown_property(&opened.markdown, "owner", "\"Lúcia 🚀\"")?;
    assert_eq!(
        edited,
        opened.markdown.replacen("\"Ana\"", "\"Lúcia 🚀\"", 1),
        "the property editor changed text beyond the selected value"
    );
    let RepositoryDocumentSave::Saved { document: saved } = store
        .save_repository_document(
            checkout.clone(),
            NOTE_PATH.into(),
            opened.revision.id.clone(),
            edited.clone(),
        )
        .await?
    else {
        bail!("the coordinated property save could not be verified")
    };
    let expected = ORIGINAL.replacen("\"Ana\"", "\"Lúcia 🚀\"", 1);
    let final_bytes = fs::read(&path)?;
    assert_eq!(
        final_bytes,
        expected.as_bytes(),
        "bytes outside the value changed"
    );
    assert_eq!(saved.revision.sha256, digest(&final_bytes));
    assert_ne!(saved.revision.sha256, before_hash);
    assert!(saved.utf8_bom);
    assert_eq!(saved.line_endings, LineEndings::Mixed);
    assert_eq!(saved.markdown, edited);
    assert_eq!(
        store
            .repository_markdown(checkout, opened.revision.id)
            .await?
            .context("the previous source revision was lost")?
            .markdown,
        ORIGINAL
    );

    // Independently produced responses can arrive out of order while the buffer changes.
    // Every analysis must still describe the exact text and editor revision it received.
    let second = edited.replacen("Café 😀 ", "Café 😀 ação 💡 ", 1);
    let third = second.replacen("ação 💡 ", "três alterações é🚧\n\n", 1);
    let latest_range = assert_wikilink(&third, "guide-buffer-3")?;
    let earlier_range = assert_wikilink(&second, "guide-buffer-2")?;
    let original_range = assert_wikilink(&edited, "guide-buffer-1")?;
    assert!(latest_range.from > earlier_range.from);
    assert!(earlier_range.from > original_range.from);
    assert_ne!(
        latest_range.byte_start - original_range.byte_start,
        latest_range.from - original_range.from,
        "the fixture must distinguish UTF-8 bytes from UTF-16 editor offsets"
    );
    assert_eq!(
        fs::read(path)?,
        final_bytes,
        "unsaved analyses touched the disk"
    );
    Ok(())
}

fn assert_wikilink(text: &str, revision: &str) -> Result<MarkdownRange> {
    let analysis = analyze_markdown(text, revision.into());
    assert_eq!(analysis.buffer_revision, revision);
    let node = analysis
        .nodes
        .iter()
        .find(|node| {
            matches!(&node.kind, MarkdownKind::Wikilink { target, alias, anchor }
            if target == "docs/Guia" && alias.as_deref() == Some("Leia à tarde")
                && anchor.as_deref() == Some("Seção"))
        })
        .context("the analysis is missing the wiki link")?;
    let expected_start = text
        .find(WIKILINK)
        .context("the fixture wiki link is missing")?;
    let expected_end = expected_start + WIKILINK.len();
    let utf16_start = text[..expected_start].encode_utf16().count();
    assert_eq!(node.range.byte_start, expected_start);
    assert_eq!(node.range.byte_end, expected_end);
    assert_eq!(node.range.from, utf16_start);
    assert_eq!(node.range.to, utf16_start + WIKILINK.encode_utf16().count());
    assert_eq!(&text[node.range.byte_start..node.range.byte_end], WIKILINK);
    Ok(node.range)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git.exe")
        .args(args)
        .current_dir(root)
        .output()?;
    ensure!(
        output.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
