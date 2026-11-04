use super::*;

/// Commands requiring document history, other buffers, or host services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Navigation {
    Alternate,
    ModifiedBuffer,
    LastEdit,
    File,
    Definition(&'static str),
    CenterHorizontal,
    Diagnostic {
        forward: bool,
        edge: bool,
    },
    Change {
        forward: bool,
        edge: bool,
        count: usize,
    },
    ChangeObject,
    History {
        later: bool,
        count: usize,
    },
}

fn empty_line(text: RopeSlice<'_>, line: usize) -> bool {
    matches!(
        text.line(line).chars().next(),
        Some('\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}')
    )
}

pub(super) fn paragraph(
    text: RopeSlice<'_>,
    r: Range,
    forward: bool,
    count: usize,
    extend: bool,
) -> Range {
    let cursor = r.cursor(text);
    let mut line = text.char_to_line(cursor);
    let line_count = text.len_lines();
    let boundary_line = if forward {
        empty_line(text, line)
            && !empty_line(text, (line + 1).min(line_count - 1))
            && cursor == boundary(text, text.line_to_char(line + 1), false)
    } else {
        empty_line(text, line.saturating_sub(1)) && !empty_line(text, line)
    };
    let at_start = text.line_to_char(line) == cursor;
    let move_anchor = if forward {
        if boundary_line {
            line += 1;
            r.head
        } else {
            cursor
        }
    } else if boundary_line && at_start {
        cursor
    } else {
        if boundary_line {
            line += 1;
        }
        r.head
    };
    for _ in 0..count {
        let before = line;
        if forward {
            while line < line_count && !empty_line(text, line) {
                line += 1;
            }
            while line < line_count && empty_line(text, line) {
                line += 1;
            }
        } else {
            while line > 0 && empty_line(text, line - 1) {
                line -= 1;
            }
            while line > 0 && !empty_line(text, line - 1) {
                line -= 1;
            }
        }
        if before == line {
            break;
        }
    }
    let head = text.line_to_char(line);
    Range {
        anchor: if extend {
            r.move_to(text, head, true).anchor
        } else {
            move_anchor
        },
        head,
    }
}

impl Session {
    pub(super) fn view_command(
        &self,
        text: RopeSlice<'_>,
        sels: &Selections,
        key: &str,
        ctx: &Context<'_>,
        mut p: Plan,
    ) -> Plan {
        p.next.pending = Pending::None;
        p.next.count = None;
        let cursor = sels.primary().cursor(text);
        let last = ctx.page_height.max(1) - 1;
        match key {
            "z" | "c" | "t" | "b" => {
                let margin = ctx.scrolloff.min(last / 2);
                let rows = match key {
                    "t" => margin,
                    "b" => last - margin,
                    _ => last / 2,
                };
                p.viewport_start = Some(ctx.row_start(text, cursor, -(rows as isize)));
            }
            "m" => p.host = Some(HostAction::Navigate(Navigation::CenterHorizontal)),
            "k" | "<up>" | "j" | "<down>" => {
                let down = matches!(key, "j" | "<down>");
                let n = self.count.unwrap_or(1).min(isize::MAX as usize) as isize;
                let top = ctx.row_start(text, ctx.viewport_start, if down { n } else { -n });
                p.viewport_start = Some(top);
                let margin = ctx.scrolloff.min(last / 2);
                let edge = ctx.row_start(
                    text,
                    top,
                    if down { margin } else { last - margin } as isize,
                );
                if (down && cursor < edge) || (!down && cursor > edge) {
                    p.ranges[p.primary] = Range {
                        anchor: if self.mode == Mode::Select {
                            sels.primary().anchor
                        } else {
                            edge
                        },
                        head: edge,
                    };
                }
            }
            "<c-b>" | "<pageup>" | "<c-f>" | "<pagedown>" | "<c-u>" | "<backspace>" | "<c-d>"
            | " " => {
                p.page(
                    text,
                    sels,
                    match key {
                        " " => "<c-d>",
                        "<backspace>" => "<c-u>",
                        _ => key,
                    },
                    ctx,
                );
            }
            "/" | "?" | "n" | "N" => {
                let mut next = self.clone();
                next.sticky_view = false;
                next.pending = Pending::None;
                p = next.plan(text, sels, key, ctx);
                p.next.sticky_view = self.sticky_view;
            }
            "<esc>" => p.next.sticky_view = false,
            _ => {}
        }
        p
    }
}

