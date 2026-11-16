use std::{borrow::Cow, ops::Range};

use comrak::nodes::Sourcepos;

use super::MarkdownRange;

pub(super) struct SourceMap<'a> {
    pub text: &'a str,
    lines: Vec<Range<usize>>,
    // Between these boundaries, each UTF-8 byte contributes one UTF-16 code unit.
    utf16_adjustments: Vec<(usize, usize)>,
}

impl<'a> SourceMap<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut lines = Vec::new();
        let mut line_start = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let mut utf16_adjustments = Vec::new();
        let mut units = 0;
        let mut chars = text.char_indices().peekable();
        while let Some((byte, ch)) = chars.next() {
            let mut width = ch.len_utf8();
            if ch == '\r' && chars.next_if(|&(_, next)| next == '\n').is_some() {
                width += 1;
            }
            units += ch.len_utf16();
            if width != ch.len_utf16() {
                utf16_adjustments.push((byte + width, units));
            }
            if matches!(ch, '\n' | '\r') {
                lines.push(line_start..byte);
                line_start = byte + width;
            }
        }
        lines.push(line_start..text.len());
        Self {
            text,
            lines,
            utf16_adjustments,
        }
    }

    pub fn ast_range(&self, position: Sourcepos) -> Option<Range<usize>> {
        self.ast_range_in_cell(position, None)
    }

    pub fn ast_range_in_cell(
        &self,
        position: Sourcepos,
        cell: Option<&TableCellMap>,
    ) -> Option<Range<usize>> {
        let first = self.lines.get(position.start.line.checked_sub(1)?)?;
        let last = self.lines.get(position.end.line.checked_sub(1)?)?;
        let mut start = first
            .start
            .checked_add(position.start.column.checked_sub(1)?)?;
        let mut end = last.start.checked_add(position.end.column)?;
        if let Some(cell) = cell {
            start = cell.original_boundary(start);
            end = cell.original_boundary(end);
        }
        if start <= first.end
            && end <= last.end
            && start <= end
            && self.text.get(start..end).is_some()
        {
            Some(start..end)
        } else {
            None
        }
    }

    pub fn editor_range(&self, range: Range<usize>) -> MarkdownRange {
        MarkdownRange {
            from: self.utf16_at(range.start),
            to: self.utf16_at(range.end),
            byte_start: range.start,
            byte_end: range.end,
        }
    }

    fn utf16_at(&self, byte: usize) -> usize {
        let previous = self
            .utf16_adjustments
            .partition_point(|(end, _)| *end <= byte);
        let Some(index) = previous.checked_sub(1) else {
            return byte;
        };
        let (byte_end, utf16_end) = self.utf16_adjustments[index];
        utf16_end + byte - byte_end
    }
}

/// Comrak strips the backslash from escaped table pipes before the inline parser assigns columns.
/// The table cell's own range is still in the original source.
pub(super) struct TableCellMap {
    pipes: Vec<(usize, usize)>,
}

impl TableCellMap {
    pub fn new(text: &str, range: Range<usize>) -> Option<Self> {
        let mut pipes = Vec::new();
        let mut skip_before = 0;
        let cell = &text[range.clone()];
        for (offset, _) in cell.match_indices('\\') {
            if offset < skip_before {
                continue;
            }
            if cell.as_bytes().get(offset + 1) == Some(&b'|') {
                let original = range.start + offset;
                pipes.push((original - pipes.len(), original));
            }
            skip_before = offset + 2;
        }
        (!pipes.is_empty()).then_some(Self { pipes })
    }

    fn original_boundary(&self, offset: usize) -> usize {
        offset
            + self
                .pipes
                .partition_point(|&(compressed, _)| compressed < offset)
    }

    pub fn unescaped<'s>(&self, text: &'s str, range: Range<usize>) -> Cow<'s, str> {
        let first = self
            .pipes
            .partition_point(|&(_, original)| original < range.start);
        let last = self
            .pipes
            .partition_point(|&(_, original)| original < range.end);
        if first == last {
            return Cow::Borrowed(&text[range]);
        }
        let mut start = range.start;
        let mut expanded = String::with_capacity(range.len() - (last - first));
        for &(_, original) in &self.pipes[first..last] {
            expanded.push_str(&text[start..original]);
            start = original + 1;
        }
        expanded.push_str(&text[start..range.end]);
        Cow::Owned(expanded)
    }
}
