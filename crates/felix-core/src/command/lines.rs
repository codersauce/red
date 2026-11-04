use super::*;
use crate::motion::line_end;
use std::collections::BTreeSet;

impl Plan {
    pub(super) fn edit_lines(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        key: &str,
        count: usize,
        ctx: &Context<'_>,
    ) {
        let mut lines = BTreeSet::new();
        let join = matches!(key, "J" | "<a-J>");
        for range in selections.ranges() {
            let (first, last) = range.lines(text);
            if join {
                lines.extend(
                    first..if first == last {
                        (last + 1).min(text.len_lines() - 1)
                    } else {
                        last
                    },
                );
            } else {
                lines.extend(first..=last);
            }
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
                "J" | "<a-J>" => {
                    let start = line_end(text, line);
                    let next = text.line_to_char(line + 1);
                    let end = next
                        + text
                            .chars_at(next)
                            .take_while(|c| matches!(c, ' ' | '\t'))
                            .count();
                    Edit {
                        start,
                        end,
                        insert: if end == line_end(text, line + 1) {
                            String::new()
                        } else {
                            " ".into()
                        },
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
        if key == "<a-J>" {
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
        if !join {
            self.next.mode = Mode::Normal;
        }
    }

    pub(super) fn open_lines(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        below: bool,
        count: usize,
    ) {
        self.ranges.clear();
        let mut offset = 0;
        let newline = document_newline(text);
        for (i, &r) in selections.ranges().iter().enumerate() {
            let (first, last) = r.lines(text);
            let target = if below { last + 1 } else { first };
            let pos = if target == 0 {
                0
            } else {
                line_end(text, target - 1)
            };
            let indent: String = if target == 0 {
                String::new()
            } else {
                text.line(target - 1)
                    .chars()
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .collect()
            };
            let unit = if target == 0 {
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
                    + if target == 0 {
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
