use std::{fs, path::Path};

use anyhow::Result;

use crate::repository::{self, git_path, git_query};

use super::*;

const ONE: &str = "Notes.md";
const TWO: &str = "Checklist.md";
const NOTES: &str = r##"---
tags: [release, '#Release/QA', other] # release comment
description: "#release stays prose"
---

Text release is prose. #release #Release/Follow-up #releases
[Link](https://example.test/#release) and [#release](notes.md)
`#release`, \#release, mail#release
%% hidden #release %%
```markdown
#release
```
"##;

const CHECKLIST: &str = r##"---
tag:
  - RELEASE # keep this
  - "#releases"
---

# Checklist

Check #Release.
"##;

fn rewrite(
    markdown: &str,
    source: &str,
    destination: &str,
    nested: bool,
) -> Result<(String, Vec<TagOccurrence>, Vec<MarkdownRenameWarning>)> {
    let change = TagChange::new(source, destination, nested)?;
    let mut warnings = Vec::new();
    let found = occurrences(markdown, ONE, &change, &mut warnings);
    let edits: Vec<_> = found.iter().map(|item| item.edit.clone()).collect();
    let rewritten = replace_ranges(markdown.into(), &edits).context("Failed exact replacement")?;
    Ok((rewritten, found, warnings))
}

#[test]
fn renames_tag_spans_without_mutating_prose_code_or_yaml_formatting() -> Result<()> {
    let (after, found, warnings) = rewrite(NOTES, "#release", "Delivery/Release", true)?;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(found.len(), 4);
    assert_eq!(found.iter().filter(|hit| hit.property).count(), 2);
    assert_eq!(found.iter().filter(|hit| hit.edit.line == 6).count(), 2);
    assert_eq!(
        after,
        NOTES
            .replace(
                "tags: [release, '#Release/QA'",
                "tags: [Delivery/Release, '#Delivery/Release/QA'"
            )
            .replace(
                "#release #Release/Follow-up",
                "#Delivery/Release #Delivery/Release/Follow-up"
            )
    );
    let (after, found, _) = rewrite(CHECKLIST, "ReLeAsE", "true", false)?;
    assert_eq!(found.len(), 2);
    assert_eq!(
        after,
        CHECKLIST
            .replace("- RELEASE", "- 'true'")
            .replace("#Release.", "#true.")
    );
    Ok(())
}

#[test]
fn leaves_unsupported_property_spelling_for_review_and_matches_only_complete_segments() -> Result<()>
{
    let source = "---\ntags: [\"r\\u0065lease\", '  #release  ', release/sub, releases]\n---\n\n#release/part #release.\n";
    let (after, found, warnings) = rewrite(source, "release", "ship", false)?;
    assert_eq!(found.len(), 2);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].line, Some(2));
    assert_eq!(
        after,
        source
            .replace("'  #release  '", "'  #ship  '")
            .replace("#release.", "#ship.")
    );
    assert!(TagChange::new("release", "only words", false).is_err());
    assert!(TagChange::new("release", "2025/12", false).is_err());
    assert!(TagChange::new("release", "ship//broken", true).is_err());
    Ok(())
}

async fn fixture() -> Result<(tempfile::TempDir, Store, repository::ProjectCheckout)> {
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("repo");
    fs::create_dir(&root)?;
    git_query(&git_path()?, &root, &["init", "--quiet"])?;
    fs::write(root.join(ONE), NOTES.replace('\n', "\r\n"))?;
    fs::write(root.join(TWO), CHECKLIST)?;
    let store = Store::open(fixture.path().join("app.sqlite"))?;
    let checkout = repository::register(&store, root).await?;
    Ok((fixture, store, checkout))
}

async fn plan(store: &Store, id: &str) -> Result<MarkdownTagRenamePlan> {
    store
        .preview_markdown_tag_rename(id.into(), "release".into(), "delivery".into(), true)
        .await
}

#[tokio::test]
async fn stages_all_matching_notes_and_rejects_a_different_checkout_or_changed_revision()
-> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let id = checkout.checkout_id;
    let root = Path::new(&checkout.checkout_root);
    let error = store
        .stage_markdown_tag_rename("different-checkout".into(), plan(&store, &id).await?)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("different checkout"));
    let preview = plan(&store, &id).await?;
    assert_eq!(preview.preview.files.len(), 2);
    fs::write(
        root.join(TWO),
        CHECKLIST.to_owned() + "\nExternal change \n",
    )?;
    let error = store
        .stage_markdown_tag_rename(id.clone(), preview)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("changed after the preview"));
    assert!(
        store
            .repository_markdown_draft_paths(id.clone())
            .await?
            .is_empty()
    );

    let preview = plan(&store, &id).await?;
    let drafts = store.stage_markdown_tag_rename(id.clone(), preview).await?;
    assert_eq!(drafts.len(), 2);
    assert_eq!(
        fs::read_to_string(root.join(ONE))?,
        NOTES.replace('\n', "\r\n")
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
async fn blocks_a_draft_only_occurrence_without_changing_other_notes() -> Result<()> {
    let (_temp, store, checkout) = fixture().await?;
    let id = checkout.checkout_id;
    let root = Path::new(&checkout.checkout_root);
    fs::write(root.join(TWO), "# Clean saved note\n")?;
    let RepositoryDocumentOpen::Document { document } = store
        .open_repository_document(id.clone(), TWO.into())
        .await?
    else {
        anyhow::bail!("Missing fixture note");
    };
    let draft = store
        .save_repository_markdown_draft(
            id.clone(),
            document.revision.id,
            None,
            "# Unsaved\n\n#release\n".into(),
        )
        .await?;
    let preview = plan(&store, &id).await?;
    assert_eq!(preview.preview.blocked, [TWO]);
    assert_eq!(preview.preview.files.len(), 1);
    assert!(
        store
            .stage_markdown_tag_rename(id.clone(), preview)
            .await
            .is_err()
    );
    assert_eq!(
        store.repository_markdown_draft_paths(id.clone()).await?,
        [TWO]
    );
    assert_eq!(
        store.repository_markdown_draft(id, TWO.into()).await?,
        Some(draft)
    );
    Ok(())
}
