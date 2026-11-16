use std::{borrow::Cow, cmp::Reverse, ops::Range, sync::LazyLock};

use comrak::{
    Arena, Options,
    nodes::{CalloutFold, ListType, Node, NodeValue, Sourcepos},
    parse_document,
};
use regex::Regex;
use serde::{Deserialize, Serialize};

mod source;
use source::{SourceMap, TableCellMap};

#[cfg(test)]
mod tests;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownAnalysis {
    pub buffer_revision: String,
    pub nodes: Vec<MarkdownNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownRange {
    pub from: usize,
    pub to: usize,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Debug, Serialize)]
pub struct MarkdownNode {
    pub range: MarkdownRange,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<MarkdownRange>,
    #[serde(flatten)]
    pub kind: MarkdownKind,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MarkdownKind {
    Heading {
        level: u8,
        text: String,
    },
    List {
        ordered: bool,
        start: Option<usize>,
    },
    ListItem {
        marker_range: MarkdownRange,
    },
    TaskMarker {
        checked: bool,
        status: char,
        value_range: MarkdownRange,
    },
    Wikilink {
        target: String,
        alias: Option<String>,
        anchor: Option<String>,
    },
    Embed {
        target: String,
        alias: Option<String>,
        anchor: Option<String>,
    },
    Link {
        target: String,
        text: String,
    },
    Image {
        target: String,
        text: String,
    },
    Frontmatter {
        body_range: MarkdownRange,
    },
    Callout {
        callout_type: String,
        title: String,
        fold: Option<&'static str>,
        metadata: Option<String>,
    },
    Tag {
        name: String,
    },
    BlockId {
        id: String,
    },
    Table,
    CodeBlock {
        language: String,
        text: String,
    },
    CodeSpan,
    Footnote {
        name: String,
        definition: bool,
    },
    Math {
        display: bool,
        text: String,
    },
    Emphasis,
    Strong,
    Strikethrough,
    Highlight,
    Comment,
}

/// Analyzes a snapshot of the editor text. Byte ranges address the input as supplied.
/// `from` and `to` address the same buffer in CodeMirror (UTF-16, one unit per line break).
pub fn analyze_markdown(text: &str, buffer_revision: String) -> MarkdownAnalysis {
    let map = SourceMap::new(text);
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let input = if body.contains('\r') {
        Cow::Owned(body.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(body)
    };
    let arena = Arena::new();
    let doc = parse_document(&arena, &input, &options());
    let mut nodes = Vec::new();
    let mut table_cell = None;
    for node in doc.descendants() {
        let ast = node.data();
        if matches!(ast.value, NodeValue::TableCell) {
            table_cell = map
                .ast_range(ast.sourcepos)
                .and_then(|range| TableCellMap::new(text, range));
            continue;
        }
        if ast.value.block() {
            table_cell = None;
        }
        let Some(range) = map.ast_range_in_cell(ast.sourcepos, table_cell.as_ref()) else {
            continue;
        };
        let span = SyntaxSpan { map: &map, range };
        match &ast.value {
            NodeValue::Heading(heading) => {
                let markers = span.heading_markers(heading.level, heading.setext, heading.closed);
                span.push(
                    &mut nodes,
                    MarkdownKind::Heading {
                        level: heading.level,
                        text: node.collect_text(),
                    },
                    markers,
                );
            }
            NodeValue::List(list) => {
                let ordered = list.list_type == ListType::Ordered;
                span.push(
                    &mut nodes,
                    MarkdownKind::List {
                        ordered,
                        start: ordered.then_some(list.start),
                    },
                    vec![],
                );
            }
            NodeValue::Item(_) => span.list_item(&mut nodes),
            NodeValue::TaskItem(task) => {
                span.list_item(&mut nodes);
                if let Some(marker) =
                    task_marker(&map, task.symbol_sourcepos, task.symbol.unwrap_or(' '))
                {
                    nodes.push(marker);
                }
            }
            NodeValue::WikiLink(link) => {
                let source = table_cell
                    .as_ref()
                    .map_or(Cow::Borrowed(span.text()), |cell| {
                        cell.unescaped(text, span.range.clone())
                    });
                if let Some(kind) = wiki_link(&source, link.embed) {
                    span.push(&mut nodes, kind, vec![]);
                }
            }
            NodeValue::Link(link) => span.push(
                &mut nodes,
                MarkdownKind::Link {
                    target: link.url.clone(),
                    text: node.collect_text(),
                },
                vec![],
            ),
            NodeValue::Image(image) => span.push(
                &mut nodes,
                MarkdownKind::Image {
                    target: image.url.clone(),
                    text: node.collect_text(),
                },
                vec![],
            ),
            NodeValue::FrontMatter(_) => {
                if let Some(body_range) = span.frontmatter_body() {
                    span.push(&mut nodes, MarkdownKind::Frontmatter { body_range }, vec![]);
                }
            }
            NodeValue::Callout(callout) => {
                let title = node
                    .first_child()
                    .filter(|child| matches!(child.data().value, NodeValue::CalloutTitle))
                    .map(|child| child.collect_text())
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| callout.default_title());
                let fold = match callout.fold {
                    CalloutFold::None => None,
                    CalloutFold::Expanded => Some("expanded"),
                    CalloutFold::Collapsed => Some("collapsed"),
                };
                span.push(
                    &mut nodes,
                    MarkdownKind::Callout {
                        callout_type: callout.callout_type.clone(),
                        title,
                        fold,
                        metadata: callout.metadata.clone(),
                    },
                    vec![],
                );
            }
            NodeValue::Table(_) => span.push(&mut nodes, MarkdownKind::Table, vec![]),
            NodeValue::CodeBlock(code) => span.push(
                &mut nodes,
                MarkdownKind::CodeBlock {
                    language: code
                        .info
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                    text: code.literal.clone(),
                },
                vec![],
            ),
            NodeValue::Code(code) => {
                span.delimited(&mut nodes, MarkdownKind::CodeSpan, code.num_backticks, b"`")
            }
            NodeValue::FootnoteDefinition(note) => span.push(
                &mut nodes,
                MarkdownKind::Footnote {
                    name: note.name.clone(),
                    definition: true,
                },
                vec![],
            ),
            NodeValue::FootnoteReference(note) => span.push(
                &mut nodes,
                MarkdownKind::Footnote {
                    name: note.name.clone(),
                    definition: false,
                },
                vec![],
            ),
            NodeValue::Math(math) => span.push(
                &mut nodes,
                MarkdownKind::Math {
                    display: math.display_math,
                    text: math.literal.clone(),
                },
                vec![],
            ),
            NodeValue::Emph => span.delimited(&mut nodes, MarkdownKind::Emphasis, 1, b"*_"),
            NodeValue::Strong => span.delimited(&mut nodes, MarkdownKind::Strong, 2, b"*_"),
            NodeValue::Strikethrough => {
                let width = if span.text().starts_with("~~") { 2 } else { 1 };
                span.delimited(&mut nodes, MarkdownKind::Strikethrough, width, b"~");
            }
            NodeValue::Highlight => span.delimited(&mut nodes, MarkdownKind::Highlight, 2, b"="),
            NodeValue::CommentInline(_) | NodeValue::MultilineComment(_) => {
                span.push(&mut nodes, MarkdownKind::Comment, vec![])
            }
            NodeValue::Text(_)
                if span.text().contains(['#', '^'])
                    && !node.ancestors().skip(1).any(ignores_text_extensions) =>
            {
                span.text_extensions(&mut nodes);
            }
            _ => {}
        }
    }
    nodes.sort_by_key(|node| (node.range.byte_start, Reverse(node.range.byte_end)));
    MarkdownAnalysis {
        buffer_revision,
        nodes,
    }
}

fn options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.autolink = true;
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.front_matter_delimiter = Some("---".into());
    options.extension.ofm_comments = true;
    options.extension.ofm_callouts = true;
    options.extension.highlight = true;
    options.extension.math_dollars = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.parse.leave_footnote_definitions = true;
    options.parse.relaxed_tasklist_matching = true;
    options
}

