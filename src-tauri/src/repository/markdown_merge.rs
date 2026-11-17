use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::TransactionBehavior;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    markdown_analysis::{MarkdownKind, analyze_markdown},
    markdown_recovery::{RepositoryMarkdownDraft, load_payload, stored_revision, write_payload},
    providers::configuration::open_locked_local_file,
    store::Store,
};

use super::{
    RepositoryMarkdownRevision,
    checkout_items::checkout_regular_file,
    checkout_root,
    documents::{
        DocumentKind, RepositoryDocument, RepositoryDocumentOpen, file_identity, lock_directories,
    },
    markdown_catalog::note_paths,
    markdown_index::source_link_note,
    markdown_links::anchor_key,
    markdown_renames::{
        MarkdownRenameFile, MarkdownRenameWarning, markdown_link, planned_merge_references,
        replace_ranges,
    },
    verify_checkout,
};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownMergePreview {
    pub id: String,
    source: String,
    destination: String,
    merged_markdown: String,
    source_redirect: String,
    files: Vec<MergeRevision>,
    references: Vec<MarkdownRenameFile>,
    warnings: Vec<MarkdownRenameWarning>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MergeRevision {
    path: String,
    revision_id: String,
}

pub struct MarkdownMergePlan {
    pub preview: MarkdownMergePreview,
    checkout: String,
    root: PathBuf,
    edits: Vec<MergeEdit>,
}

struct MergeEdit {
    revision: RepositoryMarkdownRevision,
    file_identity: String,
    markdown: String,
}

impl Store {
    pub async fn preview_markdown_merge(
        &self,
        checkout: String,
        source: String,
        destination: String,
    ) -> Result<MarkdownMergePlan> {
        ensure!(
            !source.eq_ignore_ascii_case(&destination),
            "Choose two different notes."
        );
        let root = checkout_root(self, checkout.clone()).await?;
        let root = verify_checkout(self, checkout.clone(), root).await?.root;
        let paths = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || note_paths(&root)).await??
        };
        ensure!(
            paths.contains(&source) && paths.contains(&destination),
            "Choose two Markdown notes from this checkout."
        );

        let mut documents = Vec::new();
        let mut warnings = Vec::new();
        for path in paths {
            match self
                .open_repository_document(checkout.clone(), path.clone().into())
                .await
            {
                Ok(RepositoryDocumentOpen::Document { document })
                    if document.revision.relative_path == path =>
                {
                    documents.push(document);
                }
                _ if path == source || path == destination => {
                    bail!("Could not safely open {path} for merge.");
                }
                _ => warnings.push(MarkdownRenameWarning {
                    path,
                    line: None,
                    message: "This note could not be checked for references.".into(),
                }),
            }
        }
        let drafts = self
            .repository_markdown_draft_paths(checkout.clone())
            .await?;
        tokio::task::spawn_blocking(move || {
            build_plan(
                checkout,
                root,
                source,
                destination,
                documents,
                warnings,
                &drafts,
            )
        })
        .await?
    }

    pub async fn stage_markdown_merge(
        &self,
        checkout: String,
        plan: MarkdownMergePlan,
    ) -> Result<Vec<RepositoryMarkdownDraft>> {
        ensure!(
            checkout == plan.checkout,
            "The merge preview belongs to a different checkout."
        );
        let root = checkout_root(self, checkout.clone()).await?;
        let root = verify_checkout(self, checkout.clone(), root).await?.root;
        ensure!(
            root == plan.root,
            "The checkout moved after the merge preview."
        );
        let max = self
            .checkout_attachment_limits(checkout.clone())
            .await?
            .bytes_per_file;
        let (plan, pinned) = tokio::task::spawn_blocking(move || {
            let pinned = pin_revisions(&root, &plan.edits, max)?;
            Ok::<_, anyhow::Error>((plan, pinned))
        })
        .await??;
        self.execute(move |db| {
            let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for edit in &plan.edits {
                let path = &edit.revision.relative_path;
                ensure!(
                    stored_revision(
                        &transaction,
                        &checkout,
                        &edit.revision.id,
                        DocumentKind::Markdown
                    )? == edit.revision,
                    "{path} changed after the preview. Review a new merge."
                );
                ensure!(
                    load_payload(&transaction, &checkout, path)?.is_none(),
                    "{path} has a draft. Save or discard it and review a new merge."
                );
            }
            let mut drafts = Vec::with_capacity(plan.edits.len());
            for edit in plan.edits {
                let payload = write_payload(&transaction, &edit.revision, edit.markdown)?;
                drafts.push(RepositoryMarkdownDraft {
                    base: edit.revision,
                    revision: payload.revision,
                    markdown: payload.markdown,
                });
            }
            transaction.commit()?;
            drop(pinned);
            Ok(drafts)
        })
        .await
    }
}

