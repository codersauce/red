//! Helix document selection and edit planning for a host-owned Rope.

use ropey::RopeSlice;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// A directed half-open range in Unicode scalar offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
}

impl Range {
    pub fn start(self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn cursor(self, text: RopeSlice<'_>) -> usize {
        if self.anchor < self.head {
            boundary(text, self.head, false)
        } else {
            self.head
        }
    }

    pub fn at(text: RopeSlice<'_>, pos: usize) -> Self {
        Self {
            anchor: pos,
            head: boundary(text, pos, true),
        }
    }

    pub(crate) fn lines(self, text: RopeSlice<'_>) -> (usize, usize) {
        (
            text.char_to_line(self.start()),
            text.char_to_line(if self.start() < self.end() {
                self.end() - 1
            } else {
                self.end()
            }),
        )
    }

    fn with_bounds(self, start: usize, end: usize) -> Self {
        if self.head < self.anchor {
            Self {
                anchor: end,
                head: start,
            }
        } else {
            Self {
                anchor: start,
                head: end,
            }
        }
    }

    fn aligned(self, text: RopeSlice<'_>) -> Self {
        let start = self.start().min(text.len_chars());
        let end = self.end().min(text.len_chars());
        let lower = if start == text.len_chars() {
            start
        } else {
            boundary(text, boundary(text, start, true), false)
        };
        let upper = if start == end {
            boundary(text, lower, true)
        } else {
            boundary(text, boundary(text, end, false), true)
        };
        if self.head < self.anchor {
            Self {
                anchor: upper,
                head: lower,
            }
        } else {
            Self {
                anchor: lower,
                head: upper,
            }
        }
    }
}

/// Traverse extended grapheme clusters without flattening a Rope or splitting CRLF.
pub fn boundary(text: RopeSlice<'_>, at: usize, forward: bool) -> usize {
    let byte = text.char_to_byte(at);
    let mut cursor = GraphemeCursor::new(byte, text.len_bytes(), true);
    let mut index = if forward {
        byte
    } else {
        byte.saturating_sub(1)
    };
    loop {
        let (chunk, offset, _, _) = text.chunk_at_byte(index);
        let result = if forward {
            cursor.next_boundary(chunk, offset)
        } else {
            cursor.prev_boundary(chunk, offset)
        };
        match result {
            Ok(Some(next)) => return text.byte_to_char(next),
            Ok(None) => return if forward { text.len_chars() } else { 0 },
            Err(GraphemeIncomplete::NextChunk) => index = offset + chunk.len(),
            Err(GraphemeIncomplete::PrevChunk) => index = offset - 1,
            Err(GraphemeIncomplete::PreContext(end)) => {
                let (before, start, _, _) = text.chunk_at_byte(end - 1);
                cursor.provide_context(&before[..end - start], start);
            }
            Err(GraphemeIncomplete::InvalidOffset) => unreachable!("rope chunk must cover cursor"),
        }
    }
}

/// Ordered, nonoverlapping selections and their primary index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selections {
    ranges: Vec<Range>,
    primary: usize,
}

impl Selections {
    pub fn single(text: RopeSlice<'_>, range: Range) -> Self {
        Self {
            ranges: vec![range.aligned(text)],
            primary: 0,
        }
    }

    pub fn new(text: RopeSlice<'_>, ranges: Vec<Range>, primary: usize) -> Option<Self> {
        if primary >= ranges.len() {
            return None;
        }
        let mut indexed: Vec<_> = ranges
            .into_iter()
            .enumerate()
            .map(|(i, r)| (r.aligned(text), i == primary))
            .collect();
        indexed.sort_by_key(|(r, _)| r.start());
        let mut merged: Vec<Range> = Vec::with_capacity(indexed.len());
        let mut main = 0;
        for (range, is_primary) in indexed {
            if let Some(last) = merged
                .last_mut()
                .filter(|last| last.start() == range.start() || range.start() < last.end())
            {
                let lo = last.start();
                let hi = last.end().max(range.end());
                *last = if last.head < last.anchor && range.head < range.anchor {
                    Range {
                        anchor: hi,
                        head: lo,
                    }
                } else {
                    Range {
                        anchor: lo,
                        head: hi,
                    }
                };
            } else {
                merged.push(range);
            }
            if is_primary {
                main = merged.len() - 1;
            }
        }
        Some(Self {
            ranges: merged,
            primary: main,
        })
    }

    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }
    pub fn primary_index(&self) -> usize {
        self.primary
    }
    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    /// Apply a document pointer event using the same directed block cursor as keyboard movement.
    pub fn pointer(
        &self,
        text: RopeSlice<'_>,
        pos: usize,
        extend: bool,
        add: bool,
        drag: bool,
    ) -> Self {
        let range = self.primary().move_to(text, pos, extend || drag);
        if !add && !drag {
            return Self::single(text, range);
        }
        let mut ranges = self.ranges.clone();
        let primary = if add {
            ranges.push(Range::at(text, pos));
            ranges.len() - 1
        } else {
            ranges[self.primary] = range;
            self.primary
        };
        Self::new(text, ranges, primary).expect("pointer retains a selection")
    }

    pub fn transform(&self, text: RopeSlice<'_>, f: impl Fn(Range) -> Range) -> Self {
        Self::new(
            text,
            self.ranges.iter().copied().map(f).collect(),
            self.primary,
        )
        .expect("nonempty selection")
    }
}

