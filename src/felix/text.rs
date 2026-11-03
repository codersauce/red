//! Pure text motions and selection edits for the felix kernel.
//!
//! Indexes are Unicode scalars. Motions land on grapheme boundaries. A failed
//! search returns `None` and leaves the caller's selection alone.

use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use super::selection::{Range, Selection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    CharLeft,
    CharRight,
    LineUp,
    LineDown,
    Word(WordMotion),
    LineStart,
    LineEnd,
    FirstNonBlank,
    FileStart,
    LastLine,
    /// 1-based line. `None` means the last line.
    GotoLine(Option<usize>),
    Find(FindKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordMotion {
    pub forward: bool,
    pub end: bool,
    pub long: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    Next,
    Prev,
    TillNext,
    TillPrev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseChange {
    Toggle,
    Lower,
    Upper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurroundOp {
    Add,
    Replace,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

struct Doc<'a> {
    text: &'a str,
    chars: Vec<char>,
    graphemes: Vec<usize>,
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mut graphemes = Vec::new();
        let mut index = 0;
        for grapheme in text.graphemes(true) {
            graphemes.push(index);
            index += grapheme.chars().count();
        }
        if graphemes.is_empty() {
            graphemes.push(0);
        }
        Self {
            text,
            chars,
            graphemes,
        }
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn clamp(&self, index: usize) -> usize {
        if self.len() == 0 {
            0
        } else {
            index.min(self.len() - 1)
        }
    }

    /// Last scalar, or the blank line after a trailing newline.
    fn cursor_limit(&self) -> usize {
        if self.len() == 0 || self.chars.last() == Some(&'\n') {
            self.len()
        } else {
            self.len() - 1
        }
    }

    fn grapheme_index(&self, head: usize) -> usize {
        self.graphemes
            .partition_point(|start| *start <= head.min(self.len()))
            .saturating_sub(1)
    }

    fn line_at(&self, index: usize) -> (usize, usize, usize) {
        let index = index.min(self.len());
        let start = self.chars[..index]
            .iter()
            .rposition(|character| *character == '\n')
            .map(|position| position + 1)
            .unwrap_or(0);
        let newline = self.chars[start..]
            .iter()
            .position(|character| *character == '\n')
            .map(|offset| start + offset);
        let content_end = newline.unwrap_or(self.len());
        let end = newline.map(|position| position + 1).unwrap_or(self.len());
        (start, content_end, end)
    }

    fn column_on_line(&self, index: usize) -> (usize, usize) {
        let (start, content_end, _) = self.line_at(index);
        let column = index
            .saturating_sub(start)
            .min(content_end.saturating_sub(start));
        (start, column)
    }

    fn move_vertical(&self, head: usize, delta: isize) -> usize {
        let (start, column) = self.column_on_line(head);
        let mut line_start = start;
        let mut remaining = delta.unsigned_abs();
        if delta < 0 {
            while remaining > 0 && line_start > 0 {
                line_start = self.line_at(line_start - 1).0;
                remaining -= 1;
            }
        } else {
            while remaining > 0 {
                if line_start >= self.len() {
                    break;
                }
                let (_, _, end) = self.line_at(line_start);
                if end < self.len() {
                    line_start = end;
                    remaining -= 1;
                    continue;
                }
                if self.chars.last() == Some(&'\n') {
                    line_start = self.len();
                }
                break;
            }
        }
        if line_start >= self.len() {
            return self.cursor_limit();
        }
        let (_, content_end, end) = self.line_at(line_start);
        if content_end == line_start {
            line_start.min(end.saturating_sub(usize::from(end > line_start)))
        } else {
            self.clamp(line_start + column.min(content_end - line_start - 1))
        }
    }
}

#[derive(Clone, Copy)]
enum WordTarget {
    NextStart,
    NextEnd,
    PrevStart,
    PrevEnd,
}

fn is_eol(character: char) -> bool {
    character == '\n' || character == '\r'
}

fn word_boundary(previous: char, next: char, long: bool) -> bool {
    let kind = |character: char| {
        if is_eol(character) {
            0
        } else if character.is_whitespace() {
            1
        } else if character.is_alphanumeric() || character == '_' {
            2
        } else {
            3
        }
    };
    let left = kind(previous);
    let right = kind(next);
    if long && ((left == 2 && right == 3) || (left == 3 && right == 2)) {
        return false;
    }
    left != right
}

fn reached_word_target(target: WordTarget, previous: char, next: char, long: bool) -> bool {
    let boundary = word_boundary(previous, next, long);
    match target {
        WordTarget::NextStart | WordTarget::PrevEnd => {
            boundary && (is_eol(next) || !next.is_whitespace())
        }
        WordTarget::NextEnd | WordTarget::PrevStart => {
            boundary && (!previous.is_whitespace() || is_eol(next))
        }
    }
}

impl Doc<'_> {
    fn next_grapheme(&self, index: usize) -> usize {
        let position = self.graphemes.partition_point(|start| *start <= index);
        self.graphemes.get(position).copied().unwrap_or(self.len())
    }

    fn prev_grapheme(&self, index: usize) -> usize {
        let position = self.graphemes.partition_point(|start| *start < index);
        if position == 0 {
            0
        } else {
            self.graphemes[position - 1]
        }
    }
}

/// Helix ranges are gaps: a forward range's head is exclusive, and the block
/// cursor sits on the grapheme just inside that edge.
fn to_helix(doc: &Doc<'_>, range: Range) -> (usize, usize) {
    if range.head > range.anchor {
        (range.anchor, doc.next_grapheme(range.head))
    } else if range.head < range.anchor {
        (doc.next_grapheme(range.anchor), range.head)
    } else {
        (range.head, range.head)
    }
}

fn from_helix(doc: &Doc<'_>, anchor: usize, head: usize) -> Range {
    if head > anchor {
        Range::new(anchor, doc.prev_grapheme(head))
    } else if head < anchor {
        Range::new(doc.prev_grapheme(anchor), head)
    } else {
        Range::cursor(head.min(doc.len()))
    }
}

fn range_to_word_target(
    doc: &Doc<'_>,
    origin_anchor: usize,
    origin_head: usize,
    target: WordTarget,
    long: bool,
) -> (usize, usize) {
    let backward = matches!(target, WordTarget::PrevStart | WordTarget::PrevEnd);
    let mut anchor = origin_anchor;
    let mut head = origin_head;
    let len = doc.len();
    let mut position = origin_head;
    let mut previous = if backward {
        (position < len).then(|| doc.chars[position])
    } else {
        position.checked_sub(1).map(|index| doc.chars[index])
    };

    loop {
        let next = if backward {
            if position == 0 {
                None
            } else {
                position -= 1;
                Some(doc.chars[position])
            }
        } else if position < len {
            let character = doc.chars[position];
            position += 1;
            Some(character)
        } else {
            None
        };
        let Some(character) = next else {
            break;
        };
        if !is_eol(character) {
            if backward {
                position += 1;
            } else {
                position -= 1;
            }
            break;
        }
        previous = Some(character);
        if backward {
            head = head.saturating_sub(1);
        } else {
            head += 1;
        }
    }
    if previous.is_some_and(is_eol) {
        anchor = head;
    }

    let head_start = head;
    loop {
        let next = if backward {
            if position == 0 {
                None
            } else {
                position -= 1;
                Some(doc.chars[position])
            }
        } else if position < len {
            let character = doc.chars[position];
            position += 1;
            Some(character)
        } else {
            None
        };
        let Some(character) = next else {
            break;
        };
        if previous.is_none() || reached_word_target(target, previous.unwrap(), character, long) {
            if head == head_start {
                anchor = head;
            } else {
                break;
            }
        }
        previous = Some(character);
        if backward {
            head = head.saturating_sub(1);
        } else {
            head += 1;
        }
    }
    (anchor, head)
}

fn word_range(doc: &Doc<'_>, range: Range, count: usize, target: WordTarget, long: bool) -> Range {
    let backward = matches!(target, WordTarget::PrevStart | WordTarget::PrevEnd);
    let (mut anchor, mut head) = to_helix(doc, range);
    if (backward && head == 0) || (!backward && head == doc.len()) {
        return from_helix(doc, anchor, head);
    }
    if backward {
        if anchor < head {
            anchor = head;
            head = doc.prev_grapheme(head);
        } else {
            anchor = doc.next_grapheme(head);
        }
    } else if anchor < head {
        anchor = doc.prev_grapheme(head);
    } else {
        let next = doc.next_grapheme(head);
        anchor = head;
        head = next;
    }
    for _ in 0..count.max(1) {
        let (next_anchor, next_head) = range_to_word_target(doc, anchor, head, target, long);
        if next_anchor == anchor && next_head == head {
            break;
        }
        anchor = next_anchor;
        head = next_head;
    }
    from_helix(doc, anchor, head)
}

/// Destination scalar for one motion, or `None` when a character search misses.
pub fn motion_head(
    text: &str,
    head: usize,
    motion: Motion,
    count: usize,
    target: Option<char>,
) -> Option<usize> {
    let doc = Doc::new(text);
    let head = doc.clamp(head);
    let count = count.max(1);
    let next = match motion {
        Motion::CharLeft => {
            let current = doc.grapheme_index(head);
            doc.graphemes[current.saturating_sub(count)]
        }
        Motion::CharRight => {
            let mut next = head;
            for _ in 0..count {
                if next >= doc.cursor_limit() {
                    break;
                }
                let stepped = doc.next_grapheme(next);
                next = if stepped == next {
                    doc.cursor_limit()
                } else {
                    stepped.min(doc.cursor_limit())
                };
            }
            next
        }
        Motion::LineUp => doc.move_vertical(head, -(count as isize)),
        Motion::LineDown => doc.move_vertical(head, count as isize),
        Motion::Word(_) => head,
        Motion::LineStart => doc.line_at(head).0,
        Motion::LineEnd => {
            let (start, content_end, _) = doc.line_at(head);
            if content_end == start {
                start
            } else {
                content_end - 1
            }
        }
        Motion::FirstNonBlank => {
            let (start, content_end, _) = doc.line_at(head);
            (start..content_end)
                .find(|index| !doc.chars[*index].is_whitespace())
                .unwrap_or(start)
        }
        Motion::FileStart => 0,
        Motion::LastLine => {
            let mut index = doc.len();
            if index > 0 && doc.chars.get(index - 1) == Some(&'\n') {
                index -= 1;
            }
            if index > 0 && doc.chars.get(index - 1) == Some(&'\n') {
                index -= 1;
            }
            doc.line_at(index).0
        }
        Motion::GotoLine(line) => {
            let target = line.unwrap_or(doc.text.lines().count().max(1));
            let mut start = 0;
            let mut seen = 1;
            if target <= 1 {
                0
            } else {
                for (index, character) in doc.chars.iter().enumerate() {
                    if *character == '\n' {
                        seen += 1;
                        start = index + 1;
                        if seen == target {
                            break;
                        }
                    }
                }
                doc.clamp(start)
            }
        }
        Motion::Find(kind) => return find_char(&doc.chars, head, target?, kind, count),
    };
    Some(next.min(doc.cursor_limit()))
}

fn find_char(
    chars: &[char],
    head: usize,
    target: char,
    kind: FindKind,
    count: usize,
) -> Option<usize> {
    let mut head = head.min(chars.len().saturating_sub(1));
    for _ in 0..count.max(1) {
        head = match kind {
            FindKind::Next => {
                let start = head + 1;
                chars[start..]
                    .iter()
                    .position(|character| *character == target)?
                    + start
            }
            FindKind::TillNext => {
                // Same extra-match skip as TillPrev, in the other direction.
                // On a run of the target, `t` covers the cursor and the next one.
                let mut remaining = count.max(1);
                if chars.get(head + 1) == Some(&target) {
                    remaining += 1;
                }
                let mut index = head;
                let mut found = None;
                while index + 1 < chars.len() {
                    index += 1;
                    if chars[index] == target {
                        remaining -= 1;
                        if remaining == 0 {
                            found = Some(index);
                            break;
                        }
                    }
                }
                return found.map(|index| index.saturating_sub(1).max(head));
            }
            FindKind::Prev => {
                chars[..head]
                    .iter()
                    .rposition(|character| *character == target)?
            }
            FindKind::TillPrev => {
                // Helix searches backward from the cursor scalar. When the
                // scalar before the cursor is already the target, it skips
                // one extra match so `T` on a run of the same character
                // covers the cursor and the match before it.
                let mut remaining = count.max(1);
                if head > 0 && chars.get(head - 1) == Some(&target) {
                    remaining += 1;
                }
                let mut index = head;
                let mut found = None;
                while index > 0 {
                    index -= 1;
                    if chars[index] == target {
                        remaining -= 1;
                        if remaining == 0 {
                            found = Some(index);
                            break;
                        }
                    }
                }
                return found.map(|index| index + 1);
            }
        };
    }
    Some(head)
}

pub fn goto_line_motion(line: Option<usize>) -> Motion {
    Motion::GotoLine(line)
}

pub fn apply_motion(
    text: &str,
    selection: &Selection,
    motion: Motion,
    count: usize,
    target: Option<char>,
    extend: bool,
) -> Option<Selection> {
    let mut missed = false;
    let doc = Doc::new(text);
    let next = selection.transform_each(|range| {
        if let Motion::Word(word) = motion {
            let target = match (word.forward, word.end) {
                (true, false) => WordTarget::NextStart,
                (true, true) => WordTarget::NextEnd,
                (false, false) => WordTarget::PrevStart,
                (false, true) => WordTarget::PrevEnd,
            };
            let moved = word_range(&doc, range, count, target, word.long);
            return if extend {
                Range::new(range.anchor, moved.head)
            } else {
                moved
            };
        }
        match motion_head(text, range.head, motion, count, target) {
            Some(head) => {
                if extend {
                    Range::new(range.anchor, head)
                } else if matches!(
                    motion,
                    Motion::CharLeft
                        | Motion::CharRight
                        | Motion::LineUp
                        | Motion::LineDown
                        | Motion::LineStart
                        | Motion::LineEnd
                        | Motion::FirstNonBlank
                        | Motion::FileStart
                        | Motion::LastLine
                        | Motion::GotoLine(_)
                ) {
                    Range::cursor(head)
                } else {
                    Range::new(range.head, head)
                }
            }
            None => {
                missed = true;
                range
            }
        }
    });
    (!missed).then_some(next)
}

pub fn line_bounds_range(text: &str, range: Range) -> Range {
    let doc = Doc::new(text);
    if doc.len() == 0 {
        return Range::cursor(0);
    }
    let (start, _, _) = doc.line_at(range.from());
    let (_, _, end) = doc.line_at(range.to());
    Range::covering(start, end.saturating_sub(1), range.is_forward())
}

pub fn extend_line_below(text: &str, range: Range) -> Range {
    let doc = Doc::new(text);
    if doc.len() == 0 {
        return Range::cursor(0);
    }
    let (start, _, end) = doc.line_at(range.head);
    let covers_line = range.from() <= start && range.to() >= end.saturating_sub(1);
    if covers_line && end < doc.len() {
        let (_, _, next_end) = doc.line_at(end);
        Range::covering(range.from().min(start), next_end.saturating_sub(1), true)
    } else if covers_line {
        range
    } else {
        Range::covering(start, end.saturating_sub(1), true)
    }
}

pub fn shrink_to_line_bounds(text: &str, range: Range) -> Range {
    let doc = Doc::new(text);
    if doc.len() == 0 {
        return Range::cursor(0);
    }
    let (start, _, end) = doc.line_at(range.from());
    let (last_start, _, last_end) = doc.line_at(range.to());
    let mut inner_start = if range.from() > start { end } else { start };
    let mut inner_end = if range.to() + 1 < last_end {
        last_start.saturating_sub(1)
    } else {
        last_end.saturating_sub(1)
    };
    if inner_start > inner_end {
        return line_bounds_range(text, Range::cursor(range.head));
    }
    inner_start = inner_start.min(doc.len().saturating_sub(1));
    inner_end = inner_end.min(doc.len().saturating_sub(1));
    Range::covering(inner_start, inner_end, range.is_forward())
}

pub fn select_all(text: &str) -> Range {
    let len = text.chars().count();
    if len == 0 {
        Range::cursor(0)
    } else {
        Range::covering(0, len - 1, true)
    }
}

pub fn copy_range_on_line(text: &str, range: Range, delta: isize) -> Option<Range> {
    let doc = Doc::new(text);
    if doc.len() == 0 || delta == 0 {
        return None;
    }
    let (start, content_end, _) = doc.line_at(range.from());
    let local_from = range
        .from()
        .saturating_sub(start)
        .min(content_end.saturating_sub(start));
    let local_to = range
        .to()
        .saturating_sub(start)
        .min(content_end.saturating_sub(start));
    let mut line_start = start;
    let mut remaining = delta.unsigned_abs();
    if delta < 0 {
        while remaining > 0 && line_start > 0 {
            line_start = doc.line_at(line_start - 1).0;
            remaining -= 1;
        }
    } else {
        while remaining > 0 {
            let (_, _, end) = doc.line_at(line_start);
            if end >= doc.len() {
                return None;
            }
            line_start = end;
            remaining -= 1;
        }
    }
    if remaining > 0 {
        return None;
    }
    let (_, next_content, _) = doc.line_at(line_start);
    let width = next_content.saturating_sub(line_start);
    if width == 0 || local_from >= width {
        return None;
    }
    let from = line_start + local_from;
    let to = line_start + local_to.min(width - 1);
    Some(Range::covering(from, to, range.is_forward()))
}

pub fn transform_case(text: &str, change: CaseChange) -> String {
    match change {
        CaseChange::Lower => text.to_lowercase(),
        CaseChange::Upper => text.to_uppercase(),
        CaseChange::Toggle => text
            .chars()
            .map(|character| {
                if character.is_lowercase() {
                    character.to_uppercase().collect::<String>()
                } else {
                    character.to_lowercase().collect::<String>()
                }
            })
            .collect(),
    }
}

pub fn replacement(text: &str, character: char) -> String {
    text.chars().map(|_| character).collect()
}

fn char_to_byte(text: &str, index: usize) -> usize {
    text.chars().take(index).map(char::len_utf8).sum()
}

pub fn regex_select(text: &str, selection: &Selection, pattern: &str) -> Result<Selection, String> {
    let regex = Regex::new(pattern).map_err(|error| error.to_string())?;
    let mut ranges = Vec::new();
    for range in selection.ranges() {
        let (start, end) = range.span(text.chars().count());
        let byte_start = char_to_byte(text, start);
        let byte_end = char_to_byte(text, end);
        for found in regex.find_iter(&text[byte_start..byte_end]) {
            let match_start = start + text[byte_start..byte_start + found.start()].chars().count();
            let match_end = start + text[byte_start..byte_start + found.end()].chars().count();
            if match_end > match_start {
                ranges.push(Range::covering(match_start, match_end - 1, true));
            }
        }
    }
    if ranges.is_empty() {
        return Err("no matches".to_string());
    }
    let mut selection = Selection::from_ranges(ranges, 0);
    selection.normalize(text.chars().count());
    Ok(selection)
}

pub fn regex_split(text: &str, selection: &Selection, pattern: &str) -> Result<Selection, String> {
    let regex = Regex::new(pattern).map_err(|error| error.to_string())?;
    if regex.is_match("") {
        return Err("pattern matches empty text".to_string());
    }
    let len = text.chars().count();
    let mut ranges = Vec::new();
    for range in selection.ranges() {
        let (start, end) = range.span(len);
        let byte_start = char_to_byte(text, start);
        let byte_end = char_to_byte(text, end);
        let slice = &text[byte_start..byte_end];
        let mut cursor = start;
        for found in regex.find_iter(slice) {
            let match_start = start + text[byte_start..byte_start + found.start()].chars().count();
            if match_start > cursor {
                ranges.push(Range::covering(cursor, match_start - 1, true));
            }
            cursor = start + text[byte_start..byte_start + found.end()].chars().count();
        }
        if end > cursor {
            ranges.push(Range::covering(cursor, end - 1, true));
        }
    }
    if ranges.is_empty() {
        return Err("nothing left after splitting".to_string());
    }
    let mut selection = Selection::from_ranges(ranges, 0);
    selection.normalize(len);
    Ok(selection)
}

pub fn split_lines(text: &str, selection: &Selection) -> Selection {
    let len = text.chars().count();
    let chars: Vec<char> = text.chars().collect();
    let mut ranges = Vec::new();
    for range in selection.ranges() {
        let (start, end) = range.span(len);
        let mut piece = start;
        for (index, character) in chars.iter().enumerate().take(end).skip(start) {
            if *character == '\n' {
                if index > piece {
                    ranges.push(Range::covering(piece, index - 1, true));
                }
                piece = index + 1;
            }
        }
        if end > piece {
            ranges.push(Range::covering(piece, end - 1, true));
        }
    }
    if ranges.is_empty() {
        return selection.clone();
    }
    let mut next = Selection::from_ranges(ranges, 0);
    next.normalize(len);
    next
}

pub fn regex_keep(
    text: &str,
    selection: &Selection,
    pattern: &str,
    remove: bool,
) -> Result<Selection, String> {
    let regex = Regex::new(pattern).map_err(|error| error.to_string())?;
    let len = text.chars().count();
    let ranges: Vec<Range> = selection
        .ranges()
        .iter()
        .copied()
        .filter(|range| {
            let (start, end) = range.span(len);
            let slice = &text[char_to_byte(text, start)..char_to_byte(text, end)];
            let matched = regex.is_match(slice);
            if remove {
                !matched
            } else {
                matched
            }
        })
        .collect();
    if ranges.is_empty() {
        return Err("no selections left".to_string());
    }
    Ok(Selection::from_ranges(ranges, 0))
}

pub fn trim_ranges(text: &str, selection: &Selection) -> Selection {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let ranges = selection
        .ranges()
        .iter()
        .copied()
        .map(|range| {
            let (mut start, mut end) = range.span(len);
            while start < end && chars[start].is_whitespace() {
                start += 1;
            }
            while end > start && chars[end - 1].is_whitespace() {
                end -= 1;
            }
            if start >= end {
                Range::cursor(range.from().min(len))
            } else {
                Range::covering(start, end - 1, range.is_forward())
            }
        })
        .collect();
    let mut next = Selection::from_ranges(ranges, selection.primary_index());
    next.normalize(len);
    next
}

pub fn align_edits(text: &str, selection: &Selection) -> Vec<TextEdit> {
    let doc = Doc::new(text);
    // Helix `&` pads on the left so the selection ends share a column.
    let columns: Vec<(usize, usize)> = selection
        .ranges()
        .iter()
        .map(|range| {
            let (start, _) = doc.column_on_line(range.to());
            (
                range.from(),
                range.to().saturating_sub(start).saturating_add(1),
            )
        })
        .collect();
    let Some(target) = columns.iter().map(|(_, column)| *column).max() else {
        return Vec::new();
    };
    columns
        .into_iter()
        .filter_map(|(index, column)| {
            (column < target).then_some(TextEdit {
                start: index,
                end: index,
                text: " ".repeat(target - column),
            })
        })
        .collect()
}

pub fn pair_for(character: char) -> (char, char) {
    match character {
        '(' | ')' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' => ('{', '}'),
        '<' | '>' => ('<', '>'),
        other => (other, other),
    }
}

/// Edits that add, replace, or delete a one-character surround.
///
/// The returned range is the selection after the edit, in coordinates before
/// the edit is applied. Callers map it through the edits.
pub fn surround(
    text: &str,
    range: Range,
    op: SurroundOp,
    first: char,
    second: Option<char>,
) -> Result<(Vec<TextEdit>, Range), String> {
    let len = text.chars().count();
    let (start, end) = range.span(len);
    match op {
        SurroundOp::Add => {
            let (open, close) = pair_for(first);
            let edits = vec![
                TextEdit {
                    start,
                    end: start,
                    text: open.to_string(),
                },
                TextEdit {
                    start: end,
                    end,
                    text: close.to_string(),
                },
            ];
            let covered = end.saturating_sub(start);
            Ok((edits, Range::covering(start, start + covered + 1, true)))
        }
        SurroundOp::Delete => {
            let (open, close) = pair_for(first);
            let cursor = range.head.min(len.saturating_sub(1));
            let (outer_start, outer_end) =
                surrounding_pair(text, cursor, cursor.saturating_add(1), open, close)?;
            Ok((
                vec![
                    TextEdit {
                        start: outer_start,
                        end: outer_start + 1,
                        text: String::new(),
                    },
                    TextEdit {
                        start: outer_end - 1,
                        end: outer_end,
                        text: String::new(),
                    },
                ],
                Range::covering(outer_start + 1, outer_end.saturating_sub(2), true),
            ))
        }
        SurroundOp::Replace => {
            let next = second.ok_or_else(|| "replacement character required".to_string())?;
            let (open, close) = pair_for(first);
            let (new_open, new_close) = pair_for(next);
            let cursor = range.head.min(len.saturating_sub(1));
            let (outer_start, outer_end) =
                surrounding_pair(text, cursor, cursor.saturating_add(1), open, close)?;
            Ok((
                vec![
                    TextEdit {
                        start: outer_start,
                        end: outer_start + 1,
                        text: new_open.to_string(),
                    },
                    TextEdit {
                        start: outer_end - 1,
                        end: outer_end,
                        text: new_close.to_string(),
                    },
                ],
                range,
            ))
        }
    }
}

fn surrounding_pair(
    text: &str,
    start: usize,
    end: usize,
    open: char,
    close: char,
) -> Result<(usize, usize), String> {
    let chars: Vec<char> = text.chars().collect();
    let mut outer_start = None;
    let mut index = start;
    loop {
        if chars.get(index) == Some(&open) {
            outer_start = Some(index);
            break;
        }
        if index == 0 {
            break;
        }
        index -= 1;
    }
    let outer_end = chars[end.min(chars.len())..]
        .iter()
        .position(|character| *character == close)
        .map(|offset| end.min(chars.len()) + offset + 1);
    match (outer_start, outer_end) {
        (Some(start), Some(end)) if end > start + 1 => Ok((start, end)),
        _ => Err("surrounding pair not found".to_string()),
    }
}

pub fn join_span(text: &str, start: usize, end: usize) -> String {
    let slice: String = text
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    let keep_trailing_newline = slice.ends_with('\n') || slice.ends_with('\r');
    let mut joined = String::new();
    let mut pending_break = false;
    for character in slice.chars() {
        if character == '\n' || character == '\r' {
            pending_break = !joined.is_empty();
            continue;
        }
        if pending_break {
            if !joined.ends_with(' ') && !character.is_whitespace() {
                joined.push(' ');
            }
            pending_break = false;
        }
        joined.push(character);
    }
    if keep_trailing_newline {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_motion_selects_through_the_next_word_start() {
        let text = "hello world";
        let selection = Selection::cursor(0);
        let next = apply_motion(
            text,
            &selection,
            Motion::Word(WordMotion {
                forward: true,
                end: false,
                long: false,
            }),
            1,
            None,
            false,
        )
        .unwrap();
        // Helix 25.07.1: `w` covers the word and its trailing space, with the
        // cursor on that space.
        assert_eq!(next.primary(), Range::new(0, 5));
        assert_eq!(&text[..6], "hello ");
        assert!(next.invariants_hold(text));
    }

    #[test]
    fn find_crosses_lines_and_till_stops_before_the_character() {
        let text = "ab\ncdx";
        let selection = Selection::cursor(0);
        let found = apply_motion(
            text,
            &selection,
            Motion::Find(FindKind::Next),
            1,
            Some('x'),
            false,
        )
        .unwrap();
        assert_eq!(found.primary(), Range::new(0, 5));
        let till = apply_motion(
            text,
            &selection,
            Motion::Find(FindKind::TillNext),
            1,
            Some('x'),
            false,
        )
        .unwrap();
        assert_eq!(till.primary().head, 4);
        assert!(apply_motion(
            text,
            &selection,
            Motion::Find(FindKind::Next),
            1,
            Some('z'),
            false
        )
        .is_none());
    }

    #[test]
    fn extend_line_grows_down_and_select_all_covers_the_buffer() {
        let text = "one\ntwo\n";
        let first = extend_line_below(text, Range::cursor(0));
        assert_eq!(first, Range::covering(0, 3, true));
        let second = extend_line_below(text, first);
        assert_eq!(second.to(), 7);
        assert_eq!(
            select_all(text),
            Range::covering(0, text.chars().count() - 1, true)
        );
    }

    #[test]
    fn regex_select_and_split_stay_inside_the_selection() {
        let text = "one two two";
        let selection = Selection::single(select_all(text));
        let matches = regex_select(text, &selection, "two").unwrap();
        assert_eq!(matches.len(), 2);
        let pieces = regex_split(text, &selection, " ").unwrap();
        assert_eq!(pieces.len(), 3);
        assert!(pieces.invariants_hold(text));
    }

    #[test]
    fn vertical_motion_keeps_the_column_and_stops_at_the_ends() {
        let text = "abcd\nxy\n";
        assert_eq!(motion_head(text, 3, Motion::LineDown, 1, None), Some(6));
        assert_eq!(motion_head(text, 0, Motion::LineUp, 3, None), Some(0));
    }
}
