use std::{collections::BTreeSet, ops::Range, path::PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use uuid::Uuid;
use yaml_edit::{ScalarValue, YamlFile, YamlNode};

use crate::{
    markdown_analysis::{MarkdownKind, analyze_markdown, valid_tag_name},
    markdown_recovery::RepositoryMarkdownDraft,
    store::Store,
};

use super::{
    checkout_root,
    documents::RepositoryDocumentOpen,
    markdown_catalog::note_paths,
    markdown_links::fold,
    markdown_renames::{MarkdownRenameEdit, MarkdownRenameWarning, replace_ranges},
    reviewed_notes::{ReviewedNoteEdit, ReviewedNoteRevision},
    verify_checkout,
};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownTagRenamePreview {
    pub id: String,
    source: String,
    destination: String,
    files: Vec<TagRenameFile>,
    blocked: Vec<String>,
    warnings: Vec<MarkdownRenameWarning>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TagRenameFile {
    #[serde(flatten)]
    revision: ReviewedNoteRevision,
    occurrences: Vec<TagOccurrence>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TagOccurrence {
    #[serde(flatten)]
    edit: MarkdownRenameEdit,
    property: bool,
}

pub struct MarkdownTagRenamePlan {
    pub preview: MarkdownTagRenamePreview,
    checkout: String,
    root: PathBuf,
    edits: Vec<ReviewedNoteEdit>,
}

struct TagChange {
    source: String,
    folded_source: String,
    levels: usize,
    destination: String,
    include_nested: bool,
}

impl TagChange {
    fn new(source: &str, destination: &str, include_nested: bool) -> Result<Self> {
        let normalize = |tag: &str| -> Result<String> {
            let tag = tag.trim();
            let name = tag.strip_prefix('#').unwrap_or(tag);
            ensure!(
                valid_tag_name(name),
                "Enter a valid tag using letters, digits, underscores, hyphens, and slashes."
            );
            Ok(name.into())
        };
        let source = normalize(source)?;
        let destination = normalize(destination)?;
        ensure!(source != destination, "Enter a different tag name.");
        Ok(Self {
            folded_source: fold(&source),
            levels: source.split('/').count(),
            source,
            destination,
            include_nested,
        })
    }

    fn replacement(&self, existing: &str) -> Option<String> {
        if fold(existing) == self.folded_source {
            return Some(self.destination.clone());
        }
        if self.include_nested
            && let Some((index, _)) = existing.match_indices('/').nth(self.levels - 1)
            && fold(&existing[..index]) == self.folded_source
        {
            return Some(self.destination.clone() + &existing[index..]);
        }
        None
    }
}

impl Store {
    pub async fn preview_markdown_tag_rename(
        &self,
        checkout: String,
        source: String,
        destination: String,
        include_nested: bool,
    ) -> Result<MarkdownTagRenamePlan> {
        let change = TagChange::new(&source, &destination, include_nested)?;
        let root = checkout_root(self, checkout.clone()).await?;
        let root = verify_checkout(self, checkout.clone(), root).await?.root;
        let paths = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || note_paths(&root)).await??
        };
        let draft_paths: BTreeSet<_> = self
            .repository_markdown_draft_paths(checkout.clone())
            .await?
            .into_iter()
            .collect();
        let mut files = Vec::new();
        let mut edits = Vec::new();
        let mut blocked = BTreeSet::new();
        let mut warnings = Vec::new();
        for path in paths {
            if draft_paths.contains(&path) {
                match self
                    .repository_markdown_draft(checkout.clone(), path.clone())
                    .await
                {
                    Ok(Some(draft))
                        if !occurrences(&draft.markdown, &path, &change, &mut Vec::new())
                            .is_empty() =>
                    {
                        blocked.insert(path.clone());
                    }
                    Err(_) => warnings.push(MarkdownRenameWarning {
                        path: path.clone(),
                        line: None,
                        message: "This draft could not be checked for tags.".into(),
                    }),
                    _ => {}
                }
            }
            let Ok(RepositoryDocumentOpen::Document { document }) = self
                .open_repository_document(checkout.clone(), path.clone().into())
                .await
            else {
                warnings.push(MarkdownRenameWarning {
                    path,
                    line: None,
                    message: "This note could not be checked for tags.".into(),
                });
                continue;
            };
            if document.revision.relative_path != path {
                warnings.push(MarkdownRenameWarning {
                    path,
                    line: None,
                    message: "This note no longer resolves to its original path.".into(),
                });
                continue;
            }
            let found = occurrences(&document.markdown, &path, &change, &mut warnings);
            if found.is_empty() {
                continue;
            }
            if draft_paths.contains(&path) {
                blocked.insert(path.clone());
            }
            let spans = found
                .iter()
                .map(|item| item.edit.clone())
                .collect::<Vec<_>>();
            let after = replace_ranges(document.markdown.clone(), &spans)
                .with_context(|| format!("The tag ranges overlap in {path}."))?;
            let edit = ReviewedNoteEdit::new(&document, after);
            files.push(TagRenameFile {
                revision: edit.reviewed_revision(),
                occurrences: found,
            });
            edits.push(edit);
        }
        let preview = MarkdownTagRenamePreview {
            id: Uuid::new_v4().to_string(),
            source: change.source,
            destination: change.destination,
            files,
            blocked: blocked.into_iter().collect(),
            warnings,
        };
        Ok(MarkdownTagRenamePlan {
            preview,
            checkout,
            root,
            edits,
        })
    }

    pub async fn stage_markdown_tag_rename(
        &self,
        checkout: String,
        plan: MarkdownTagRenamePlan,
    ) -> Result<Vec<RepositoryMarkdownDraft>> {
        ensure!(
            checkout == plan.checkout,
            "The tag preview belongs to a different checkout."
        );
        ensure!(
            plan.preview.blocked.is_empty(),
            "Save or discard the affected drafts and review a new preview."
        );
        ensure!(
            !plan.edits.is_empty(),
            "This preview contains no tag changes."
        );
        self.stage_reviewed_note_edits(checkout, plan.root, plan.edits)
            .await
    }
}

fn occurrences(
    markdown: &str,
    path: &str,
    change: &TagChange,
    warnings: &mut Vec<MarkdownRenameWarning>,
) -> Vec<TagOccurrence> {
    let mut results = Vec::new();
    let breaks = markdown
        .match_indices('\n')
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let line = |byte| breaks.partition_point(|offset| *offset < byte) + 1;
    for node in analyze_markdown(markdown, "rename-tags".into()).nodes {
        let range = node.range.byte_start..node.range.byte_end;
        match node.kind {
            MarkdownKind::Tag { name } => {
                if let Some(next) = change.replacement(&name) {
                    results.extend(occurrence(
                        markdown,
                        range.clone(),
                        format!("#{next}"),
                        line(range.start),
                        false,
                    ));
                }
            }
            MarkdownKind::Frontmatter { body_range } => {
                let start = body_range.byte_start;
                let body = &markdown[start..body_range.byte_end];
                match properties(body, change) {
                    Ok((spans, skipped)) => {
                        for (relative, next) in spans {
                            let range = (start + relative.start)..(start + relative.end);
                            results.extend(occurrence(
                                markdown,
                                range.clone(),
                                next,
                                line(range.start),
                                true,
                            ));
                        }
                        for byte in skipped {
                            warnings.push(MarkdownRenameWarning {
                                path: path.into(), line: Some(line(start + byte)),
                                message: "Review this tag property manually because its scalar spelling spans lines or uses escapes.".into(),
                            });
                        }
                    }
                    Err(_) => warnings.push(MarkdownRenameWarning {
                        path: path.into(),
                        line: Some(line(start)),
                        message: "This note's YAML properties could not be checked for tags."
                            .into(),
                    }),
                }
            }
            _ => {}
        }
    }
    results.sort_unstable_by_key(|item| item.edit.byte_start);
    results
}

fn occurrence(
    markdown: &str,
    range: Range<usize>,
    after: String,
    line: usize,
    property: bool,
) -> Option<TagOccurrence> {
    let before = markdown.get(range.clone())?;
    (before != after).then(|| TagOccurrence {
        edit: MarkdownRenameEdit {
            byte_start: range.start,
            byte_end: range.end,
            line,
            before: before.into(),
            after,
        },
        property,
    })
}

type PropertyEdits = (Vec<(Range<usize>, String)>, Vec<usize>);

fn properties(body: &str, change: &TagChange) -> Result<PropertyEdits> {
    let yaml: YamlFile = body.parse()?;
    let mut documents = yaml.documents();
    let mapping = documents
        .next()
        .and_then(|doc| doc.as_mapping())
        .context("Tag properties must be in a YAML mapping.")?;
    ensure!(
        documents.next().is_none(),
        "The frontmatter contains more than one YAML document."
    );
    let mut spans = Vec::new();
    let mut skipped = Vec::new();
    for property in mapping
        .entries()
        .filter(|entry| entry.key_matches("tags") || entry.key_matches("tag"))
    {
        let Some(value) = property.value_node() else {
            continue;
        };
        if let Some(sequence) = value.as_sequence() {
            for item in sequence.values() {
                property_tag(body, &item, change, &mut spans, &mut skipped);
            }
        } else {
            property_tag(body, &value, change, &mut spans, &mut skipped);
        }
    }
    Ok((spans, skipped))
}

fn property_tag(
    body: &str,
    value: &YamlNode,
    change: &TagChange,
    spans: &mut Vec<(Range<usize>, String)>,
    skipped: &mut Vec<usize>,
) {
    let Some(scalar) = value.as_scalar() else {
        return;
    };
    let original = scalar.as_string();
    let tag = original.trim();
    let bare = tag.strip_prefix('#').unwrap_or(tag);
    let Some(next) = change.replacement(bare) else {
        return;
    };
    let next = if tag.starts_with('#') {
        format!("#{next}")
    } else {
        next
    };
    let range = value.byte_range();
    let start = range.start as usize;
    let end = range.end as usize;
    let Some(raw) = body.get(start..end) else {
        skipped.push(start);
        return;
    };
    let quoted = raw
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        });
    let text = quoted.unwrap_or(raw);
    if text.contains(['\n', '\r']) || text.trim() != tag {
        skipped.push(start);
        return;
    }
    let leading = text.len() - text.trim_start().len();
    let at = start + usize::from(quoted.is_some()) + leading;
    let next = if quoted.is_some() {
        next
    } else {
        ScalarValue::string(next).to_yaml_string()
    };
    spans.push((at..at + tag.len(), next));
}

#[cfg(test)]
mod tests;
