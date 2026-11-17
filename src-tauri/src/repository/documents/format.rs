use anyhow::{Result, ensure};
use similar::{Algorithm, DiffOp, capture_diff_slices};

use super::{DocumentKind, LineEndings, SourceFormatChange, TextEncoding};

const BOM: &str = "\u{feff}";
const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];
const UTF16_LE_BOM: &[u8] = &[0xff, 0xfe];
const UTF16_BE_BOM: &[u8] = &[0xfe, 0xff];

pub(super) struct EditorFormat {
    pub encoding: TextEncoding,
    pub endings: LineEndings,
    pub markdown: String,
}

pub(in crate::repository) struct RawDocument {
    pub encoding: TextEncoding,
    text: String,
}

impl RawDocument {
    pub fn from_bytes(kind: DocumentKind, bytes: &[u8]) -> Result<Self> {
        let (encoding, payload) = if let Some(payload) = bytes.strip_prefix(UTF8_BOM) {
            (TextEncoding::Utf8Bom, payload)
        } else if let Some(payload) = bytes.strip_prefix(UTF16_LE_BOM) {
            (TextEncoding::Utf16Le, payload)
        } else if let Some(payload) = bytes.strip_prefix(UTF16_BE_BOM) {
            (TextEncoding::Utf16Be, payload)
        } else {
            (TextEncoding::Utf8, bytes)
        };
        let text = match encoding {
            TextEncoding::Utf8 | TextEncoding::Utf8Bom => std::str::from_utf8(payload)?.to_owned(),
            TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                ensure!(
                    matches!(kind, DocumentKind::Source),
                    "Markdown must be UTF-8"
                );
                let (pairs, rest) = payload.as_chunks::<2>();
                ensure!(rest.is_empty(), "invalid UTF-16 byte length");
                let units = pairs
                    .iter()
                    .copied()
                    .map(if encoding == TextEncoding::Utf16Le {
                        u16::from_le_bytes
                    } else {
                        u16::from_be_bytes
                    });
                char::decode_utf16(units).collect::<Result<String, _>>()?
            }
        };
        ensure!(!text.contains('\0'), "source documents must be text");
        Ok(Self { encoding, text })
    }

    pub(super) fn into_text(self) -> String {
        self.text
    }

    pub fn raw_text(self) -> String {
        if self.encoding != TextEncoding::Utf8 {
            format!("{BOM}{}", self.text)
        } else {
            self.text
        }
    }

    pub(super) fn format(&self) -> EditorFormat {
        EditorFormat::from_text(self.encoding, &self.text)
    }

    pub fn encode_edit(&self, markdown: &str) -> Vec<u8> {
        encode_text(self.encoding, &preserve_endings(&self.text, markdown))
    }

    pub fn convert(&self, change: SourceFormatChange) -> Result<Vec<u8>> {
        ensure!(
            change.encoding.is_some() || change.line_endings.is_some(),
            "choose a target file format"
        );
        let separator = match change.line_endings {
            None => None,
            Some(LineEndings::Lf) => Some("\n"),
            Some(LineEndings::Crlf) => Some("\r\n"),
            Some(LineEndings::Cr) => Some("\r"),
            Some(_) => anyhow::bail!("select LF, CRLF, or CR line endings"),
        };
        let text = separator.map(|separator| {
            lines(&self.text)
                .iter()
                .map(|line| line.text)
                .collect::<Vec<_>>()
                .join(separator)
        });
        let encoding = change.encoding.unwrap_or(self.encoding);
        let text = text.as_deref().unwrap_or(&self.text);
        ensure!(
            encoding != TextEncoding::Utf8 || !text.starts_with(BOM),
            "UTF-8 without BOM cannot preserve a leading U+FEFF character"
        );
        Ok(encode_text(encoding, text))
    }
}

impl EditorFormat {
    pub fn from_raw(raw: &str) -> Self {
        let (encoding, text) = utf8_text(raw);
        Self::from_text(encoding, text)
    }

    fn from_text(encoding: TextEncoding, text: &str) -> Self {
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
            encoding,
            endings,
            markdown,
        }
    }
}

fn utf8_text(raw: &str) -> (TextEncoding, &str) {
    raw.strip_prefix(BOM)
        .map_or((TextEncoding::Utf8, raw), |text| {
            (TextEncoding::Utf8Bom, text)
        })
}

pub(super) fn encode(original: &str, markdown: &str) -> Vec<u8> {
    let (encoding, text) = utf8_text(original);
    encode_text(encoding, &preserve_endings(text, markdown))
}

fn encode_text(encoding: TextEncoding, text: &str) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => text.as_bytes().to_vec(),
        TextEncoding::Utf8Bom => [UTF8_BOM, text.as_bytes()].concat(),
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let (bom, unit) = if encoding == TextEncoding::Utf16Le {
                (UTF16_LE_BOM, u16::to_le_bytes as fn(u16) -> [u8; 2])
            } else {
                (UTF16_BE_BOM, u16::to_be_bytes as fn(u16) -> [u8; 2])
            };
            let mut bytes = bom.to_vec();
            bytes.extend(text.encode_utf16().flat_map(unit));
            bytes
        }
    }
}

fn preserve_endings(original: &str, markdown: &str) -> String {
    let original_lines = lines(original);
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
    for (index, text) in new_text.iter().enumerate() {
        encoded.push_str(text);
        if index + 1 < new_text.len() {
            encoded.push_str(separators[index]);
        }
    }
    encoded
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
