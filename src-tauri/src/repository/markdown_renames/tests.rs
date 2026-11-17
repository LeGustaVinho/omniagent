use std::process::Command;

use super::*;

#[test]
fn repairs_only_the_reviewed_wikilink_and_retains_plugin_source() -> Result<()> {
    let source = concat!(
        "\u{feff}---\r\nkanban-plugin: board # comment\r\nsettings: '[[Old#Plan|Review]]'\r\n---\r\n\r\n",
        "```dataview\r\nLIST FROM [[Old#Plan|Review]]\r\n```\r\n\r\n",
        "%% kanban:settings\r\n{\"note\": \"[[Old#Plan|Review]]\"}\r\n%%\r\n\r\n",
        "| Check |\r\n| --- |\r\n| [[Old#Plan\\|Review]] |\r\n\r\n- [/] [[Old#Plan|Review]] 📅 2026-11-25\r\n",
    );
    let line = source
        .lines()
        .position(|line| line.starts_with("| [["))
        .unwrap() as u32
        + 1;
    let output = replace_wiki_note_reference(
        source,
        "Old#Plan|Review",
        Some(line),
        "docs/Café.md",
        Some("Handoff"),
    )?;
    assert_eq!(
        output,
        source.replacen(
            "[[Old#Plan\\|Review]]",
            "[[/docs/Café.md#Handoff\\|Review]]",
            1
        )
    );
    assert!(
        replace_wiki_note_reference(source, "Old#Plan|Review", None, "docs/Café.md", None).is_err()
    );
    Ok(())
}

#[test]
fn repairs_only_the_clicked_inline_markdown_destination_and_preserves_the_label_and_title()
-> Result<()> {
    let source = "# 🚀 Launch\r\n\r\n[First](./Old.md#Ready) · [**Second**](<./Old.md#Ready> \"Keep title\")\r\n\r\n`[Protected](./Old.md#Ready)`\r\n";
    let offset = crate::markdown_analysis::analyze_markdown(source, "repair-test".into())
        .nodes
        .into_iter()
        .filter(|node| {
            matches!(
                node.kind,
                crate::markdown_analysis::MarkdownKind::Link { .. }
            )
        })
        .nth(1)
        .expect("the second link is parsed")
        .range
        .from;
    let output = replace_markdown_note_reference(
        source,
        "./Old.md#Ready",
        "notes/Launch.md",
        Some(3),
        Some(offset),
        "docs/Café #2.md",
        Some("Release checklist"),
    )?;
    assert_eq!(
        output,
        source.replace(
            "<./Old.md#Ready>",
            "<../docs/Caf%C3%A9%20%232.md#Release%20checklist>"
        )
    );
    assert!(
        replace_markdown_note_reference(
            source,
            "./Old.md#Ready",
            "notes/Launch.md",
            Some(3),
            None,
            "docs/Café #2.md",
            None
        )
        .is_err()
    );
    assert!(
        replace_markdown_note_reference(
            "[Shared][guide]\n\n[guide]: ./Old.md\n",
            "./Old.md",
            "notes/Launch.md",
            Some(1),
            None,
            "docs/Café #2.md",
            None
        )
        .is_err()
    );
    Ok(())
}

const ORIGINAL: &str = "\u{feff}# Heading\r\n\r\nTarget paragraph ^anchor\r\n\r\n[Sibling](./Sibling.md)\r\n![[./cover.png]]\r\n[[#Heading]]\r\n";
const INCOMING: &str = "---\r\nprivate-plugin-setting: '[[chapters/Old]]'\r\n---\r\n\r\n## 📚 Links\r\n\r\n[[chapters/Old#Heading|Label]]\r\n[Text](<chapters/Old.md#Heading> \"preserve title\")\r\n![[chapters/Old#^anchor]]\r\n\r\n| Reference |\r\n| --- |\r\n| [[chapters/Old\\|Cell label]] |\r\n\r\n`[[chapters/Old]]`\r\n\r\n%% [[chapters/Old]] %%\r\n\r\n```dataview\r\nLIST FROM [[chapters/Old]]\r\n```\r\n";

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    database: PathBuf,
    store: Store,
    checkout: String,
}

impl Fixture {
    async fn create() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("checkout");
        fs::create_dir_all(root.join("chapters"))?;
        fs::create_dir_all(root.join("archive/week"))?;
        fs::create_dir_all(root.join("duplicate"))?;
        ensure!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()?
                .success()
        );
        fs::write(root.join("chapters/Old.md"), ORIGINAL)?;
        fs::write(root.join("chapters/Sibling.md"), "Part of the old folder")?;
        fs::write(root.join("chapters/cover.png"), b"repository asset")?;
        fs::write(
            root.join("duplicate/Old.md"),
            "The duplicate is a different note",
        )?;
        fs::write(root.join("incoming.md"), INCOMING)?;
        fs::write(root.join("race.md"), "[[chapters/Old]]")?;
        fs::write(root.join("ambiguous.md"), "[[Old]]")?;
        let database = directory.path().join("profile.sqlite");
        let store = Store::open(database.clone())?;
        let checkout = crate::repository::register(&store, root.clone())
            .await?
            .checkout_id;
        Ok(Self {
            _directory: directory,
            root,
            database,
            store,
            checkout,
        })
    }

    async fn preview(&self) -> Result<MarkdownRenamePreview> {
        let RepositoryDocumentOpen::Document { document } = self
            .store
            .open_repository_document(self.checkout.clone(), "chapters/Old.md".into())
            .await?
        else {
            bail!("missing fixture source");
        };
        self.store
            .preview_markdown_rename(
                self.checkout.clone(),
                "chapters/Old.md".into(),
                "archive/week/Renamed note.md".into(),
                document.revision.id,
            )
            .await
    }
}