fn build_plan(
    checkout: String,
    root: PathBuf,
    source: String,
    destination: String,
    documents: Vec<RepositoryDocument>,
    mut warnings: Vec<MarkdownRenameWarning>,
    drafts: &[String],
) -> Result<MarkdownMergePlan> {
    let document_by_path = documents
        .iter()
        .map(|doc| (doc.revision.relative_path.as_str(), doc))
        .collect::<BTreeMap<_, _>>();
    let source_document = document_by_path
        .get(source.as_str())
        .context("Source note not found.")?;
    let destination_document = document_by_path
        .get(destination.as_str())
        .context("Destination note not found.")?;
    ensure_distinct_anchors(source_document, destination_document)?;
    ensure_distinct_footnotes(source_document, destination_document)?;
    let frontmatter_end = frontmatter_end(&source_document.markdown);
    let source_body = &source_document.markdown[frontmatter_end..];
    ensure!(
        !source_body.trim().is_empty(),
        "The source note has no Markdown body to merge."
    );
    let preview = append_section(&destination_document.markdown, source_body);
    let redirect = append_section(
        &source_document.markdown[..frontmatter_end],
        &format!("Merged into {}.\n", markdown_link(&source, &destination)),
    );
    ensure!(
        source_document.markdown != redirect,
        "This note already redirects to the destination."
    );
    let references = planned_merge_references(
        &root,
        &source,
        &destination,
        &documents,
        &mut warnings,
        &preview,
        &redirect,
    );
    let rewrite = |document: &RepositoryDocument| -> Result<String> {
        let path = &document.revision.relative_path;
        match references.iter().find(|file| file.path == *path) {
            Some(file) => replace_ranges(document.markdown.clone(), &file.edits)
                .with_context(|| format!("The reviewed references changed in {path}.")),
            None => Ok(document.markdown.clone()),
        }
    };
    let source_text = rewrite(source_document)?;
    let destination_text = rewrite(destination_document)?;
    let merged = append_section(&destination_text, &source_text[frontmatter_end..]);

    let make_edit = |doc: &RepositoryDocument, markdown| MergeEdit {
        revision: doc.revision.clone(),
        file_identity: doc.file_identity.clone(),
        markdown,
    };
    // Save the destination first and only publish the source redirect after checked references.
    let mut edits = vec![make_edit(destination_document, merged.clone())];
    for file in &references {
        if file.path != source && file.path != destination {
            let doc = document_by_path
                .get(file.path.as_str())
                .context("Reviewed reference note not found.")?;
            edits.push(make_edit(doc, rewrite(doc)?));
        }
    }
    edits.push(make_edit(source_document, redirect.clone()));
    for edit in &edits {
        let path = &edit.revision.relative_path;
        ensure!(
            !drafts.contains(path),
            "{path} has a draft. Save or discard it before merging."
        );
    }
    for path in drafts {
        warnings.push(MarkdownRenameWarning {
            path: path.clone(),
            line: None,
            message: "This note has a draft. Reference analysis used the saved version.".into(),
        });
    }
    let files = edits
        .iter()
        .map(|edit| MergeRevision {
            path: edit.revision.relative_path.clone(),
            revision_id: edit.revision.id.clone(),
        })
        .collect();

    Ok(MarkdownMergePlan {
        preview: MarkdownMergePreview {
            id: Uuid::new_v4().to_string(),
            source,
            destination,
            merged_markdown: merged,
            source_redirect: redirect,
            files,
            references,
            warnings,
        },
        checkout,
        root,
        edits,
    })
}

