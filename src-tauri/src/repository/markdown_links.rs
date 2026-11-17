use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownLinkNote {
    pub path: String,
    pub title: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub anchors: Vec<MarkdownLinkAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownLinkAnchor {
    pub kind: MarkdownLinkAnchorKind,
    pub label: String,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownLinkAnchorKind {
    Heading,
    Block,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownLinkSyntax {
    #[default]
    Wiki,
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownLinkQuery {
    pub source_path: String,
    pub target: String,
    pub anchor: Option<String>,
    #[serde(default)]
    pub syntax: MarkdownLinkSyntax,
}

impl MarkdownLinkQuery {
    /// Converts the raw destination of a CommonMark link or image to the shared query.
    /// Wiki queries already have unescaped target and fragment components in the Markdown AST.
    pub fn from_markdown(source_path: impl Into<String>, destination: &str) -> Option<Self> {
        let (target, anchor) = destination.split_once('#').unwrap_or((destination, ""));
        let target = percent_decode(target)?;
        let anchor = percent_decode(anchor)?;
        if target.contains(':') || target.starts_with("//") || target.starts_with("\\\\") {
            return None;
        }
        Some(Self {
            source_path: source_path.into(),
            target: target.into_owned(),
            anchor: (!anchor.is_empty()).then(|| anchor.into_owned()),
            syntax: MarkdownLinkSyntax::Markdown,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownLinkLocation {
    pub path: String,
    pub anchor: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MarkdownLinkResolution {
    Resolved {
        location: MarkdownLinkLocation,
    },
    Ambiguous {
        candidates: Vec<MarkdownLinkLocation>,
    },
    Missing,
    MissingAnchor {
        location: MarkdownLinkLocation,
    },
    Attachment {
        paths: Vec<String>,
    },
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownLinkSuggestion {
    pub path: String,
    pub target: String,
    pub label: String,
    pub anchor: Option<String>,
    pub line: Option<u32>,
}

/// Resolves against a single checkout's catalog snapshot. This module performs no filesystem IO;
/// attachment candidates still go through the checkout's canonical file service when opened.
pub struct MarkdownLinkResolver<'a> {
    by_path: BTreeMap<String, Vec<&'a MarkdownLinkNote>>,
    by_name: BTreeMap<String, Vec<&'a MarkdownLinkNote>>,
    notes: Vec<&'a MarkdownLinkNote>,
}

impl<'a> MarkdownLinkResolver<'a> {
    pub fn new(catalog: &'a [MarkdownLinkNote]) -> Self {
        let mut notes: Vec<_> = catalog
            .iter()
            .filter(|note| canonical_note(&note.path))
            .collect();
        notes.sort_by(|left, right| left.path.cmp(&right.path));
        notes.dedup_by(|left, right| left.path == right.path);
        let mut by_path = BTreeMap::<_, Vec<_>>::new();
        let mut by_name = BTreeMap::<_, Vec<_>>::new();
        for &note in &notes {
            by_path.entry(fold(&note.path)).or_default().push(note);
            let names: BTreeSet<_> = std::iter::once(note_stem(&note.path))
                .chain(note.title.as_deref())
                .chain(note.aliases.iter().map(String::as_str))
                .map(|name| fold(name.trim()))
                .filter(|name| !name.is_empty())
                .collect();
            for name in names {
                by_name.entry(name).or_default().push(note);
            }
        }
        Self {
            by_path,
            by_name,
            notes,
        }
    }

    pub fn resolve(&self, query: &MarkdownLinkQuery) -> MarkdownLinkResolution {
        use MarkdownLinkResolution::*;

        if !canonical_note(&query.source_path) {
            return Invalid;
        }
        let target = match query.syntax {
            MarkdownLinkSyntax::Wiki => query.target.trim(),
            MarkdownLinkSyntax::Markdown => &query.target,
        };
        let raw = target.replace('\\', "/");
        let anchor = query
            .anchor
            .as_deref()
            .map(str::trim)
            .filter(|fragment| !fragment.is_empty());
        let parent = parent(&query.source_path);
        let notes = if raw.is_empty() {
            if anchor.is_none() {
                return Missing;
            }
            self.by_path
                .get(&fold(&query.source_path))
                .cloned()
                .unwrap_or_default()
        } else if is_attachment(&raw) {
            let paths = attachment_paths(parent, &raw, query.syntax);
            return if paths.is_empty() {
                Invalid
            } else {
                Attachment { paths }
            };
        } else {
            let Some(notes) = self.candidates(parent, &raw, query.syntax) else {
                return Invalid;
            };
            notes
        };
        match notes.as_slice() {
            [] => Missing,
            [note] => {
                let location = locate(note, anchor);
                if anchor.is_some() && location.line.is_none() {
                    MissingAnchor { location }
                } else {
                    Resolved { location }
                }
            }
            _ => Ambiguous {
                candidates: notes.into_iter().map(|note| locate(note, anchor)).collect(),
            },
        }
    }

    pub fn resolve_batch(&self, queries: &[MarkdownLinkQuery]) -> Vec<MarkdownLinkResolution> {
        queries.iter().map(|query| self.resolve(query)).collect()
    }

    /// Suggests canonical checkout-rooted targets for the unfinished address inside a wikilink.
    pub fn suggest(
        &self,
        source_path: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<MarkdownLinkSuggestion> {
        if limit == 0 || !canonical_note(source_path) {
            return vec![];
        }
        let Some((raw, anchor)) = completion_address(prefix.trim()) else {
            return vec![];
        };
        if let Some(anchor) = anchor {
            return self.suggest_anchors(source_path, &raw, &anchor, limit);
        }
        let path_query = raw.contains('/');
        let path_prefixes = if path_query {
            scoped_paths(parent(source_path), &raw, MarkdownLinkSyntax::Wiki, true)
                .into_iter()
                .map(|prefix| fold(&prefix))
                .collect()
        } else {
            vec![]
        };
        let name_prefix = fold(&raw);
        self.notes
            .iter()
            .copied()
            .filter(|note| {
                self.unique_path(note)
                    && if path_query {
                        let path = fold(&note.path);
                        path_prefixes
                            .iter()
                            .any(|prefix: &String| path.starts_with(prefix))
                    } else {
                        std::iter::once(note_stem(&note.path))
                            .chain(note.title.as_deref())
                            .chain(note.aliases.iter().map(String::as_str))
                            .any(|name| fold(name.trim()).starts_with(&name_prefix))
                    }
            })
            .take(limit)
            .map(|note| suggestion(note, None))
            .collect()
    }

    fn candidates(
        &self,
        parent: &str,
        raw: &str,
        syntax: MarkdownLinkSyntax,
    ) -> Option<Vec<&'a MarkdownLinkNote>> {
        if syntax == MarkdownLinkSyntax::Wiki && !raw.contains('/') && !matches!(raw, "." | "..") {
            if raw.eq_ignore_ascii_case(".git")
                || raw.chars().any(char::is_control)
                || matches!(raw.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
            {
                return None;
            }
            return Some(
                self.by_name
                    .get(&fold(without_note_extension(raw)))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        let paths = scoped_paths(parent, raw, syntax, false);
        if paths.is_empty() {
            return None;
        }
        for path in paths {
            if syntax == MarkdownLinkSyntax::Markdown && !is_markdown(&path) {
                continue;
            }
            let notes = self.find_path(&path);
            if !notes.is_empty() {
                return Some(notes);
            }
        }
        Some(vec![])
    }

    fn find_path(&self, path: &str) -> Vec<&'a MarkdownLinkNote> {
        let key = fold(path);
        let mut found = self.by_path.get(&key).cloned().unwrap_or_default();
        if !is_markdown(path) {
            for extension in [".md", ".markdown"] {
                found.extend(
                    self.by_path
                        .get(&(key.clone() + extension))
                        .into_iter()
                        .flatten()
                        .copied(),
                );
            }
        }
        found.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        found
    }

    fn unique_path(&self, note: &MarkdownLinkNote) -> bool {
        self.by_path
            .get(&fold(&note.path))
            .is_some_and(|notes| notes.len() == 1)
    }

    fn suggest_anchors(
        &self,
        source_path: &str,
        target: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<MarkdownLinkSuggestion> {
        let raw = target.trim().replace('\\', "/");
        let notes = if raw.is_empty() {
            self.by_path
                .get(&fold(source_path))
                .cloned()
                .unwrap_or_default()
        } else if is_attachment(&raw) {
            vec![]
        } else {
            self.candidates(parent(source_path), &raw, MarkdownLinkSyntax::Wiki)
                .unwrap_or_default()
        };
        let (kind, prefix) = requested_anchor(prefix.trim());
        let key = anchor_key(kind, prefix);
        let mut suggestions = Vec::new();
        for note in notes.into_iter().filter(|note| self.unique_path(note)) {
            let mut anchors: Vec<_> = note
                .anchors
                .iter()
                .filter(|entry| entry.line > 0 && entry.kind == kind)
                .collect();
            anchors.sort_unstable_by(|left, right| {
                (left.line, &left.label).cmp(&(right.line, &right.label))
            });
            let mut seen = BTreeSet::new();
            for entry in anchors {
                let candidate = anchor_key(kind, &entry.label);
                if candidate.starts_with(&key) && seen.insert(candidate) {
                    suggestions.push(suggestion(note, Some(entry)));
                    if suggestions.len() == limit {
                        return suggestions;
                    }
                }
            }
        }
        suggestions
    }
}

fn locate(note: &MarkdownLinkNote, anchor: Option<&str>) -> MarkdownLinkLocation {
    let matched = anchor.and_then(|requested| {
        let (kind, label) = requested_anchor(requested);
        let key = anchor_key(kind, label);
        note.anchors
            .iter()
            .filter(|entry| {
                entry.line > 0 && entry.kind == kind && anchor_key(kind, &entry.label) == key
            })
            .min_by(|left, right| (left.line, &left.label).cmp(&(right.line, &right.label)))
    });
    MarkdownLinkLocation {
        path: note.path.clone(),
        anchor: matched
            .map(anchor_label)
            .or_else(|| anchor.map(str::to_owned)),
        line: matched.map(|entry| entry.line),
    }
}

fn suggestion(
    note: &MarkdownLinkNote,
    entry: Option<&MarkdownLinkAnchor>,
) -> MarkdownLinkSuggestion {
    let name = note
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| note_stem(&note.path));
    let anchor = entry.map(anchor_label);
    let mut target = String::from("/");
    append_wiki_address(&mut target, &note.path);
    if let Some(anchor) = &anchor {
        target.push('#');
        append_wiki_address(&mut target, anchor);
    }
    MarkdownLinkSuggestion {
        path: note.path.clone(),
        target,
        label: anchor
            .as_ref()
            .map_or_else(|| name.to_owned(), |anchor| format!("{name} · {anchor}")),
        anchor,
        line: entry.map(|entry| entry.line),
    }
}

fn append_wiki_address(output: &mut String, text: &str) {
    for ch in text.chars() {
        if matches!(ch, '\\' | '[' | ']' | '#' | '|') {
            output.push('\\');
        }
        output.push(ch);
    }
}

fn completion_address(prefix: &str) -> Option<(String, Option<String>)> {
    let mut target = String::new();
    let mut anchor = None;
    let mut input = prefix.chars().peekable();
    while let Some(ch) = input.next() {
        let ch = match ch {
            '\\' if matches!(input.peek(), Some('\\' | '[' | ']' | '#' | '|')) => input.next()?,
            '|' => return None,
            '#' if anchor.is_none() => {
                anchor = Some(String::new());
                continue;
            }
            _ => ch,
        };
        anchor.as_mut().unwrap_or(&mut target).push(ch);
    }
    Some((target.replace('\\', "/"), anchor))
}

fn requested_anchor(value: &str) -> (MarkdownLinkAnchorKind, &str) {
    match value.strip_prefix('^') {
        Some(label) => (MarkdownLinkAnchorKind::Block, label),
        None => (MarkdownLinkAnchorKind::Heading, value),
    }
}

fn anchor_label(entry: &MarkdownLinkAnchor) -> String {
    match entry.kind {
        MarkdownLinkAnchorKind::Heading => entry.label.clone(),
        MarkdownLinkAnchorKind::Block => format!("^{}", entry.label.trim_start_matches('^')),
    }
}

pub(super) fn anchor_key(kind: MarkdownLinkAnchorKind, label: &str) -> String {
    if kind == MarkdownLinkAnchorKind::Block {
        return label.trim_start_matches('^').to_owned();
    }
    let normalized = label.nfkc().collect::<String>().to_lowercase();
    let mut key = String::new();
    for ch in normalized
        .trim()
        .chars()
        .filter(|ch| !matches!(ch, '*' | '`' | '~'))
    {
        if ch.is_whitespace() || ch == '-' {
            if !key.ends_with('-') {
                key.push('-');
            }
        } else {
            key.push(ch);
        }
    }
    key
}

fn note_stem(path: &str) -> &str {
    without_note_extension(path.rsplit('/').next().unwrap_or(path))
}

fn without_note_extension(path: &str) -> &str {
    if let Some((stem, ext)) = path.rsplit_once('.')
        && (ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
    {
        stem
    } else {
        path
    }
}

fn is_markdown(path: &str) -> bool {
    without_note_extension(path).len() != path.len()
}

pub(super) fn fold(value: &str) -> String {
    value.nfc().collect::<String>().to_lowercase()
}

fn canonical_note(path: &str) -> bool {
    is_markdown(path)
        && !path.starts_with('/')
        && normalize_path("", path, false).as_deref() == Some(path)
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn note_relative(raw: &str) -> bool {
    raw.starts_with("./") || raw.starts_with("../") || matches!(raw, "." | "..")
}

fn scoped_paths(parent: &str, raw: &str, syntax: MarkdownLinkSyntax, prefix: bool) -> Vec<String> {
    let bases: &[&str] = if raw.starts_with('/') {
        &[""]
    } else if syntax == MarkdownLinkSyntax::Markdown || note_relative(raw) {
        &[parent]
    } else {
        &["", parent]
    };
    unique_paths(bases, raw, prefix)
}

fn attachment_paths(parent: &str, raw: &str, syntax: MarkdownLinkSyntax) -> Vec<String> {
    let bases: &[&str] = if raw.starts_with('/') {
        &[""]
    } else if syntax == MarkdownLinkSyntax::Markdown || note_relative(raw) {
        &[parent]
    } else {
        &[parent, ""]
    };
    unique_paths(bases, raw, false)
}

fn unique_paths(bases: &[&str], raw: &str, prefix: bool) -> Vec<String> {
    let mut seen = BTreeSet::new();
    bases
        .iter()
        .filter_map(|base| normalize_path(base, raw, prefix))
        .filter(|path| seen.insert(fold(path)))
        .collect()
}

fn normalize_path(base: &str, raw: &str, prefix: bool) -> Option<String> {
    if raw.starts_with("//")
        || raw.contains(['\\', ':', '"', '<', '>', '|', '?', '*'])
        || raw.chars().any(char::is_control)
    {
        return None;
    }
    let mut components: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    let trailing_directory = raw.ends_with('/');
    let mut parts = raw.split('/').filter(|part| !part.is_empty()).peekable();
    while let Some(part) = parts.next() {
        match part {
            "." => {}
            ".." => {
                components.pop()?;
            }
            _ => {
                let unfinished = prefix && !trailing_directory && parts.peek().is_none();
                if part.eq_ignore_ascii_case(".git") || (!unfinished && part.ends_with(['.', ' ']))
                {
                    return None;
                }
                components.push(part);
            }
        }
    }
    if !prefix && components.is_empty() {
        return None;
    }
    let mut path = components.join("/");
    if prefix && trailing_directory && !path.is_empty() {
        path.push('/');
    }
    Some(path)
}

fn is_attachment(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "avif"
                | "bmp"
                | "gif"
                | "jpeg"
                | "jpg"
                | "png"
                | "svg"
                | "webp"
                | "aac"
                | "flac"
                | "m4a"
                | "mp3"
                | "ogg"
                | "opus"
                | "wav"
                | "m4v"
                | "mov"
                | "mp4"
                | "ogv"
                | "webm"
                | "pdf"
        )
    })
}

fn percent_decode(source: &str) -> Option<Cow<'_, str>> {
    if !source.contains('%') {
        return Some(Cow::Borrowed(source));
    }
    let mut bytes = Vec::with_capacity(source.len());
    let mut input = source.bytes();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(bytes).ok().map(Cow::Owned)
}
