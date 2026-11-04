//! Comment edits are applied as one transaction across all directed selections.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentMode {
    Auto,
    Line,
    Block,
}

struct Block<'a> {
    range: Range,
    content: std::ops::Range<usize>,
    markers: Option<(&'a str, &'a str)>,
}

fn inspect<'a>(text: RopeSlice<'_>, ranges: &[Range], pairs: &'a [(&str, &str)]) -> Vec<Block<'a>> {
    ranges
        .iter()
        .map(|&range| {
            let slice = text.slice(range.start()..range.end());
            let leading = slice.chars().take_while(|c| c.is_whitespace()).count();
            let start = range.start() + leading;
            let end = range.end()
                - slice
                    .chars_at(slice.len_chars())
                    .reversed()
                    .take_while(|c| c.is_whitespace())
                    .count();
            let content = start..end.max(start);
            let markers = pairs
                .iter()
                .copied()
                .filter(|&(left, right)| {
                    let (a, b) = (left.chars().count(), right.chars().count());
                    content.len() >= a + b
                        && text.slice(start..start + a) == left
                        && text.slice(end - b..end) == right
                })
                .max_by_key(|&(left, right)| (left.len(), right.len()));
            Block {
                range,
                content,
                markers,
            }
        })
        .collect()
}

fn all_wrapped(blocks: &[Block<'_>]) -> bool {
    blocks.iter().any(|b| b.markers.is_some())
        && blocks
            .iter()
            .all(|b| b.content.is_empty() || b.markers.is_some())
}

impl Session {
    pub fn toggle_comments(
        &self,
        text: RopeSlice<'_>,
        selection: &Selections,
        mode: CommentMode,
        line: Option<&str>,
        pairs: &[(&str, &str)],
    ) -> Plan {
        let mut p = self.unchanged(selection);
        p.next.cancel_pending();
        p.next.mode = Mode::Normal;
        let mut lines = Vec::new();
        for &range in selection.ranges() {
            let (start, end) = range.lines(text);
            let next = lines
                .last()
                .map_or(start, |&last: &usize| start.max(last + 1));
            lines.extend(next..=end);
        }
        let use_line = pairs.is_empty() && line.is_some()
            || mode == CommentMode::Line && (line.is_some() || pairs.is_empty());
        if use_line {
            p.comment_lines(text, &lines, line.unwrap_or("#"));
            return p;
        }
        let block_only = line.is_none() && !pairs.is_empty();
        let fallback = [("/*", "*/")];
        let pairs = if pairs.is_empty() {
            &fallback[..]
        } else {
            pairs
        };
        if mode == CommentMode::Block {
            let blocks = inspect(text, selection.ranges(), pairs);
            p.comment_blocks(text, &blocks, pairs[0], true);
            return p;
        }
        // Helix splits each range with the rope's line iterator, including
        // its empty terminal range when the last selected line ends in LF.
        let mut by_line = Vec::new();
        for range in selection.ranges() {
            let (first, last) = range.lines(text);
            let mut at = text.line_to_char(first);
            let end = text.line_to_char(last) + text.line(last).len_chars();
            for line in text.slice(at..end).lines() {
                let next = at + line.len_chars();
                by_line.push(Range {
                    anchor: at,
                    head: next,
                });
                at = next;
            }
        }
        let split = Selections::new(text, by_line, 0).expect("a selection has at least one line");
        let line_blocks = inspect(text, split.ranges(), pairs);
        let selected = inspect(text, selection.ranges(), pairs);
        match mode {
            CommentMode::Block => unreachable!("handled before splitting into lines"),
            CommentMode::Line => {
                if !all_wrapped(&line_blocks) {
                    p.primary = 0;
                }
                p.comment_blocks(text, &line_blocks, pairs[0], true);
            }
            CommentMode::Auto if all_wrapped(&line_blocks) => {
                p.comment_blocks(text, &line_blocks, pairs[0], false)
            }
            CommentMode::Auto if all_wrapped(&selected) => {
                p.comment_blocks(text, &selected, pairs[0], false)
            }
            CommentMode::Auto if block_only => {
                p.comment_blocks(text, &line_blocks, pairs[0], false)
            }
            CommentMode::Auto => p.comment_lines(text, &lines, line.unwrap_or("#")),
        }
        p
    }
}

impl Plan {
    fn comment_lines(&mut self, text: RopeSlice<'_>, lines: &[usize], token: &str) {
        let width = token.chars().count();
        let mut column = usize::MAX;
        let mut all = true;
        let mut margin = 1;
        let mut nonblank = Vec::new();
        for &i in lines {
            let l = text.line(i);
            let Some(pos) = l.chars().position(|c| !c.is_whitespace()) else {
                continue;
            };
            column = column.min(pos);
            nonblank.push(i);
            if pos + width > l.len_chars() || l.slice(pos..pos + width) != token {
                all = false;
            }
            if l.get_char(pos + width) != Some(' ') {
                margin = 0;
            }
        }
        for i in nonblank {
            let start = text.line_to_char(i) + column;
            self.edits.push(if all {
                Edit {
                    start,
                    end: start + width + margin,
                    insert: String::new(),
                }
            } else {
                Edit {
                    start,
                    end: start,
                    insert: format!("{token} "),
                }
            });
        }
        self.map_edits();
        self.history = History::Atomic;
    }

    fn comment_blocks(
        &mut self,
        text: RopeSlice<'_>,
        blocks: &[Block<'_>],
        (left, right): (&str, &str),
        select_output: bool,
    ) {
        let remove = all_wrapped(blocks);
        let mut ranges = Vec::new();
        let mut offset = 0;
        for b in blocks {
            let start = b.content.start;
            let end = b.content.end;
            let mut selected = Range {
                anchor: b.range.start() + offset,
                head: b.range.end() + offset,
            };
            if !b.content.is_empty() {
                if let (true, Some((left, right))) = (remove, b.markers) {
                    let after = start + left.chars().count();
                    let before = end - right.chars().count();
                    let space_after =
                        usize::from(after < before && text.get_char(after) == Some(' '));
                    let space_before = usize::from(
                        before > after + space_after && text.get_char(before - 1) == Some(' '),
                    );
                    self.edits.push(Edit {
                        start,
                        end: after + space_after,
                        insert: String::new(),
                    });
                    self.edits.push(Edit {
                        start: before - space_before,
                        end,
                        insert: String::new(),
                    });
                } else if !remove && b.markers.is_none() {
                    self.edits.push(Edit {
                        start,
                        end: start,
                        insert: format!("{left} "),
                    });
                    self.edits.push(Edit {
                        start: end,
                        end,
                        insert: format!(" {right}"),
                    });
                    let added = left.chars().count() + right.chars().count() + 2;
                    selected = b
                        .range
                        .with_bounds(b.range.start() + offset, end + offset + added);
                    offset += added;
                }
            }
            ranges.push(selected);
        }
        if select_output && !remove {
            self.ranges = ranges;
        } else {
            self.map_edits();
        }
        self.history = History::Atomic;
    }
}