impl Plan {
    pub(super) fn goto_window(
        &mut self,
        text: RopeSlice<'_>,
        sels: &Selections,
        key: &str,
        count: usize,
        ctx: &Context<'_>,
    ) {
        let height = ctx.page_height.max(1);
        let margin = ctx.scrolloff.min((height - 1) / 2);
        let mut last = 0usize;
        let mut row = ctx.row_start(text, ctx.viewport_start, 0);
        for _ in 1..height {
            let next = ctx.row_start(text, row, 1);
            if row == next {
                break;
            }
            last += 1;
            row = next;
        }
        let offset = match key {
            "t" => margin.saturating_add(count - 1),
            "b" => last.saturating_sub(margin.saturating_add(count - 1)),
            _ => last / 2,
        };
        let pos = ctx.row_start(
            text,
            ctx.viewport_start,
            offset.max(margin).min(last.saturating_sub(margin)) as isize,
        );
        self.set(sels.transform(text, |r| {
            r.move_to(text, pos, self.next.mode == Mode::Select)
        }));
    }

    fn remap_inserts(&mut self) {
        // Both inputs are ordered. An insertion at the start sticks before
        // the selection; one at the end sticks after it.
        let mut edits = self.edits.iter().peekable();
        let mut shift = 0;
        let mut map = |pos, inclusive| {
            while let Some(edit) = edits.next_if(|e| e.start < pos || (inclusive && e.start == pos))
            {
                shift += edit.insert.chars().count();
            }
            pos + shift
        };
        for r in &mut self.ranges {
            let start = map(r.start(), true);
            let end = if r.start() == r.end() {
                start
            } else {
                map(r.end(), false)
            };
            *r = r.with_bounds(start, end);
        }
        self.history = History::Atomic;
    }

    pub(super) fn insert_blank_lines(
        &mut self,
        text: RopeSlice<'_>,
        sels: &Selections,
        below: bool,
        count: usize,
    ) {
        let newline = document_newline(text);
        let mut value = String::new();
        if count.checked_mul(newline.len()).is_none_or(|bytes| {
            bytes > 8 * 1024 * 1024 / sels.ranges().len() || value.try_reserve_exact(bytes).is_err()
        }) {
            self.message = Some("Too many newlines".into());
            return;
        }
        for _ in 0..count {
            value.push_str(newline);
        }
        for &r in sels.ranges() {
            let (start, end) = r.lines(text);
            let pos = text.line_to_char(if below { end + 1 } else { start });
            self.edits.push(Edit {
                start: pos,
                end: pos,
                insert: value.clone(),
            });
        }
        self.remap_inserts();
    }

    pub(super) fn rotate_contents(
        &mut self,
        text: RopeSlice<'_>,
        sels: &Selections,
        forward: bool,
        count: Option<usize>,
    ) {
        let mut fragments = sels
            .ranges()
            .iter()
            .map(|r| text.slice(r.start()..r.end()).to_string())
            .collect::<Vec<_>>();
        let group = count.unwrap_or(fragments.len()).clamp(1, fragments.len());
        for chunk in fragments.chunks_mut(group) {
            if forward {
                chunk.rotate_right(1)
            } else {
                chunk.rotate_left(1)
            }
        }
        let mut removed = 0;
        let mut added = 0;
        for ((&r, dest), fragment) in sels.ranges().iter().zip(&mut self.ranges).zip(fragments) {
            let start = r.start() - removed + added;
            let n = fragment.chars().count();
            *dest = r.with_bounds(start, start + n);
            self.edits.push(Edit {
                start: r.start(),
                end: r.end(),
                insert: fragment,
            });
            removed += r.end() - r.start();
            added += n;
        }
        self.history = History::Atomic;
    }

    pub(super) fn align(&mut self, text: RopeSlice<'_>, sels: &Selections, tab: usize) {
        let mut columns: Vec<Vec<(usize, usize)>> = Vec::new();
        let mut last = None;
        let mut column = 0;
        for &r in sels.ranges() {
            let row = text.char_to_line(r.head);
            if row != text.char_to_line(r.anchor) {
                self.message = Some("align cannot work with multi line selections".into());
                return;
            }
            column = if last == Some(row) { column + 1 } else { 0 };
            last = Some(row);
            if column == columns.len() {
                columns.push(Vec::new());
            }
            columns[column].push((
                r.start(),
                crate::motion::column_at(text, text.line_to_char(row), r.head, tab),
            ));
        }
        let mut offsets = vec![0; columns[0].len()];
        for col in columns {
            let target = col
                .iter()
                .zip(&offsets)
                .map(|((_, c), o)| c + o)
                .max()
                .unwrap_or(0);
            for ((at, c), offset) in col.into_iter().zip(&mut offsets) {
                let n = target - c - *offset;
                if n > 0 {
                    *offset += n;
                    self.edits.push(Edit {
                        start: at,
                        end: at,
                        insert: " ".repeat(n),
                    });
                }
            }
        }
        self.edits.sort_by_key(|e| e.start);
        self.remap_inserts();
        self.next.mode = Mode::Normal;
    }
}

