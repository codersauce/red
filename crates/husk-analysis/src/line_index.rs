//! UTF-8 byte offset and LSP UTF-16 position conversion.

/// A zero-based line and UTF-16 code-unit position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A half-open source range expressed in LSP positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionRange {
    pub start: Position,
    pub end: Position,
}

/// Line starts for one immutable source revision.
#[derive(Debug, Clone)]
pub struct LineIndex {
    line_starts: Box<[usize]>,
    text_len: usize,
}

impl LineIndex {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut line_starts = Vec::with_capacity(text.lines().count().saturating_add(1));
        line_starts.push(0);
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self {
            line_starts: line_starts.into_boxed_slice(),
            text_len: text.len(),
        }
    }

    #[must_use]
    pub fn position(&self, text: &str, byte: usize) -> Position {
        let byte = byte.min(self.text_len);
        let line = self.line_starts.partition_point(|start| *start <= byte) - 1;
        let line_start = self.line_starts[line];
        let character = text[line_start..byte]
            .encode_utf16()
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        Position {
            line: line.try_into().unwrap_or(u32::MAX),
            character,
        }
    }

    #[must_use]
    pub fn byte_offset(&self, text: &str, position: Position) -> Option<usize> {
        let line = usize::try_from(position.line).ok()?;
        let line_start = *self.line_starts.get(line)?;
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text_len);
        let mut content_end = line_end;
        if text.as_bytes().get(content_end.saturating_sub(1)) == Some(&b'\n') {
            content_end -= 1;
        }
        if text.as_bytes().get(content_end.saturating_sub(1)) == Some(&b'\r') {
            content_end -= 1;
        }
        let line_text = &text[line_start..content_end];
        let target = usize::try_from(position.character).ok()?;
        let mut utf16_offset = 0;
        for (byte, character) in line_text.char_indices() {
            if utf16_offset == target {
                return Some(line_start + byte);
            }
            utf16_offset += character.len_utf16();
            if utf16_offset > target {
                return None;
            }
        }
        (utf16_offset == target).then_some(content_end)
    }

    #[must_use]
    pub fn range(&self, text: &str, range: &std::ops::Range<usize>) -> PositionRange {
        PositionRange {
            start: self.position(text, range.start),
            end: self.position(text, range.end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_utf16_positions_without_splitting_scalars() {
        let text = "a😀b\r\ncafé\n";
        let index = LineIndex::new(text);

        assert_eq!(
            index.position(text, "a😀".len()),
            Position {
                line: 0,
                character: 3,
            }
        );
        assert_eq!(
            index.byte_offset(
                text,
                Position {
                    line: 0,
                    character: 3,
                },
            ),
            Some("a😀".len())
        );
        assert_eq!(
            index.byte_offset(
                text,
                Position {
                    line: 0,
                    character: 2,
                },
            ),
            None
        );
        assert_eq!(
            index.byte_offset(
                text,
                Position {
                    line: 0,
                    character: 4,
                },
            ),
            Some("a😀b".len())
        );
        assert_eq!(
            index.byte_offset(
                text,
                Position {
                    line: 0,
                    character: 5,
                },
            ),
            None
        );
        assert_eq!(
            index.position(text, text.find("café").expect("fixture contains café")),
            Position {
                line: 1,
                character: 0,
            }
        );
    }
}