#[tokio::test]
async fn applies_only_reviewed_spans_and_preserves_concurrent_edits_and_historical_snapshots()
-> Result<()> {
    let fixture = Fixture::create().await?;
    let plan = fixture.preview().await?;
    assert_eq!(plan.source, "chapters/Old.md");
    assert_eq!(plan.destination, "archive/week/Renamed note.md");
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert_eq!(
        plan.files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["chapters/Old.md", "incoming.md", "race.md"]
    );
    assert_eq!(
        plan.files[1]
            .edits
            .iter()
            .map(|edit| (edit.before.as_str(), edit.after.as_str()))
            .collect::<Vec<_>>(),
        [
            ("chapters/Old", "/archive/week/Renamed note.md"),
            ("chapters/Old.md", "archive/week/Renamed%20note.md"),
            ("chapters/Old", "/archive/week/Renamed note.md"),
            ("chapters/Old", "/archive/week/Renamed note.md"),
        ]
    );
    assert!(
        fixture
            .store
            .apply_markdown_rename("wrong-checkout".into(), plan.id.clone())
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("incoming.md"))?,
        INCOMING
    );

    fs::write(
        fixture.root.join("race.md"),
        "[[chapters/Old]]\nExternal edit after preview.",
    )?;
    let report = fixture
        .store
        .apply_markdown_rename(fixture.checkout.clone(), plan.id.clone())
        .await?;
    assert!(
        matches!(report.outcome, MarkdownRenameOutcome::Moved { .. }),
        "{report:?}"
    );
    assert!(report.partial);
    assert!(
        matches!(&report.files[2], MarkdownRenameFileResult::Changed { path, .. } if path == "race.md")
    );
    assert!(!fixture.root.join("chapters/Old.md").exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("archive/week/Renamed note.md"))?,
        ORIGINAL
            .replace("./Sibling.md", "../../chapters/Sibling.md")
            .replace("./cover.png", "../../chapters/cover.png")
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("incoming.md"))?,
        INCOMING
            .replace(
                "chapters/Old#Heading|",
                "/archive/week/Renamed note.md#Heading|"
            )
            .replace(
                "<chapters/Old.md#Heading>",
                "<archive/week/Renamed%20note.md#Heading>"
            )
            .replace(
                "chapters/Old#^anchor",
                "/archive/week/Renamed note.md#^anchor"
            )
            .replace(
                "chapters/Old\\|Cell",
                "/archive/week/Renamed note.md\\|Cell"
            )
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("race.md"))?,
        "[[chapters/Old]]\nExternal edit after preview."
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("ambiguous.md"))?,
        "[[Old]]"
    );

    let reopened = Store::open(fixture.database.clone())?;
    let historical = reopened
        .repository_markdown(fixture.checkout.clone(), plan.source_revision_id)
        .await?
        .context("old snapshot disappeared")?;
    assert_eq!(historical.revision.relative_path, "chapters/Old.md");
    assert_eq!(historical.markdown, ORIGINAL);
    assert_eq!(
        reopened
            .apply_markdown_rename(fixture.checkout.clone(), plan.id)
            .await?,
        report
    );
    Ok(())
}

#[tokio::test]
async fn a_changed_source_keeps_the_document_and_its_links_in_place() -> Result<()> {
    let fixture = Fixture::create().await?;
    let plan = fixture.preview().await?;
    fs::write(fixture.root.join("chapters/Old.md"), "Changed externally")?;
    let result = fixture
        .store
        .apply_markdown_rename(fixture.checkout.clone(), plan.id)
        .await?;
    assert!(
        matches!(result.outcome, MarkdownRenameOutcome::Conflict { .. }),
        "{result:?}"
    );
    assert!(result.files.is_empty());
    assert!(!fixture.root.join("archive/week/Renamed note.md").exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("chapters/Old.md"))?,
        "Changed externally"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("incoming.md"))?,
        INCOMING
    );
    Ok(())
}

#[tokio::test]
async fn rejects_preview_for_a_destination_redirected_through_a_junction() -> Result<()> {
    let fixture = Fixture::create().await?;
    let outside = fixture._directory.path().join("unregistered");
    fs::create_dir(&outside)?;
    let result = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(fixture.root.join("redirected"))
        .arg(&outside)
        .output()?;
    ensure!(
        result.status.success(),
        "could not create the fixture junction: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let RepositoryDocumentOpen::Document { document } = fixture
        .store
        .open_repository_document(fixture.checkout.clone(), "chapters/Old.md".into())
        .await?
    else {
        bail!("missing fixture source");
    };
    let error = fixture
        .store
        .preview_markdown_rename(
            fixture.checkout.clone(),
            "chapters/Old.md".into(),
            "redirected/Renamed.md".into(),
            document.revision.id,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("outside its checkout"),
        "{error:#}"
    );
    assert!(!outside.join("Renamed.md").exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("chapters/Old.md"))?,
        ORIGINAL
    );
    Ok(())
}
