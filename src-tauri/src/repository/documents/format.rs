use similar::{Algorithm, DiffOp, capture_diff_slices};

use super::LineEndings;

const BOM: &str = "\u{feff}";

pub(super) struct EditorFormat {
    pub bom: bool,
    pub endings: LineEndings,
    pub markdown: String,
}

impl EditorFormat {
    pub fn from_raw(raw: &str) -> Self {
        let (bom, text) = strip_bom(raw);
        let lines = lines(text);
        let mut styles = lines.iter().filter_map(|line| match line.separator {
            "\n" => Some(LineEndings::Lf),
            "\r\n" => Some(LineEndings::Crlf),
            "\r" => Some(LineEndings::Cr),
            _ => None,
        });
        let first = styles.next().unwrap_or(LineEndings::None);
        let endings = if styles.all(|style| style == first) {
            first
        } else {
            LineEndings::Mixed
        };
        let markdown = lines
            .iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            bom,
            endings,
            markdown,
        }
    }
}

pub(super) fn encode(original: &str, markdown: &str) -> Vec<u8> {
    let (bom, text) = strip_bom(original);
    let original_lines = lines(text);
    let old_text = original_lines
        .iter()
        .map(|line| line.text)
        .collect::<Vec<_>>();
    let new_text = markdown.split('\n').collect::<Vec<_>>();
    let default = original_lines
        .first()
        .map_or("\n", |line| line.separator_or("\n"));
    let mut separators = vec![default; new_text.len()];

    // Keep each unchanged line's terminator, including files that mix CRLF, LF, and CR.
    // Replacement lines inherit the corresponding old terminator when there is one.
    for operation in capture_diff_slices(Algorithm::Patience, &old_text, &new_text) {
        let (old_start, new_start, count) = match operation {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => (old_index, new_index, len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (old_index, new_index, old_len.min(new_len)),
            _ => continue,
        };
        for offset in 0..count {
            separators[new_start + offset] =
                original_lines[old_start + offset].separator_or(default);
        }
    }

    let mut encoded = String::with_capacity(markdown.len());
    if bom {
        encoded.push_str(BOM);
    }
    for (index, text) in new_text.iter().enumerate() {
        encoded.push_str(text);
        if index + 1 < new_text.len() {
            encoded.push_str(separators[index]);
        }
    }
    encoded.into_bytes()
}

fn strip_bom(raw: &str) -> (bool, &str) {
    match raw.strip_prefix(BOM) {
        Some(text) => (true, text),
        None => (false, raw),
    }
}

struct Line<'a> {
    text: &'a str,
    separator: &'a str,
}

impl Line<'_> {
    fn separator_or<'a>(&'a self, default: &'a str) -> &'a str {
        if self.separator.is_empty() {
            default
        } else {
            self.separator
        }
    }
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    while let Some(offset) = bytes[start..]
        .iter()
        .position(|byte| *byte == b'\r' || *byte == b'\n')
    {
        let end = start + offset;
        let separator_len = if bytes[end..].starts_with(b"\r\n") {
            2
        } else {
            1
        };
        lines.push(Line {
            text: &text[start..end],
            separator: &text[end..end + separator_len],
        });
        start = end + separator_len;
    }
    lines.push(Line {
        text: &text[start..],
        separator: "",
    });
    lines
}
