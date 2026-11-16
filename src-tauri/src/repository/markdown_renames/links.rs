use anyhow::{Context, Result, ensure};
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use crate::{
    markdown_analysis::{MarkdownKind, analyze_markdown},
    providers::configuration::plain_metadata,
};

use super::super::{
    markdown_index::source_link_note,
    markdown_links::{
        MarkdownLinkQuery, MarkdownLinkResolution, MarkdownLinkResolver, MarkdownLinkSyntax,
    },
};
use super::{
    FileIdentity, MarkdownRenameEdit, MarkdownRenameFile, MarkdownRenameWarning, RepositoryDocument,
};

pub(super) fn planned_references(
    root: &Path,
    source: &str,
    destination: &str,
    documents: &[RepositoryDocument],
    warnings: &mut Vec<MarkdownRenameWarning>,
) -> (Vec<MarkdownRenameFile>, Vec<FileIdentity>) {
    let before_notes = documents
        .iter()
        .map(|document| source_link_note(&document.revision.relative_path, &document.markdown))
        .collect::<Vec<_>>();
    let mut after_notes = before_notes.clone();
    if let Some(moved) = after_notes.iter_mut().find(|note| note.path == source) {
        moved.path = destination.to_owned();
    }
    let before = MarkdownLinkResolver::new(&before_notes);
    let after = MarkdownLinkResolver::new(&after_notes);
    let mut preview = Vec::new();
    let mut identities = Vec::new();
    for document in documents {
        let path = &document.revision.relative_path;
        let text = &document.markdown;
        let effective_path = if path == source { destination } else { path };
        let nodes = analyze_markdown(text, document.revision.id.clone()).nodes;
        let protected = nodes
            .iter()
            .filter_map(|node| {
                matches!(
                    node.kind,
                    MarkdownKind::CodeBlock { .. }
                        | MarkdownKind::CodeSpan
                        | MarkdownKind::Frontmatter { .. }
                        | MarkdownKind::Comment
                )
                .then_some(node.range.byte_start..node.range.byte_end)
            })
            .collect::<Vec<_>>();
        let tables = nodes
            .iter()
            .filter_map(|node| {
                matches!(node.kind, MarkdownKind::Table)
                    .then_some(node.range.byte_start..node.range.byte_end)
            })
            .collect::<Vec<_>>();
        let mut edits = Vec::new();
        for node in &nodes {
            let span = node.range.byte_start..node.range.byte_end;
            if protected.iter().any(|range| overlaps(range, &span)) {
                continue;
            }
            let query = match &node.kind {
                MarkdownKind::Wikilink { target, anchor, .. }
                | MarkdownKind::Embed { target, anchor, .. } => Some(MarkdownLinkQuery {
                    source_path: path.clone(),
                    target: target.clone(),
                    anchor: anchor.clone(),
                    syntax: MarkdownLinkSyntax::Wiki,
                }),
                MarkdownKind::Link { target, .. } | MarkdownKind::Image { target, .. } => {
                    MarkdownLinkQuery::from_markdown(path.clone(), target)
                }
                _ => None,
            };
            let Some(mut query) = query else { continue };
            let Some(original) = resolved_path(root, before.resolve(&query)) else {
                continue;
            };
            if original != source && effective_path == path {
                continue;
            }
            let intended = if original == source {
                destination
            } else {
                &original
            };
            query.source_path = effective_path.to_owned();
            if resolved_path(root, after.resolve(&query)).as_deref() == Some(intended) {
                continue;
            }
            let Some(raw) = text.get(span.clone()) else {
                continue;
            };
            let line = text[..span.start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            let in_table = tables.iter().any(|table| overlaps(table, &span));
            let target_range = match query.syntax {
                MarkdownLinkSyntax::Wiki => wiki_target_range(raw, in_table, false),
                MarkdownLinkSyntax::Markdown => markdown_target_range(raw),
            };
            let Some(range) = target_range else {
                warnings.push(MarkdownRenameWarning { path: path.clone(), line: Some(line), message: "the link requires a manual rewrite because its exact target span is unavailable".into() });
                continue;
            };
            let normalized = rewrite_address(&query, intended);
            let encoded = match query.syntax {
                MarkdownLinkSyntax::Wiki => escape_wiki(&normalized),
                MarkdownLinkSyntax::Markdown => escape_markdown(&normalized),
            };
            let authored = std::mem::replace(&mut query.target, normalized);
            let exact = resolved_path(root, after.resolve(&query)).as_deref() == Some(intended);
            if !exact || !valid_replacement(raw, &range, &encoded, &query, in_table) {
                warnings.push(MarkdownRenameWarning {
                    path: path.clone(),
                    line: Some(line),
                    message: "the link could not be rewritten without changing its syntax".into(),
                });
                continue;
            }
            if authored != query.target || raw[range.clone()] != encoded {
                edits.push(MarkdownRenameEdit {
                    byte_start: span.start + range.start,
                    byte_end: span.start + range.end,
                    line,
                    before: raw[range].to_owned(),
                    after: encoded,
                });
            }
        }
        edits.sort_unstable_by_key(|edit| (edit.byte_start, edit.byte_end));
        edits.dedup();
        if edits.is_empty() {
            continue;
        }
        if edits
            .windows(2)
            .any(|pair| pair[0].byte_end > pair[1].byte_start)
        {
            warnings.push(MarkdownRenameWarning {
                path: path.clone(),
                line: None,
                message: "overlapping link ranges need manual review".into(),
            });
            continue;
        }
        preview.push(MarkdownRenameFile {
            path: path.clone(),
            revision_id: document.revision.id.clone(),
            edits,
        });
        identities.push(FileIdentity {
            path: path.clone(),
            identity: document.file_identity.clone(),
        });
    }
    (preview, identities)
}

fn overlaps(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn resolved_path(root: &Path, resolution: MarkdownLinkResolution) -> Option<String> {
    match resolution {
        MarkdownLinkResolution::Resolved { location }
        | MarkdownLinkResolution::MissingAnchor { location } => Some(location.path),
        MarkdownLinkResolution::Attachment { paths } => paths.into_iter().find(|relative| {
            let path: PathBuf = root.join(relative).components().collect();
            plain_metadata(&path)
                .ok()
                .flatten()
                .is_some_and(|metadata| metadata.is_file())
                && dunce::canonicalize(&path).ok().is_some_and(|canonical| {
                    canonical.starts_with(root)
                        && canonical.to_string_lossy().to_lowercase()
                            == path.to_string_lossy().to_lowercase()
                })
        }),
        _ => None,
    }
}

fn rewrite_address(query: &MarkdownLinkQuery, intended: &str) -> String {
    let raw = query.target.replace('\\', "/");
    let relative_wiki = raw.starts_with("./") || raw.starts_with("../");
    if raw.starts_with('/') || (query.syntax == MarkdownLinkSyntax::Wiki && !relative_wiki) {
        return format!("/{intended}");
    }
    let mut source_parent = query.source_path.split('/').collect::<Vec<_>>();
    source_parent.pop();
    let target = intended.split('/').collect::<Vec<_>>();
    let shared = source_parent
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left.to_lowercase() == right.to_lowercase())
        .count();
    let prefix = "../".repeat(source_parent.len() - shared);
    let relative = prefix + &target[shared..].join("/");
    if (relative_wiki || raw.starts_with("./")) && !relative.starts_with("../") {
        return format!("./{relative}");
    }
    relative
}

fn wiki_target_range(raw: &str, in_table: bool, include_fragment: bool) -> Option<Range<usize>> {
    let prefix = if raw.starts_with("![[") {
        3
    } else if raw.starts_with("[[") {
        2
    } else {
        return None;
    };
    let body = raw.get(prefix..)?.strip_suffix("]]")?;
    let mut chars = body.char_indices().peekable();
    let mut end = body.len();
    while let Some((index, ch)) = chars.next() {
        if ch == '|'
            || (!include_fragment && ch == '#')
            || (ch == '\\' && in_table && chars.peek().is_some_and(|(_, next)| *next == '|'))
        {
            end = index;
            break;
        }
        if ch == '\\'
            && chars
                .peek()
                .is_some_and(|(_, next)| matches!(*next, '#' | '|' | '[' | ']' | '\\'))
        {
            chars.next();
        }
    }
    trimmed_range(raw, prefix..prefix + end)
}

pub fn replace_wiki_note_reference(
    markdown: &str,
    raw: &str,
    source_line: Option<u32>,
    destination: &str,
    anchor: Option<&str>,
) -> Result<String> {
    ensure!(
        !destination.is_empty()
            && !destination.contains(['\\', ':', '\r', '\n', '\0'])
            && destination
                .split('/')
                .all(|component| !component.is_empty() && !matches!(component, "." | "..")),
        "select a note path in the checkout"
    );
    ensure!(
        anchor.is_none_or(|value| !value.contains(['\r', '\n', '\0'])),
        "enter a heading or block on one line"
    );
    let nodes = analyze_markdown(markdown, "wiki-repair".into()).nodes;
    let mut selected = None;
    for node in &nodes {
        let span = node.range.byte_start..node.range.byte_end;
        let (target, fragment, alias) = match &node.kind {
            MarkdownKind::Wikilink {
                target,
                anchor,
                alias,
            }
            | MarkdownKind::Embed {
                target,
                anchor,
                alias,
            } => (target, anchor, alias),
            _ => continue,
        };
        let line = markdown[..span.start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32
            + 1;
        let authored = &markdown[span.clone()];
        let inner = authored
            .trim_start_matches('!')
            .strip_prefix("[[")
            .and_then(|body| body.strip_suffix("]]"));
        let mut canonical = target.clone();
        if let Some(fragment) = fragment {
            canonical.push('#');
            canonical.push_str(fragment);
        }
        if let Some(alias) = alias {
            canonical.push('|');
            canonical.push_str(alias);
        }
        if (inner != Some(raw) && canonical != raw)
            || source_line.is_some_and(|wanted| wanted != line)
        {
            continue;
        }
        if nodes.iter().any(|outer| {
            matches!(
                outer.kind,
                MarkdownKind::CodeBlock { .. }
                    | MarkdownKind::CodeSpan
                    | MarkdownKind::Frontmatter { .. }
                    | MarkdownKind::Comment
            ) && overlaps(&(outer.range.byte_start..outer.range.byte_end), &span)
        }) {
            continue;
        }
        let in_table = nodes.iter().any(|outer| {
            matches!(outer.kind, MarkdownKind::Table)
                && overlaps(&(outer.range.byte_start..outer.range.byte_end), &span)
        });
        let range = wiki_target_range(authored, in_table, true)
            .context("the reference target could not be located")?;
        ensure!(
            selected.is_none(),
            "more than one matching link was found. Select its source line to repair it"
        );
        selected = Some((span, range, in_table));
    }
    let (span, range, in_table) =
        selected.context("the reference is no longer at the selected location")?;
    let target = format!("/{destination}");
    let mut replacement = escape_wiki(&target);
    if let Some(anchor) = anchor {
        replacement.push('#');
        replacement.push_str(&escape_wiki(anchor));
    }
    let query = MarkdownLinkQuery {
        source_path: String::new(),
        target,
        anchor: anchor.map(str::to_owned),
        syntax: MarkdownLinkSyntax::Wiki,
    };
    ensure!(
        valid_replacement(
            &markdown[span.clone()],
            &range,
            &replacement,
            &query,
            in_table
        ),
        "the selected destination could not be represented in this reference"
    );
    let mut result = markdown.to_owned();
    result.replace_range(
        span.start + range.start..span.start + range.end,
        &replacement,
    );
    Ok(result)
}

fn markdown_target_range(raw: &str) -> Option<Range<usize>> {
    let bracket = usize::from(raw.starts_with('!'));
    let label = raw.get(bracket..)?.strip_prefix('[')?;
    let label_end = matching_boundary(label, '[', ']', false)?;
    let start = bracket + 1 + label_end + 1;
    let rest = raw.get(start..)?.strip_prefix('(')?;
    let target = rest.trim_start();
    let start = start + 1 + rest.len() - target.len();
    let (path, start) = if let Some(angle) = target.strip_prefix('<') {
        (
            &angle[..matching_boundary(angle, '\0', '>', false)?],
            start + 1,
        )
    } else {
        (&target[..matching_boundary(target, '(', ')', true)?], start)
    };
    let stop = path
        .char_indices()
        .find_map(|(index, ch)| (ch == '#').then_some(index))
        .unwrap_or(path.len());
    trimmed_range(raw, start..start + stop)
}

fn matching_boundary(text: &str, open: char, close: char, stops_at_space: bool) -> Option<usize> {
    let mut depth = 0usize;
    let mut chars = text.char_indices();
    while let Some((index, ch)) = chars.next() {
        if ch == '\\' {
            chars.next();
        } else if ch == open {
            depth += 1;
        } else if ch == close {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
        } else if stops_at_space && ch.is_ascii_whitespace() {
            return Some(index);
        }
    }
    None
}

fn trimmed_range(raw: &str, range: Range<usize>) -> Option<Range<usize>> {
    let target = raw.get(range.clone())?;
    let trimmed = target.trim();
    let start = range.start + target.len() - target.trim_start().len();
    (!trimmed.is_empty()).then_some(start..start + trimmed.len())
}

fn escape_wiki(path: &str) -> String {
    let mut output = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '[' | ']' | '#' | '|') {
            output.push('\\');
        }
        output.push(ch);
    }
    output
}

fn escape_markdown(path: &str) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/_.-~".contains(&byte) {
            output.push(char::from(byte));
        } else {
            let _ = write!(&mut output, "%{byte:02X}");
        }
    }
    output
}

fn valid_replacement(
    raw: &str,
    range: &Range<usize>,
    replacement: &str,
    query: &MarkdownLinkQuery,
    in_table: bool,
) -> bool {
    let mut link = raw.to_owned();
    link.replace_range(range.clone(), replacement);
    if in_table {
        link = format!("| Target |\n| --- |\n| {link} |");
    }
    analyze_markdown(&link, "replacement".into())
        .nodes
        .into_iter()
        .any(|node| {
            let actual = match node.kind {
                MarkdownKind::Wikilink { target, anchor, .. }
                | MarkdownKind::Embed { target, anchor, .. } => Some(MarkdownLinkQuery {
                    source_path: query.source_path.clone(),
                    target,
                    anchor,
                    syntax: MarkdownLinkSyntax::Wiki,
                }),
                MarkdownKind::Link { target, .. } | MarkdownKind::Image { target, .. } => {
                    MarkdownLinkQuery::from_markdown(&query.source_path, &target)
                }
                _ => None,
            };
            actual.as_ref() == Some(query)
        })
}