fn task_marker(map: &SourceMap<'_>, position: Sourcepos, status: char) -> Option<MarkdownNode> {
    let value = map.ast_range(position)?;
    if map.text[value.clone()].chars().ne(std::iter::once(status)) {
        return None;
    }
    let marker = value.start.checked_sub(1)?..value.end + 1;
    if map.text.as_bytes().get(marker.start) != Some(&b'[')
        || map.text.as_bytes().get(value.end) != Some(&b']')
    {
        return None;
    }
    Some(MarkdownNode {
        range: map.editor_range(marker),
        markers: vec![],
        kind: MarkdownKind::TaskMarker {
            checked: matches!(status, 'x' | 'X'),
            status,
            value_range: map.editor_range(value),
        },
    })
}

fn wiki_link(source: &str, embed: bool) -> Option<MarkdownKind> {
    let source = source
        .strip_prefix(if embed { "![[" } else { "[[" })?
        .strip_suffix("]]")?;
    let (destination, alias) = split_wiki(source, '|');
    let (target, anchor) = split_wiki(destination, '#');
    let target = unescape_wiki(target);
    let alias = alias.map(unescape_wiki);
    let anchor = anchor.map(unescape_wiki);
    Some(if embed {
        MarkdownKind::Embed {
            target,
            alias,
            anchor,
        }
    } else {
        MarkdownKind::Wikilink {
            target,
            alias,
            anchor,
        }
    })
}

fn split_wiki(source: &str, delimiter: char) -> (&str, Option<&str>) {
    let mut escaped = false;
    for (index, ch) in source.char_indices() {
        if !escaped && ch == delimiter {
            return (&source[..index], Some(&source[index + ch.len_utf8()..]));
        }
        escaped = !escaped && ch == '\\';
    }
    (source, None)
}

fn unescape_wiki(source: &str) -> String {
    let mut output = String::new();
    let mut chars = source.trim().chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && matches!(chars.peek(), Some('[' | ']' | '#' | '|' | '\\')) {
            output.extend(chars.next());
        } else {
            output.push(ch);
        }
    }
    output
}

