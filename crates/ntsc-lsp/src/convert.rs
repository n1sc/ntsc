//! Byte-offset to LSP position mapping.
//!
//! LSP positions are 0-based line/UTF-16-column pairs while the compiler
//! works with byte offsets, so every conversion goes through [`LineIndex`],
//! built once per buffer revision.

/// Line start table for one buffer revision.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset of the first byte of each line (line `i` starts at
    /// `starts[i]`, 0-based lines).
    starts: Vec<usize>,
    text_len: usize,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                starts.push(i + 1);
            }
        }
        Self {
            starts,
            text_len: text.len(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// 0-based line number containing `offset` (clamped to the last line).
    pub fn line_for_offset(&self, offset: usize) -> usize {
        let offset = offset.min(self.text_len);
        match self.starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        }
    }

    /// Convert a byte `offset` into an LSP position, counting the column
    /// in UTF-16 code units as the protocol requires.
    pub fn position(&self, text: &str, offset: usize) -> lsp_types::Position {
        let offset = offset.min(self.text_len);
        let line = self.line_for_offset(offset);
        let line_start = self.starts[line];
        let column = utf16_len(&text[line_start..offset.min(text.len())]);
        lsp_types::Position::new(line as u32, column as u32)
    }

    /// Convert an LSP position back into a byte offset (`None` for an
    /// out-of-range line).
    pub fn offset(&self, text: &str, position: lsp_types::Position) -> Option<usize> {
        let line = position.line as usize;
        let line_start = *self.starts.get(line)?;
        let line_end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text_len)
            .min(text.len());
        let line_text = &text[line_start..line_end];
        Some(line_start + prefix_for_utf16(line_text, position.character as usize))
    }

    /// Convert a byte-offset span into an LSP range.
    pub fn range(&self, text: &str, start: usize, end: usize) -> lsp_types::Range {
        let start = start.min(self.text_len);
        let end = end.min(self.text_len);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        lsp_types::Range::new(self.position(text, start), self.position(text, end))
    }
}

/// UTF-16 length of `text` (LSP columns count UTF-16 code units).
fn utf16_len(text: &str) -> usize {
    text.chars().map(|ch| ch.len_utf16()).sum()
}

/// Byte length of the prefix of `line` holding `units` UTF-16 code units.
/// A column past the end of the line clamps to the line length.
fn prefix_for_utf16(line: &str, units: usize) -> usize {
    let mut remaining = units;
    for (i, ch) in line.char_indices() {
        let width = ch.len_utf16();
        if remaining < width {
            return i;
        }
        remaining -= width;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_ascii() {
        let text = "fun main() {\n    say(\"hi\")\n}\n";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 4);
        let pos = index.position(text, 17);
        assert_eq!(pos, lsp_types::Position::new(1, 4));
        assert_eq!(index.offset(text, pos), Some(17));
    }

    #[test]
    fn counts_multibyte_columns_as_utf16() {
        let text = "say(\"é\")\n";
        let index = LineIndex::new(text);
        // `é` is 2 bytes but 1 UTF-16 unit: byte 7 (after `é`) is column 6.
        let pos = index.position(text, 7);
        assert_eq!(pos.character, 6);
        assert_eq!(index.offset(text, pos), Some(7));
    }

    #[test]
    fn clamps_out_of_range_offsets() {
        let text = "hi\n";
        let index = LineIndex::new(text);
        let range = index.range(text, 0, 10_000);
        assert_eq!(range.end, lsp_types::Position::new(1, 0));
    }
}
