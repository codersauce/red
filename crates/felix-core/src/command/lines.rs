use super::*;
use crate::motion::{logical_end, logical_start};
use std::collections::{BTreeMap, BTreeSet};

impl Plan {
    pub(super) fn edit_lines(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        key: &str,
        count: usize,
        ctx: &Context<'_>,
    ) {
        if matches!(key, "J" | "<a-J>") {
            self.join_lines(text, selections, key == "<a-J>", ctx);
            return;
        }
        let mut lines = BTreeSet::new();
        for range in selections.ranges() {
            let (first, last) = range.lines(text);
            lines.extend(first..=last);
        }
        for line in lines {
            let start = text.line_to_char(line);
            let edit = match key {
                ">" if text.line(line).chars().any(|c| !c.is_whitespace()) => Edit {
                    start,
                    end: start,
                    insert: ctx.indent.repeat(count),
                },
                "<" => {
                    let mut col = 0;
                    let mut end = start;
                    let target = count.saturating_mul(
                        ctx.indent
                            .chars()
                            .map(|c| if c == '\t' { ctx.tab_width } else { 1 })
                            .sum(),
                    );
                    for ch in text.line(line).chars() {
                        if !matches!(ch, ' ' | '\t') || col >= target {
                            break;
                        }
                        col += if ch == '\t' {
                            ctx.tab_width.max(1) - col % ctx.tab_width.max(1)
                        } else {
                            1
                        };
                        end += 1;
                    }
                    Edit {
                        start,
                        end,
                        insert: String::new(),
                    }
                }
                _ => continue,
            };
            self.edits.push(edit);
        }
        if self.edits.is_empty() {
            return;
        }
        self.map_edits();
        self.history = History::Atomic;
        self.next.mode = Mode::Normal;
    }

    fn join_lines(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        select_spaces: bool,
        ctx: &Context<'_>,
    ) {
        let skip_indent = |start| {
            start
                + text
                    .chars_at(start)
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .count()
        };
        let mut joins = BTreeMap::new();
        for range in selections.ranges() {
            let mut line = logical_start(text, range.start());
            let last = logical_start(text, boundary(text, range.end(), false).max(range.start()));
            let mut token = ctx.comment_at(text, skip_indent(line));
            loop {
                let start = logical_end(text, line);
                if start == text.len_chars() {
                    break;
                }
                line = boundary(text, start, true);
                let mut end = skip_indent(line);
                if let Some(next) = ctx.comment_at(text, end) {
                    if token == Some(next) {
                        end = skip_indent(end + next.chars().count());
                    }
                    token = Some(next);
                }
                joins.entry(start).or_insert_with(|| Edit {
                    start,
                    end,
                    insert: if end == logical_end(text, line) {
                        String::new()
                    } else {
                        " ".into()
                    },
                });
                if line >= last {
                    break;
                }
            }
        }
        self.edits.extend(joins.into_values());
        if self.edits.is_empty() {
            return;
        }
        self.map_edits();
        if select_spaces {
            let mut removed = 0;
            let mut spaces = Vec::new();
            for edit in &self.edits {
                if !edit.insert.is_empty() {
                    let pos = edit.start - removed;
                    spaces.push(Range {
                        anchor: pos,
                        head: pos,
                    });
                }
                removed += edit.end - edit.start - edit.insert.len();
            }
            if !spaces.is_empty() {
                self.ranges = spaces;
                self.primary = 0;
            }
        }
        self.history = History::Atomic;
    }

    pub(super) fn open_lines(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        below: bool,
        count: usize,
        ctx: &Context<'_>,
    ) {
        self.ranges.clear();
        let mut offset = 0;
        let newline = document_newline(text);
        for (i, &r) in selections.ranges().iter().enumerate() {
            let (pos, at_top) = open_line_site(text, r, below);
            let previous = if below {
                logical_start(text, pos)
            } else {
                logical_start(text, r.start())
            };
            let leading = text
                .chars_at(previous)
                .take_while(|c| matches!(c, ' ' | '\t'))
                .count();
            let indent = if let Some(token) = ctx.comment_at(text, previous + leading) {
                format!("{}{token} ", text.slice(previous..previous + leading))
            } else if !at_top
                && ctx
                    .newline_indents
                    .binary_search_by_key(&pos, |&(p, _)| p)
                    .is_ok()
            {
                ctx.indentation_at(pos, 0)
            } else {
                ctx.indent_level_of(text.slice(previous..previous + leading))
            };
            let unit = if at_top {
                format!("{indent}{newline}")
            } else {
                format!("{newline}{indent}")
            };
            let width = unit.chars().count();
            if i == selections.primary_index() {
                self.primary = self.ranges.len();
            }
            for n in 0..count {
                let caret = pos
                    + offset
                    + n * width
                    + if at_top {
                        indent.chars().count()
                    } else {
                        width
                    };
                self.ranges.push(Range {
                    anchor: caret,
                    head: caret,
                });
            }
            self.edits.push(Edit {
                start: pos,
                end: pos,
                insert: unit.repeat(count),
            });
            offset += count * width;
        }
        self.next.mode = Mode::Insert;
        self.history = History::Begin;
    }
}

/// Source offset where opening a line inserts its text, and whether the new line precedes the document.
pub fn open_line_site(text: RopeSlice<'_>, range: Range, below: bool) -> (usize, bool) {
    if below {
        let last = logical_start(text, boundary(text, range.end(), false).max(range.start()));
        (logical_end(text, last), false)
    } else {
        let first = logical_start(text, range.start());
        (boundary(text, first, false), first == 0)
    }
}