mod command;
mod jumps;
mod motion;
pub use jumps::Jumps;
mod repeat;
mod search;
pub use command::{Context, Edit, History, HostAction, Mode, Paste, Plan, Session};
pub use repeat::{LastInsert, ReplayKey};

impl Selections {
    /// Carry endpoints across a host replacement expressed in original offsets.
    pub fn rebase(
        &self,
        text: RopeSlice<'_>,
        edits: impl IntoIterator<Item = (usize, usize, usize)>,
    ) -> Self {
        let mut ranges = self.ranges.clone();
        for (start, end, inserted) in edits {
            let map = |pos, after| {
                if pos < start {
                    return pos;
                }
                if pos >= end && (pos != start || after) {
                    return pos - (end - start) + inserted;
                }
                if pos == start || !after {
                    return start;
                }
                if inserted == end - start {
                    pos
                } else {
                    start + inserted
                }
            };
            for r in &mut ranges {
                *r = Range {
                    anchor: map(r.anchor, r.anchor <= r.head),
                    head: map(r.head, r.head <= r.anchor),
                };
            }
        }
        Self::new(text, ranges, self.primary).expect("rebasing retains a selection")
    }
}

/// Infer the document indentation unit from increases between nonblank lines.
/// The host caches the result at open, before editing changes the sample.
pub fn detect_indent(text: RopeSlice<'_>) -> Option<String> {
    let mut counts = [0usize; 17];
    let mut previous = (' ', 0);
    for line in text.lines().take(1000) {
        let Some(first) = line.get_char(0) else {
            continue;
        };
        if first == '\n' || first == '\r' {
            continue;
        }
        if first != ' ' && first != '\t' {
            previous = (' ', 0);
            continue;
        }
        let leading = line.chars().take(257).take_while(|&c| c == first).count();
        if leading > 256 || line.chars().all(char::is_whitespace) {
            continue;
        }
        if (previous.0 == first || previous.1 == 0) && leading > previous.1 {
            if first == '\t' {
                counts[0] += 2;
            } else if let Some(freq) = counts.get_mut(leading - previous.1) {
                *freq += 1;
            }
        }
        previous = (first, leading);
    }
    let (unit, &best) = counts.iter().enumerate().max_by_key(|(_, n)| **n)?;
    counts[unit] = 0;
    let runner_up = counts.into_iter().max()?;
    (best > 0 && runner_up * 100 < best * 66).then(|| {
        if unit == 0 {
            "\t".into()
        } else {
            " ".repeat(unit)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;
    #[test]
    fn graphemes_across_rope_chunks_and_line_endings() {
        use unicode_segmentation::UnicodeSegmentation;
        let source = "🇧🇷e\u{301}👩‍💻\r\n".repeat(600);
        let rope = Rope::from_str(&source);
        let mut i = 0;
        for cluster in source.graphemes(true) {
            let next = i + cluster.chars().count();
            assert_eq!(boundary(rope.slice(..), i, true), next);
            assert_eq!(boundary(rope.slice(..), next, false), i);
            i = next;
        }
    }

    #[test]
    fn normalize_retains_primary_when_ranges_merge_but_touching_ranges_survive() {
        let rope = Rope::from_str("abc def");
        let s = Selections::new(
            rope.slice(..),
            vec![
                Range { anchor: 7, head: 5 },
                Range { anchor: 3, head: 0 },
                Range { anchor: 5, head: 3 },
                Range { anchor: 7, head: 6 },
            ],
            3,
        )
        .unwrap();
        assert_eq!(s.ranges().len(), 3);
        assert_eq!(s.primary(), Range { anchor: 7, head: 5 });
    }
}