fn ensure_distinct_anchors(
    source: &RepositoryDocument,
    destination: &RepositoryDocument,
) -> Result<()> {
    let source = source_link_note(&source.revision.relative_path, &source.markdown);
    let destination = source_link_note(&destination.revision.relative_path, &destination.markdown);
    for anchor in &source.anchors {
        let key = anchor_key(anchor.kind, &anchor.label);
        ensure!(
            !destination
                .anchors
                .iter()
                .any(|target| target.kind == anchor.kind
                    && anchor_key(target.kind, &target.label) == key),
            "Both notes have the anchor {}. Give it a unique name before merging so existing references keep their destination.",
            anchor.label
        );
    }
    Ok(())
}

fn ensure_distinct_footnotes(
    source: &RepositoryDocument,
    destination: &RepositoryDocument,
) -> Result<()> {
    let footnotes = |document: &RepositoryDocument| {
        analyze_markdown(&document.markdown, document.revision.id.clone())
            .nodes
            .into_iter()
            .filter_map(|node| match node.kind {
                MarkdownKind::Footnote {
                    name,
                    definition: true,
                } => Some(name.to_lowercase()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let source = footnotes(source);
    let destination = footnotes(destination);
    if let Some(name) = source.intersection(&destination).next() {
        bail!("Both notes define footnote {name}. Give it a unique name before merging.");
    }
    Ok(())
}

fn frontmatter_end(markdown: &str) -> usize {
    analyze_markdown(markdown, "merge-frontmatter".into())
        .nodes
        .into_iter()
        .find(|node| matches!(node.kind, MarkdownKind::Frontmatter { .. }))
        .map_or(0, |node| {
            let end = node.range.byte_end;
            // The closing delimiter's line break belongs to the frontmatter.
            end + usize::from(!markdown[..end].ends_with('\n') && markdown[end..].starts_with('\n'))
        })
}

fn append_section(before: &str, after: &str) -> String {
    let separators = before
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\n')
        .count()
        + after.bytes().take_while(|byte| *byte == b'\n').count();
    let spaces = if before.is_empty() || after.is_empty() {
        0
    } else {
        2usize.saturating_sub(separators)
    };
    format!("{before}{}{after}", "\n".repeat(spaces))
}

fn pin_revisions(root: &Path, edits: &[MergeEdit], max: u64) -> Result<Vec<File>> {
    let mut pins = Vec::new();
    for edit in edits {
        let path = &edit.revision.relative_path;
        let file = checkout_regular_file(root, path)?;
        pins.extend(lock_directories(
            file.parent().context("The note has no parent folder.")?,
        )?);
        let handle = open_locked_local_file(&file)?;
        let mut bytes = Vec::new();
        (&handle)
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= max
                && file_identity(&handle)? == edit.file_identity
                && format!("{:x}", Sha256::digest(&bytes)) == edit.revision.sha256,
            "{path} changed after the preview. Review a new merge."
        );
        let line_breaks = edit.markdown.bytes().filter(|byte| *byte == b'\n').count();
        let extra = if bytes.contains(&b'\r') {
            line_breaks
        } else {
            0
        };
        let bom = if bytes.starts_with("\u{feff}".as_bytes()) {
            3
        } else {
            0
        };
        ensure!(
            (edit.markdown.len() + extra + bom) as u64 <= max,
            "The merged draft for {path} exceeds the project file limit."
        );
        pins.push(handle);
    }
    Ok(pins)
}

#[cfg(test)]
mod tests;
