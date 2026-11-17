use std::{fs, path::Path};

use anyhow::{Context, Result};

use crate::repository::{self, git_path, git_query};

use super::*;

const SOURCE: &str = "notes/Handbook.md";
const DESTINATION: &str = "guides/Combined.md";
const REFERENCE: &str = "Home.md";
const SOURCE_TEXT: &str =
    "---\r\naliases: [Handbook alias]\r\n---\r\n\r\n# Procedures\r\n\r\n![Logo](logo.png)\r\n";
const DESTINATION_TEXT: &str = "---\nstatus: open\n---\n\n# Guide\n";
const REFERENCE_TEXT: &str = "[Original label](notes/Handbook.md#Procedures \"Keep title\")\n\n[[Handbook alias#Procedures|Keep alias]]\n\n`[[Handbook]]`\n";

async fn fixture() -> Result<(tempfile::TempDir, Store, repository::ProjectCheckout)> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("notes"))?;
    fs::create_dir_all(root.join("guides"))?;
    let git = git_path()?;
    git_query(&git, &root, &["init", "--quiet"])?;
    fs::write(root.join(SOURCE), SOURCE_TEXT)?;
    fs::write(root.join(DESTINATION), DESTINATION_TEXT)?;
    fs::write(root.join(REFERENCE), REFERENCE_TEXT)?;
    fs::write(root.join("notes/logo.png"), b"fixture")?;
    let store = Store::open(temp.path().join("app.sqlite"))?;
    let checkout = repository::register(&store, root).await?;
    Ok((temp, store, checkout))
}

async fn plan(store: &Store, checkout: &str) -> Result<MarkdownMergePlan> {
    store
        .preview_markdown_merge(checkout.into(), SOURCE.into(), DESTINATION.into())
        .await
}

#[tokio::test]
async fn previews_and_stages_moved_assets_retargeted_links_and_a_source_redirect() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let id = checkout.checkout_id;
    let root = PathBuf::from(checkout.checkout_root);
    let plan = plan(&store, &id).await?;
    assert_eq!(plan.preview.source, SOURCE);
    assert_eq!(plan.preview.destination, DESTINATION);
    assert!(
        plan.preview.warnings.is_empty(),
        "{:?}",
        plan.preview.warnings
    );
    assert_eq!(plan.preview.references.len(), 2);
    assert_eq!(
        plan.preview.merged_markdown,
        "---\nstatus: open\n---\n\n# Guide\n\n# Procedures\n\n![Logo](../notes/logo.png)\n"
    );
    assert_eq!(
        plan.preview.source_redirect,
        "---\naliases: [Handbook alias]\n---\n\nMerged into [Combined](../guides/Combined.md).\n"
    );
    let drafts = store.stage_markdown_merge(id.clone(), plan).await?;
    assert_eq!(
        drafts
            .iter()
            .map(|draft| draft.base.relative_path.as_str())
            .collect::<Vec<_>>(),
        [DESTINATION, REFERENCE, SOURCE]
    );
    assert_eq!(
        drafts[1].markdown,
        "[Original label](guides/Combined.md#Procedures \"Keep title\")\n\n[[/guides/Combined.md#Procedures|Keep alias]]\n\n`[[Handbook]]`\n"
    );
    assert_eq!(fs::read_to_string(root.join(REFERENCE))?, REFERENCE_TEXT);
    assert_eq!(fs::read_to_string(root.join(SOURCE))?, SOURCE_TEXT);
    assert_eq!(
        fs::read_to_string(root.join(DESTINATION))?,
        DESTINATION_TEXT
    );
    for draft in drafts {
        assert_eq!(
            store
                .repository_markdown_draft(id.clone(), draft.base.relative_path.clone())
                .await?,
            Some(draft)
        );
    }
    Ok(())
}

#[tokio::test]
async fn detects_a_changed_reference_and_preserves_concurrent_drafts() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let id = checkout.checkout_id;
    let root = PathBuf::from(checkout.checkout_root);
    let first_plan = plan(&store, &id).await?;
    fs::write(
        root.join(REFERENCE),
        REFERENCE_TEXT.to_owned() + "\nChanged outside the app.\n",
    )?;
    let error = store
        .stage_markdown_merge(id.clone(), first_plan)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("changed after the preview"));
    assert!(
        store
            .repository_markdown_draft_paths(id.clone())
            .await?
            .is_empty()
    );

    let next_plan = plan(&store, &id).await?;
    let revision = next_plan
        .edits
        .iter()
        .find(|edit| edit.revision.relative_path == REFERENCE)
        .context("Missing reference edit")?
        .revision
        .id
        .clone();
    let draft = store
        .save_repository_markdown_draft(
            id.clone(),
            revision,
            None,
            "An independent draft.\n".into(),
        )
        .await?;
    let error = store
        .stage_markdown_merge(id.clone(), next_plan)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("has a draft"));
    assert_eq!(
        store.repository_markdown_draft_paths(id.clone()).await?,
        [REFERENCE]
    );
    assert_eq!(
        store
            .repository_markdown_draft(id, REFERENCE.into())
            .await?,
        Some(draft)
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_anchor_blocks_merge_before_any_drafts_are_written() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    fs::write(
        Path::new(&checkout.checkout_root).join(DESTINATION),
        "# Procedures\n",
    )?;
    let error = plan(&store, &checkout.checkout_id)
        .await
        .err()
        .context("Duplicate anchors allowed a merge")?;
    assert!(format!("{error:#}").contains("Both notes have the anchor Procedures"));
    assert!(
        store
            .repository_markdown_draft_paths(checkout.checkout_id)
            .await?
            .is_empty()
    );
    Ok(())
}
