use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use yaml_edit::{YamlFile, YamlNode};

use crate::markdown_analysis::{MarkdownKind, MarkdownRange, analyze_markdown};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NoteMetadata {
    pub frontmatter: Option<String>,
    pub aliases: Vec<String>,
    pub properties: Vec<IndexedProperty>,
    pub headings: Vec<IndexedHeading>,
    pub blocks: Vec<IndexedBlock>,
    pub tags: BTreeSet<String>,
    pub links: Vec<IndexedNoteLink>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedProperty {
    pub name: String,
    pub source: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedHeading {
    pub label: String,
    pub level: u8,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedBlock {
    pub id: String,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexedLinkSyntax {
    Wiki,
    Markdown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedNoteLink {
    pub syntax: IndexedLinkSyntax,
    pub target: String,
    pub alias: Option<String>,
    pub anchor: Option<String>,
    pub embed: bool,
    pub line: usize,
    pub range: MarkdownRange,
}

pub(super) fn project(path: &str, search_text: &str, sha256: &str) -> (String, NoteMetadata) {
    let analysis = analyze_markdown(search_text, sha256.to_owned());
    let line_breaks = search_text
        .match_indices(['\r', '\n'])
        .filter_map(|(offset, matched)| {
            (matched == "\n" || !search_text[offset..].starts_with("\r\n")).then_some(offset)
        })
        .collect::<Vec<_>>();
    let mut metadata = NoteMetadata::default();
    let mut title = None;
    for node in analysis.nodes {
        let range = node.range;
        let line = line_breaks.partition_point(|offset| *offset < range.byte_start) + 1;
        match node.kind {
            MarkdownKind::Heading { level, text } => metadata.headings.push(IndexedHeading {
                label: text,
                level,
                line,
            }),
            MarkdownKind::BlockId { id } => metadata.blocks.push(IndexedBlock { id, line }),
            MarkdownKind::Tag { name } => {
                metadata.tags.insert(name);
            }
            MarkdownKind::Frontmatter { body_range } => {
                let content = &search_text[body_range.byte_start..body_range.byte_end];
                metadata.frontmatter = Some(format!(
                    "---\n{}---\n",
                    content.replace("\r\n", "\n").replace('\r', "\n")
                ));
                title = collect_properties(content, &mut metadata);
            }
            MarkdownKind::Wikilink {
                target,
                alias,
                anchor,
            }
            | MarkdownKind::Embed {
                target,
                alias,
                anchor,
            } => {
                let embed = search_text[range.byte_start..range.byte_end].starts_with('!');
                metadata.links.push(IndexedNoteLink {
                    syntax: IndexedLinkSyntax::Wiki,
                    target,
                    alias,
                    anchor,
                    embed,
                    line,
                    range,
                });
            }
            MarkdownKind::Link { target, text } | MarkdownKind::Image { target, text } => {
                let embed = search_text[range.byte_start..range.byte_end].starts_with('!');
                metadata.links.push(IndexedNoteLink {
                    syntax: IndexedLinkSyntax::Markdown,
                    target,
                    alias: Some(text),
                    anchor: None,
                    embed,
                    line,
                    range,
                });
            }
            _ => {}
        }
    }
    (title.unwrap_or_else(|| filename_title(path)), metadata)
}

fn filename_title(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_owned()
}

fn collect_properties(content: &str, metadata: &mut NoteMetadata) -> Option<String> {
    let yaml: YamlFile = content.parse().ok()?;
    let mut documents = yaml.documents();
    let mapping = documents.next()?.as_mapping()?;
    if documents.next().is_some() {
        return None;
    }
    let mut title = None;
    for entry in mapping.entries() {
        let (Some(name), Some(value)) = (
            entry.key_node().as_ref().and_then(scalar),
            entry.value_node(),
        ) else {
            continue;
        };
        match name.as_str() {
            "title" => title = scalar(&value),
            "aliases" | "alias" => metadata.aliases.extend(text_values(&value)),
            "tags" | "tag" => metadata.tags.extend(
                text_values(&value)
                    .into_iter()
                    .map(|tag| tag.trim_start_matches('#').to_owned())
                    .filter(|tag| !tag.is_empty()),
            ),
            _ => {}
        }
        metadata.properties.push(IndexedProperty {
            name,
            source: value.to_string(),
        });
    }
    title
}

fn scalar(value: &YamlNode) -> Option<String> {
    let text = value.as_scalar()?.as_string();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn text_values(value: &YamlNode) -> Vec<String> {
    value.as_sequence().map_or_else(
        || scalar(value).into_iter().collect(),
        |sequence| sequence.values().filter_map(|item| scalar(&item)).collect(),
    )
}