impl Session {
    /// Visible word ranges indexed by their two-letter jump labels.
    pub fn word_labels(&self) -> &[Range] {
        match &self.pending {
            Pending::WordLabels { words, .. } => words,
            _ => &[],
        }
    }
}

impl Plan {
    pub(super) fn start_word_labels(
        &mut self,
        text: RopeSlice<'_>,
        primary: Range,
        ctx: &Context<'_>,
    ) {
        // Interleave the next and previous words around the primary cursor. At
        // most 26² candidates can be addressed with the pinned default alphabet.
        let cursor = primary.cursor(text);
        let start = text.line_to_char(text.char_to_line(ctx.viewport_start));
        let bottom = ctx.row_start(text, ctx.viewport_start, ctx.page_height as isize);
        let end = text.line_to_char((text.char_to_line(bottom) + 1).min(text.len_lines()));
        let mut f = Range {
            anchor: cursor,
            head: cursor,
        };
        let mut b = f;
        if text.get_char(cursor).is_some_and(|c| !c.is_whitespace()) {
            let end = crate::motion::word(text, f, "e", 1, false);
            if end.anchor == cursor {
                f = end;
            }
            let begin = crate::motion::word(text, b, "b", 1, false);
            if begin.anchor == boundary(text, cursor, true) {
                b = begin;
            }
        }
        let mut words = Vec::new();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        loop {
            let old_len = words.len();
            for forward in [true, false] {
                let next = if forward { &mut f } else { &mut b };
                while if forward {
                    next.head < end
                } else {
                    next.head > start
                } {
                    let head = next.head;
                    *next =
                        crate::motion::word(text, *next, if forward { "e" } else { "b" }, 1, false);
                    if next.head == head {
                        break;
                    }
                    let mut pos = next.head;
                    let is_long = (0..2).all(|_| {
                        let other = boundary(text, pos, !forward);
                        let valid = other != pos
                            && text
                                .slice(other.min(pos)..other.max(pos))
                                .chars()
                                .all(is_word);
                        pos = other;
                        valid
                    });
                    if !is_long {
                        continue;
                    }
                    if forward {
                        next.anchor += text
                            .chars_at(next.anchor)
                            .take_while(|&c| !is_word(c))
                            .count();
                    } else {
                        next.anchor -= text
                            .chars_at(next.anchor)
                            .reversed()
                            .take_while(|&c| !is_word(c))
                            .count();
                    }
                    words.push(*next);
                    break;
                }
                if words.len() == 26 * 26 {
                    break;
                }
            }
            if words.len() == old_len || words.len() == 26 * 26 {
                break;
            }
        }
        if !words.is_empty() {
            self.next.pending = Pending::WordLabels {
                words: words.into(),
                group: None,
            };
        }
    }

    pub(super) fn word_label_key(
        &mut self,
        text: RopeSlice<'_>,
        sels: &Selections,
        words: &std::sync::Arc<[Range]>,
        group: Option<usize>,
        key: &str,
    ) {
        self.next.pending = Pending::None;
        let [b'a'..=b'z'] = key.as_bytes() else {
            return;
        };
        let idx = usize::from(key.as_bytes()[0] - b'a');
        if let Some(group) = group {
            if let Some(&word) = words.get(group * 26 + idx) {
                let primary = sels.primary();
                let target = if self.next.mode != Mode::Select {
                    Range {
                        anchor: word.start(),
                        head: word.end(),
                    }
                } else {
                    Range {
                        anchor: if word.anchor < word.head {
                            primary.start().min(word.anchor)
                        } else {
                            primary.end().max(word.anchor)
                        },
                        head: word.head,
                    }
                };
                self.set(Selections::single(text, target));
            }
        } else if idx * 26 < words.len() {
            self.next.pending = Pending::WordLabels {
                words: words.clone(),
                group: Some(idx),
            };
        }
    }
}
