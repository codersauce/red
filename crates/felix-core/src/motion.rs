use crate::{Range, boundary};
use regex::Regex;
use ropey::RopeSlice;
use std::sync::LazyLock;

impl Range {
    pub(crate) fn move_to(self, text: RopeSlice<'_>, target: usize, extend: bool) -> Self {
        if !extend {
            return Self::at(text, target);
        }
        let anchor = match (self.anchor <= self.head, target < self.anchor) {
            (true, true) => boundary(text, self.anchor, true),
            (false, false) => boundary(text, self.anchor, false),
            _ => self.anchor,
        };
        let head = if target < anchor {
            target
        } else {
            boundary(text, target, true)
        };
        Self { anchor, head }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Category {
    Newline,
    Space,
    Word,
    Punctuation,
    Other,
}

fn category(ch: char, long: bool) -> Category {
    // Helix distinguishes punctuation and math/currency/modifier symbols from
    // other symbols and combining marks, even when moving by WORD.
    static PUNCT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\p{P}\p{Sm}\p{Sc}\p{Sk}]").unwrap());
    match ch {
        '\n' => Category::Newline,
        c if c.is_whitespace() => Category::Space,
        c if c == '_' || c.is_alphanumeric() => Category::Word,
        c if PUNCT.is_match(c.encode_utf8(&mut [0; 4])) => {
            if long {
                Category::Word
            } else {
                Category::Punctuation
            }
        }
        _ => Category::Other,
    }
}

pub(crate) fn word(
    text: RopeSlice<'_>,
    range: Range,
    key: &str,
    count: usize,
    extend: bool,
) -> Range {
    let backward = matches!(key, "b" | "B");
    let long = key.as_bytes()[0].is_ascii_uppercase();
    let to_start = matches!(key, "w" | "W");
    if backward && range.head == 0 || !backward && range.head == text.len_chars() {
        return range;
    }
    let cursor = range.cursor(text);
    let next = boundary(text, cursor, true);
    let mut span = if backward {
        Range {
            anchor: next,
            head: cursor,
        }
    } else {
        Range {
            anchor: cursor,
            head: next,
        }
    };
    for _ in 0..count {
        let origin = span;
        let mut chars = text.chars_at(span.head);
        let mut previous = if backward {
            text.get_char(span.head)
        } else {
            span.head.checked_sub(1).and_then(|i| text.get_char(i))
        };
        let mut at_start = true;
        let mut skipping_lines = true;
        while let Some(ch) = if backward { chars.prev() } else { chars.next() } {
            if skipping_lines && ch == '\n' {
                span.head = if backward {
                    span.head - 1
                } else {
                    span.head + 1
                };
                span.anchor = span.head;
                previous = Some(ch);
                continue;
            }
            if skipping_lines && previous == Some('\n') {
                span.anchor = span.head;
            }
            skipping_lines = false;
            let hit = previous.is_none_or(|prev| {
                category(ch, long) != category(prev, long)
                    && (ch == '\n'
                        || if to_start {
                            !ch.is_whitespace()
                        } else {
                            !prev.is_whitespace()
                        })
            });
            if hit {
                if !at_start {
                    break;
                }
                span.anchor = span.head;
            }
            previous = Some(ch);
            span.head = if backward {
                span.head - 1
            } else {
                span.head + 1
            };
            at_start = false;
        }
        if origin == span {
            break;
        }
    }
    if extend {
        range.move_to(text, span.cursor(text), true)
    } else {
        span
    }
}

pub(crate) fn line_end(text: RopeSlice<'_>, line: usize) -> usize {
    let end = text.line_to_char(line + 1);
    if end > text.line_to_char(line) && text.char(end - 1) == '\n' {
        boundary(text, end, false)
    } else {
        end
    }
}

pub(crate) fn goto(
    text: RopeSlice<'_>,
    r: Range,
    key: &str,
    count: Option<usize>,
    extend: bool,
) -> Range {
    let line = text.char_to_line(r.cursor(text));
    let last = text.len_lines() - 1;
    let last_non_eof = if text.line(last).len_chars() == 0 {
        last.saturating_sub(1)
    } else {
        last
    };
    let pos = match key {
        "g" => text.line_to_char(count.unwrap_or(1).saturating_sub(1).min(last_non_eof)),
        "e" => {
            // Red's rope indexes additional Unicode separators; the pinned Helix
            // release counts LF (including CRLF) when jumping to the last line.
            let mut last_line = last_non_eof;
            while last_line > 0 && text.char(text.line_to_char(last_line) - 1) != '\n' {
                last_line -= 1;
            }
            text.line_to_char(last_line)
        }
        "h" => text.line_to_char(line),
        "l" => boundary(text, line_end(text, line), false).max(text.line_to_char(line)),
        "s" => match text.line(line).chars().position(|c| !c.is_whitespace()) {
            Some(n) => text.line_to_char(line) + n,
            None => return r,
        },
        _ => return r,
    };
    r.move_to(text, pos, extend)
}

pub(crate) fn find(
    text: RopeSlice<'_>,
    r: Range,
    command: char,
    ch: char,
    count: usize,
    extend: bool,
) -> Range {
    let backward = matches!(command, 'F' | 'T');
    let till = matches!(command, 't' | 'T');
    let start = if r.anchor < r.head {
        r.head - 1
    } else {
        r.head
    };
    let next = start.saturating_add(1).min(text.len_chars());
    let adjacent = if backward {
        start.checked_sub(1)
    } else {
        Some(next)
    };
    let skip = usize::from(till && adjacent.and_then(|i| text.get_char(i)) == Some(ch));
    let nth = (count - 1).saturating_add(skip);
    let matched = if backward {
        (0..start).rev().filter(|&i| text.char(i) == ch).nth(nth)
    } else {
        (next..text.len_chars())
            .filter(|&i| text.char(i) == ch)
            .nth(nth)
    };
    let Some(pos) = matched else { return r };
    let target = match (backward, till) {
        (true, true) => pos + 1,
        (false, true) => pos.saturating_sub(1),
        _ => pos,
    };
    let origin = if extend {
        r
    } else {
        let cursor = r.cursor(text);
        Range {
            anchor: cursor,
            head: cursor,
        }
    };
    origin.move_to(text, target, true)
}

pub(crate) fn word_object(text: RopeSlice<'_>, r: Range, long: bool, around: bool) -> Range {
    let pos = r.cursor(text);
    let class = text.get_char(pos).map(|c| category(c, false));
    let mut start = pos;
    while start > 0 {
        let c = text.char(start - 1);
        if c.is_whitespace() || !long && pos < text.len_chars() && Some(category(c, false)) != class
        {
            break;
        }
        start -= 1;
    }
    let mut end = pos;
    while let Some(c) = text.get_char(end) {
        if c.is_whitespace() || !long && Some(category(c, false)) != class {
            break;
        }
        end += 1;
    }
    if around && start != end {
        // Helix around-word objects include zero-width spaces but exclude line breaks.
        let spaces = |c| {
            matches!(
                c,
                ' ' | '\t' | '\u{a0}' | '\u{180e}' | '\u{2000}'
                    ..='\u{200b}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
            )
        };
        let trailing = text.chars_at(end).take_while(|&c| spaces(c)).count();
        if trailing == 0 {
            while start > 0 && spaces(text.char(start - 1)) {
                start -= 1;
            }
        } else {
            end += trailing;
        }
    }
    Range {
        anchor: start,
        head: end,
    }
}

fn brackets(ch: char) -> Option<(char, char)> {
    [
        ('(', ')'),
        ('[', ']'),
        ('{', '}'),
        ('<', '>'),
        ('‘', '’'),
        ('“', '”'),
        ('«', '»'),
        ('「', '」'),
        ('（', '）'),
    ]
    .into_iter()
    .find(|&(open, close)| ch == open || ch == close)
}

/// Find the requested enclosing delimiter, skipping balanced inner pairs.
fn delimiter_offset(
    chars: impl Iterator<Item = char>,
    wanted: char,
    opposite: char,
    count: usize,
) -> Option<usize> {
    let mut depth = 0;
    let mut remaining = count;
    for (index, ch) in chars.enumerate() {
        if ch == wanted {
            if depth == 0 {
                remaining -= 1;
                if remaining == 0 {
                    return Some(index + 1);
                }
            } else {
                depth -= 1;
            }
        } else if ch == opposite {
            depth += 1;
        }
    }
    None
}

pub(crate) fn match_bracket(text: RopeSlice<'_>, pos: usize) -> Option<usize> {
    const SCAN_LIMIT: usize = 10_000;
    let ch = text.get_char(pos)?;
    let (open, close) = brackets(ch)?;
    if ch == open {
        let chars = text.chars_at(pos + 1).take(SCAN_LIMIT);
        delimiter_offset(chars, close, open, 1).map(|offset| pos + offset)
    } else {
        let chars = text.chars_at(pos).reversed().take(SCAN_LIMIT);
        delimiter_offset(chars, open, close, 1).map(|offset| pos - offset)
    }
}

pub(crate) fn pair_object(
    text: RopeSlice<'_>,
    r: Range,
    delimiter: char,
    around: bool,
    count: usize,
) -> Range {
    if delimiter.is_ascii_alphanumeric() || r.end() >= text.len_chars() || count == 0 {
        return r;
    }
    let (open, close) = brackets(delimiter).unwrap_or((delimiter, delimiter));
    let pos = r.cursor(text);
    let ch = text.char(pos);
    // A quote under the cursor could belong to either side of the object.
    if open == close && ch == open {
        return r;
    }
    let start = if ch == open {
        Some(pos)
    } else {
        delimiter_offset(text.chars_at(pos).reversed(), open, close, count)
            .map(|offset| pos - offset)
    };
    let end = if ch == close {
        Some(pos)
    } else {
        delimiter_offset(text.chars_at(pos + 1), close, open, count).map(|offset| pos + offset)
    };
    let (Some(start), Some(end)) = (start, end) else {
        return r;
    };
    if around {
        r.with_bounds(start, boundary(text, end, true))
    } else {
        r.with_bounds(boundary(text, start, true), end)
    }
}

pub(crate) fn vertical(
    text: RopeSlice<'_>,
    r: Range,
    down: bool,
    count: usize,
    tab_width: usize,
    goal: Option<usize>,
    extend: bool,
) -> (Range, usize) {
    use unicode_width::UnicodeWidthStr;
    let cursor = r.cursor(text);
    let row = text.char_to_line(cursor);
    let width = |a, b, x| {
        if text.char(a) == '\t' {
            tab_width.max(1) - x % tab_width.max(1)
        } else {
            UnicodeWidthStr::width(std::borrow::Cow::<str>::from(text.slice(a..b)).as_ref()).max(1)
        }
    };
    let goal = goal.unwrap_or_else(|| {
        let mut col = 0;
        let mut i = text.line_to_char(row);
        while i < cursor {
            let next = boundary(text, i, true);
            col += width(i, next, col);
            i = next;
        }
        col
    });
    let row = if down {
        row.saturating_add(count).min(text.len_lines() - 1)
    } else {
        row.saturating_sub(count)
    };
    if extend && text.line(row).len_chars() == 0 {
        return (r, goal);
    }
    let mut pos = text.line_to_char(row);
    let end = line_end(text, row);
    let mut col = 0;
    while pos < end {
        let next = boundary(text, pos, true);
        col += width(pos, next, col);
        if col > goal {
            break;
        }
        pos = next;
    }
    (r.move_to(text, pos, extend), goal)
}

pub(crate) fn paragraph(text: RopeSlice<'_>, r: Range, count: usize, around: bool) -> Range {
    let cursor = r.cursor(text);
    let row = text.char_to_line(cursor);
    let blank = |i| {
        let line = text.line(i);
        line.len_chars() == 0 || line == "\n" || line == "\r\n"
    };
    let current_blank = blank(row);
    let previous_blank = blank(row.saturating_sub(1));
    let at_separator = current_blank
        && !previous_blank
        && cursor == boundary(text, text.line_to_char(row + 1), false);
    let mut start = row + usize::from(current_blank != previous_blank);
    let mut end = row + usize::from(at_separator);
    let previous_paragraph = |mut line: usize| {
        while line > 0 && blank(line - 1) {
            line -= 1;
        }
        while line > 0 && !blank(line - 1) {
            line -= 1;
        }
        line
    };
    if !at_separator {
        start = previous_paragraph(start);
    }
    let mut found = 0;
    for _ in 0..count {
        let first = end;
        while end < text.len_lines() && !blank(end) {
            end += 1;
        }
        if first != end {
            found += 1;
        }
        while end < text.len_lines() && blank(end) {
            end += 1;
        }
        if end == text.len_lines() {
            break;
        }
    }
    if found < count && end == text.len_lines() {
        start = previous_paragraph(start);
    }
    if !around {
        while end > 0 && blank(end - 1) {
            end -= 1;
        }
    }
    Range {
        anchor: text.line_to_char(start),
        head: text.line_to_char(end),
    }
}

pub(crate) fn copies(
    text: RopeSlice<'_>,
    selections: &crate::Selections,
    down: bool,
    count: usize,
    tab_width: usize,
) -> crate::Selections {
    let mut out = selections.ranges().to_vec();
    let mut primary = selections.primary_index();
    for (i, &r) in selections.ranges().iter().enumerate() {
        let head = if r.head > r.anchor {
            r.head - 1
        } else {
            r.head
        };
        let anchor = if r.head > r.anchor {
            r.anchor
        } else {
            r.anchor.saturating_sub(1)
        };
        let rows = text.char_to_line(head).abs_diff(text.char_to_line(anchor)) + 1;
        let max = if down {
            text.len_lines() - 1 - text.char_to_line(head.max(anchor))
        } else {
            text.char_to_line(head.max(anchor))
        };
        let mut added = 0;
        for step in (rows..=max).step_by(rows) {
            let project = |pos| {
                let base = Range::at(text, pos);
                let (next, goal) = vertical(text, base, down, step, tab_width, None, false);
                let (_, actual) = vertical(text, next, down, 0, tab_width, None, false);
                (goal == actual).then(|| next.cursor(text))
            };
            if let (Some(a), Some(h)) = (project(anchor), project(head)) {
                let copied = Range { anchor: a, head: a }.move_to(text, h, true);
                if i == selections.primary_index() {
                    primary = out.len();
                }
                out.push(copied);
                added += 1;
                if added >= count {
                    break;
                }
            }
        }
    }
    crate::Selections::new(text, out, primary).expect("copy retains original selections")
}
