use super::*;
use crate::motion::{self, logical_end, logical_start};

impl Plan {
    /// Rebase all endpoints against the planned set of ordered edits once.
    pub(super) fn map_edits(&mut self) {
        let mut previous = 0;
        let mut mapped = 0;
        let offsets: Vec<_> = self
            .edits
            .iter()
            .map(|edit| {
                let start = mapped + (edit.start - previous);
                let end = start + edit.insert.chars().count();
                previous = edit.end;
                mapped = end;
                (start, end)
            })
            .collect();
        let map = |pos, after| {
            let i = self
                .edits
                .partition_point(|e| e.end < pos || e.end == pos && e.start != e.end);
            if let Some(e) = self.edits.get(i) {
                let (lo, hi) = offsets[i];
                if pos >= e.start {
                    if e.start != e.end && e.end - e.start == hi - lo {
                        return lo + pos - e.start;
                    }
                    return if after { hi } else { lo };
                }
            }
            if i == 0 {
                pos
            } else {
                offsets[i - 1].1 + pos - self.edits[i - 1].end
            }
        };
        for r in &mut self.ranges {
            *r = Range {
                anchor: map(r.anchor, r.anchor <= r.head),
                head: map(r.head, r.head <= r.anchor),
            };
        }
    }

    pub(super) fn enter_insert(&mut self, text: RopeSlice<'_>, selections: &Selections, key: &str) {
        self.next.mode = Mode::Insert;
        self.history = History::Begin;
        self.next.restore_append = key == "a";
        if key == "a"
            && selections
                .ranges()
                .iter()
                .any(|r| r.end() == text.len_chars() && r.start() != r.end())
        {
            self.edits.push(Edit {
                start: text.len_chars(),
                end: text.len_chars(),
                insert: document_newline(text).into(),
            });
        }
        for (i, &r) in selections.ranges().iter().enumerate() {
            self.ranges[i] = match key {
                "i" => Range {
                    anchor: r.end(),
                    head: r.start(),
                },
                "a" => {
                    let head = if r.end() == text.len_chars() && !self.edits.is_empty() {
                        r.end() + document_newline(text).chars().count()
                    } else {
                        boundary(text, r.end(), true)
                    };
                    Range {
                        anchor: r.start(),
                        head,
                    }
                }
                _ => {
                    let start = logical_start(text, r.cursor(text));
                    let end = logical_end(text, start);
                    let pos = if key == "A" {
                        end
                    } else {
                        start
                            + text
                                .slice(start..end)
                                .chars()
                                .position(|c| !c.is_whitespace())
                                .unwrap_or(0)
                    };
                    Range {
                        anchor: pos,
                        head: pos,
                    }
                }
            };
        }
    }

    fn insert_pair(&mut self, text: RopeSlice<'_>, selections: &Selections, ch: char) {
        let close = match ch {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            ch => ch,
        };
        let closing = matches!(ch, ')' | ']' | '}');
        let symmetric = matches!(ch, '\'' | '"' | '`');
        let mut offset = 0;
        for (i, &r) in selections.ranges().iter().enumerate() {
            let pos = r.cursor(text);
            let under = text.get_char(pos);
            let skip = ch == close && under == Some(ch);
            let previous_word = pos
                .checked_sub(1)
                .and_then(|i| text.get_char(i))
                .is_some_and(char::is_alphanumeric);
            let both = !closing
                && !skip
                && !under.is_some_and(char::is_alphanumeric)
                && !(symmetric && previous_word);
            let width = if skip {
                0
            } else if both {
                2
            } else {
                1
            };
            if !skip {
                self.edits.push(Edit {
                    start: pos,
                    end: pos,
                    insert: if both {
                        format!("{ch}{close}")
                    } else {
                        ch.into()
                    },
                });
            }
            let single = r.start() != r.end() && boundary(text, r.start(), true) == r.end();
            self.ranges[i] = if pos == text.len_chars() && r.anchor == r.head {
                Range {
                    anchor: pos + offset + 1,
                    head: pos + offset + 1,
                }
            } else if skip {
                Range {
                    anchor: offset
                        + if single {
                            boundary(text, r.anchor, true)
                        } else {
                            r.anchor
                        },
                    head: offset + boundary(text, r.head, true),
                }
            } else if !both {
                Range {
                    anchor: r.anchor + offset + usize::from(single || r.anchor > r.head),
                    head: r.head + offset + 1,
                }
            } else if r.anchor > r.head {
                Range {
                    anchor: if single {
                        pos + offset + 2
                    } else {
                        r.anchor + offset + 2
                    },
                    head: pos + offset + 1,
                }
            } else {
                Range {
                    anchor: if single {
                        pos + offset + 1
                    } else {
                        r.anchor + offset
                    },
                    head: pos + offset + 2,
                }
            };
            offset += width;
        }
    }