fn ignores_text_extensions(node: Node<'_>) -> bool {
    matches!(
        node.data().value,
        NodeValue::Link(_) | NodeValue::Image(_) | NodeValue::WikiLink(_) | NodeValue::Escaped
    )
}

static OBSIDIAN_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#[\p{L}\p{M}\p{N}_-]+(?:/[\p{L}\p{M}\p{N}_-]+)*|\^[a-zA-Z0-9-]+")
        .expect("valid Obsidian token pattern")
});

struct SyntaxSpan<'a, 's> {
    map: &'a SourceMap<'s>,
    range: Range<usize>,
}

impl SyntaxSpan<'_, '_> {
    fn text(&self) -> &str {
        &self.map.text[self.range.clone()]
    }

    fn push(&self, nodes: &mut Vec<MarkdownNode>, kind: MarkdownKind, markers: Vec<MarkdownRange>) {
        nodes.push(MarkdownNode {
            range: self.map.editor_range(self.range.clone()),
            kind,
            markers,
        });
    }

    fn list_item(&self, nodes: &mut Vec<MarkdownNode>) {
        let source = self.text().as_bytes();
        let digits = source
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let width = match source.get(digits) {
            Some(b'.' | b')') if digits > 0 => digits + 1,
            Some(b'-' | b'+' | b'*') if digits == 0 => 1,
            _ => return,
        };
        let marker_range = self
            .map
            .editor_range(self.range.start..self.range.start + width);
        self.push(nodes, MarkdownKind::ListItem { marker_range }, vec![]);
    }

    fn delimited(
        &self,
        nodes: &mut Vec<MarkdownNode>,
        kind: MarkdownKind,
        width: usize,
        allowed: &[u8],
    ) {
        let bytes = self.text().as_bytes();
        let mut markers = Vec::new();
        if width > 0
            && bytes.len() >= 2 * width
            && allowed.contains(&bytes[0])
            && bytes[..width]
                .iter()
                .chain(&bytes[bytes.len() - width..])
                .all(|b| *b == bytes[0])
        {
            markers.push(
                self.map
                    .editor_range(self.range.start..self.range.start + width),
            );
            markers.push(
                self.map
                    .editor_range(self.range.end - width..self.range.end),
            );
        }
        self.push(nodes, kind, markers);
    }

    fn heading_markers(&self, level: u8, setext: bool, closed: bool) -> Vec<MarkdownRange> {
        let raw = self.text();
        if setext {
            let trimmed = raw.trim_end_matches([' ', '\t']);
            let symbol = if level == 1 { b'=' } else { b'-' };
            let width = trimmed
                .bytes()
                .rev()
                .take_while(|&byte| byte == symbol)
                .count();
            if width > 0 {
                let end = self.range.start + trimmed.len();
                return vec![self.map.editor_range(end - width..end)];
            }
            return vec![];
        }
        let start_width = raw.bytes().take_while(|b| *b == b'#').count();
        let mut markers = Vec::new();
        if start_width == usize::from(level) {
            markers.push(
                self.map
                    .editor_range(self.range.start..self.range.start + start_width),
            );
        }
        if closed {
            let trimmed = raw.trim_end_matches([' ', '\t']);
            let end_width = trimmed.bytes().rev().take_while(|b| *b == b'#').count();
            let end = self.range.start + trimmed.len();
            if end_width > 0 && end - end_width >= self.range.start + start_width {
                markers.push(self.map.editor_range(end - end_width..end));
            }
        }
        markers
    }

    fn frontmatter_body(&self) -> Option<MarkdownRange> {
        let text = self.text();
        let opener_end = text.find(['\r', '\n'])?;
        let line_break_len = if text[opener_end..].starts_with("\r\n") {
            2
        } else {
            1
        };
        let closing_start = text.rfind(['\r', '\n'])? + 1;
        let body = self.range.start + opener_end + line_break_len..self.range.start + closing_start;
        (body.start <= body.end).then(|| self.map.editor_range(body))
    }

    fn text_extensions(&self, nodes: &mut Vec<MarkdownNode>) {
        for token in OBSIDIAN_TOKENS.find_iter(self.text()) {
            let start = self.range.start + token.start();
            let end = self.range.start + token.end();
            let before = self.map.text[..start].chars().next_back();
            let name = &token.as_str()[1..];
            let kind = if token.as_str().starts_with('#') {
                if before
                    .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '#' | '/' | '_' | '\\'))
                    || name.chars().all(|ch| ch.is_numeric() || ch == '/')
                {
                    continue;
                }
                MarkdownKind::Tag { name: name.into() }
            } else {
                let line_end = self.map.text[end..]
                    .find(['\r', '\n'])
                    .map_or(self.map.text.len(), |offset| end + offset);
                if before.is_some_and(|ch| !ch.is_whitespace())
                    || !self.map.text[end..line_end].trim().is_empty()
                {
                    continue;
                }
                MarkdownKind::BlockId { id: name.into() }
            };
            nodes.push(MarkdownNode {
                range: self.map.editor_range(start..end),
                kind,
                markers: vec![],
            });
        }
    }
}