    fn insert_newline(&mut self, text: RopeSlice<'_>, selections: &Selections, ctx: &Context<'_>) {
        let newline = document_newline(text);
        let mut displacement = 0isize;
        let mut previous_cursor = 0;
        for (index, &range) in selections.ranges().iter().enumerate() {
            let pos = range.cursor(text);
            let line = logical_start(text, pos);
            let prefix = text.slice(line..pos);
            let trailing = prefix
                .chars_at(prefix.len_chars())
                .reversed()
                .take_while(|c| c.is_whitespace())
                .count();
            let leading = prefix
                .chars()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .count();
            let token = ctx.comment_at(text, line + leading);
            let (start, end, mut insert) = if trailing == prefix.len_chars() {
                (line, line, newline.to_owned())
            } else {
                let width = ctx.tab_width.max(1);
                let columns = prefix
                    .chars()
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .fold(0, |col, ch| {
                        col + if ch == '\t' { width - col % width } else { 1 }
                    });
                let indent = if let Some(token) = token {
                    format!("{}{token} ", text.slice(line..line + leading))
                } else {
                    ctx.indentation_at(pos, columns)
                };
                (
                    (pos - trailing).max(previous_cursor),
                    pos,
                    format!("{newline}{indent}"),
                )
            };
            let paired = pos
                .checked_sub(1)
                .and_then(|i| text.get_char(i))
                .zip(text.get_char(pos))
                .is_some_and(|pair| {
                    matches!(
                        pair,
                        ('(', ')')
                            | ('[', ']')
                            | ('{', '}')
                            | ('"', '"')
                            | ('\'', '\'')
                            | ('`', '`')
                    )
                });
            let inner = if paired && token.is_none() {
                let outer = insert.clone();
                insert.push_str(ctx.indent);
                let inner = insert.chars().count();
                insert.push_str(&outer);
                inner
            } else {
                insert.chars().count()
            };
            let advance = inner as isize - (end - start) as isize;
            self.ranges[index] = Range {
                anchor: range.anchor.saturating_add_signed(if pos > range.anchor {
                    displacement
                } else {
                    displacement + advance
                }),
                head: range.head.saturating_add_signed(displacement + advance),
            };
            displacement += insert.chars().count() as isize - (end - start) as isize;
            self.edits.push(Edit { start, end, insert });
            previous_cursor = pos;
        }
    }

    pub(super) fn insert_key(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        key: &str,
        ctx: &Context<'_>,
    ) {
        match key {
            "(" | ")" | "[" | "]" | "{" | "}" | "\"" | "'" | "`" => {
                self.insert_pair(text, selections, key.chars().next().unwrap())
            }
            "<esc>" | "<c-c>" => {
                if self.next.restore_append {
                    self.set(selections.transform(text, |r| Range {
                        anchor: r.start(),
                        head: if r.head > r.anchor {
                            boundary(text, r.end(), false)
                        } else {
                            r.end()
                        },
                    }));
                    self.next.restore_append = false;
                }
                self.next.mode = Mode::Normal;
                self.history = History::Commit;
            }
            "<tab>"
                if selections.ranges().iter().any(|r| {
                    let cursor = r.cursor(text);
                    let start = logical_start(text, cursor);
                    text.slice(start..cursor)
                        .chars()
                        .any(|c| !c.is_whitespace())
                }) =>
            {
                self.host = Some(HostAction::SyntaxParentEnd)
            }
            "<pageup>" | "<pagedown>" => self.page(text, selections, key, ctx),
            "<c-s>" => self.history = History::Checkpoint,
            "<left>" | "<right>" | "<home>" | "<end>" => {
                self.set(selections.transform(text, |r| {
                    let pos = r.cursor(text);
                    let start = logical_start(text, pos);
                    let dest = match key {
                        "<left>" => boundary(text, pos, false),
                        "<right>" => boundary(text, pos, true),
                        "<home>" => start,
                        _ => logical_end(text, start),
                    };
                    Range::at(text, dest)
                }))
            }
            "<up>" | "<down>" => {
                self.visual_vertical(text, selections, if key == "<down>" { 1 } else { -1 }, ctx)
            }
            "<backspace>" | "<c-h>" | "<delete>" | "<c-d>" | "<c-w>" | "<a-d>"
            | "<a-backspace>" | "<a-delete>" | "<c-u>" | "<c-k>" => {
                let forward =
                    matches!(key, "<delete>" | "<c-d>" | "<a-d>" | "<a-delete>" | "<c-k>");
                let mut append_eol = false;
                for (i, &r) in selections.ranges().iter().enumerate() {
                    let pos = r.cursor(text);
                    let start = logical_start(text, pos);
                    let end = logical_end(text, start);
                    let other = match key {
                        "<c-u>" if pos == start && start > 0 => boundary(text, start, false),
                        "<c-u>" => {
                            let first = start
                                + text
                                    .slice(start..end)
                                    .chars()
                                    .position(|c| !c.is_whitespace())
                                    .unwrap_or(0);
                            if first < pos { first } else { start }
                        }
                        "<c-k>" if pos == end => boundary(text, end, true),
                        "<c-k>" => end,
                        "<c-w>" | "<a-backspace>" => motion::word(text, r, "b", 1, false).start(),
                        "<a-d>" | "<a-delete>" => motion::word(text, r, "e", 1, false).end(),
                        _ => boundary(text, pos, forward),
                    };
                    if forward && r.head > r.anchor {
                        self.ranges[i].head = (r.head + 1).min(text.len_chars());
                        append_eol |= other == text.len_chars();
                    }
                    let (start, end) = (pos.min(other), pos.max(other));
                    if start < end {
                        if let Some(last) = self.edits.last_mut().filter(|e| e.end >= start) {
                            last.end = last.end.max(end);
                        } else {
                            self.edits.push(Edit {
                                start,
                                end,
                                insert: String::new(),
                            });
                        }
                    }
                }
                self.map_edits();
                if append_eol {
                    self.edits.push(Edit {
                        start: text.len_chars(),
                        end: text.len_chars(),
                        insert: document_newline(text).into(),
                    });
                }
            }
            "<enter>" | "<c-j>" => self.insert_newline(text, selections, ctx),
            _ => {
                for &r in selections.ranges() {
                    let pos = r.cursor(text);
                    let insert = match key {
                        "<tab>" | "<backtab>" => ctx.indent.into(),
                        _ if key.chars().count() == 1 => key.into(),
                        _ => return,
                    };
                    self.edits.push(Edit {
                        start: pos,
                        end: pos,
                        insert,
                    });
                }
                self.map_edits();
            }
        }
    }
}
