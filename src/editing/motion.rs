//! Unicode-aware Vim motion and text-object resolution over an ordinary buffer.

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    buffer::Buffer,
    undo::{TextPosition, TextRange},
    unicode_utils::{char_prefix, char_suffix, trim_line_ending},
};

/// Direction and inclusion policy for Vim's single-character search motions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterMotion {
    /// `f{character}` lands on the next matching character.
    Find,
    /// `t{character}` lands immediately before the next matching character.
    Till,
    /// `F{character}` lands on the previous matching character.
    FindBackward,
    /// `T{character}` lands immediately after the previous matching character.
    TillBackward,
}

/// Whether a text object includes its surrounding delimiters or whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObjectScope {
    /// Select only the object's contents.
    Inner,
    /// Include surrounding delimiters or adjacent whitespace.
    Around,
}

/// Text objects supported consistently by editor windows and embedded text areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObjectKind {
    /// A Vim keyword or punctuation word.
    Word,
    /// A whitespace-delimited Vim WORD.
    BigWord,
    /// A sentence, including its terminating punctuation and closing delimiters.
    Sentence,
    /// A paragraph or blank-line group.
    Paragraph,
    /// The innermost surrounding delimiter pair.
    Delimited {
        /// Opening delimiter.
        open: char,
        /// Closing delimiter.
        close: char,
    },
    /// A same-line, unescaped quoted string.
    Quote(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextUnitKind {
    Keyword,
    Punctuation,
    Symbol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SentenceUnitKind {
    Text,
    Whitespace,
    Paragraph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SentenceUnit {
    start: usize,
    end: usize,
    kind: SentenceUnitKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoundaryMotion {
    position: TextPosition,
    ends_at_buffer: bool,
}

/// Resolve motions against a canonical scalar-coordinate cursor position.
#[derive(Debug, Clone, Copy)]
pub struct MotionResolver<'buffer> {
    buffer: &'buffer Buffer,
    cursor: TextPosition,
}

impl<'buffer> MotionResolver<'buffer> {
    /// Creates a resolver without borrowing editor, window, or UI state.
    #[must_use]
    pub const fn new(buffer: &'buffer Buffer, cursor: TextPosition) -> Self {
        Self { buffer, cursor }
    }

    /// Computes the range consumed by `w` or `W`, preserving Vim's `cw` semantics.
    #[must_use]
    pub fn word_range(&self, count: u16, change_word: bool, big_word: bool) -> Option<TextRange> {
        let start = self.cursor;
        let start_index = self.buffer.position_to_char_idx(start);
        let snapshot = self.buffer.contents_snapshot();
        let contents = snapshot.slice(start_index..).to_string();
        let characters = contents.chars().collect::<Vec<_>>();
        let mut end = 0;
        characters.get(end)?;
        let word_kind = |character: char| {
            if character.is_whitespace() {
                0
            } else if big_word || character.is_alphanumeric() || character == '_' {
                1
            } else {
                2
            }
        };
        let word_kinds = contents
            .graphemes(true)
            .flat_map(|grapheme| {
                let kind = word_kind(grapheme.chars().next().unwrap_or_default());
                grapheme.chars().map(move |_| kind)
            })
            .collect::<Vec<_>>();
        let preserve_trailing_whitespace = change_word
            && characters
                .get(end)
                .is_some_and(|character| !character.is_whitespace());

        for index in 0..count {
            let Some(&character) = characters.get(end) else {
                break;
            };
            let final_motion = index + 1 == count;
            if matches!(character, '\r' | '\n') {
                if final_motion && (change_word || (!big_word && index > 0)) {
                    break;
                }
                end += 1;
                if character == '\r' && characters.get(end) == Some(&'\n') {
                    end += 1;
                }
                if !final_motion {
                    while characters
                        .get(end)
                        .is_some_and(|next| next.is_whitespace() && !matches!(next, '\r' | '\n'))
                    {
                        end += 1;
                    }
                }
                continue;
            }

            let kind = word_kinds[end];
            while word_kinds.get(end) == Some(&kind)
                && characters
                    .get(end)
                    .is_some_and(|next| !matches!(next, '\r' | '\n'))
            {
                end += 1;
            }
            if !preserve_trailing_whitespace || index + 1 < count {
                while characters
                    .get(end)
                    .is_some_and(|&next| next.is_whitespace() && !matches!(next, '\r' | '\n'))
                {
                    end += 1;
                }
                if !final_motion {
                    if change_word {
                        while characters.get(end).is_some_and(|next| next.is_whitespace()) {
                            end += 1;
                        }
                    } else if let Some(&line_ending @ ('\r' | '\n')) = characters.get(end) {
                        end += 1;
                        if line_ending == '\r' && characters.get(end) == Some(&'\n') {
                            end += 1;
                        }
                        while characters.get(end).is_some_and(|next| {
                            next.is_whitespace() && !matches!(next, '\r' | '\n')
                        }) {
                            end += 1;
                        }
                    }
                }
            }
        }

        let end = self.buffer.char_idx_to_position(start_index + end);
        (start != end || change_word).then(|| TextRange::new(start, end))
    }

    /// Computes the destination of `w`, `b`, `e`, `ge`, and their WORD variants.
    #[must_use]
    pub fn word_target(
        &self,
        count: u16,
        backward: bool,
        end: bool,
        big_word: bool,
    ) -> Option<TextPosition> {
        let characters = self.buffer.contents_snapshot();
        let character_count = characters.len_chars();
        if character_count == 0 {
            return None;
        }

        let word_kind = |character: char| {
            if character.is_whitespace() {
                0
            } else if big_word || character.is_alphanumeric() || character == '_' {
                1
            } else {
                2
            }
        };
        let mut cursor = self
            .buffer
            .position_to_char_idx(self.cursor)
            .min(character_count.saturating_sub(1));
        let mut target = None;

        for _ in 0..count {
            if backward {
                if end && !characters.char(cursor).is_whitespace() {
                    let kind = word_kind(characters.char(cursor));
                    while cursor > 0 && word_kind(characters.char(cursor - 1)) == kind {
                        cursor -= 1;
                    }
                }
                if cursor == 0 {
                    break;
                }
                cursor -= 1;
                while cursor > 0 && characters.char(cursor).is_whitespace() {
                    cursor -= 1;
                }
                if characters.char(cursor).is_whitespace() {
                    break;
                }
                let found_end = cursor;
                let kind = word_kind(characters.char(cursor));
                while cursor > 0 && word_kind(characters.char(cursor - 1)) == kind {
                    cursor -= 1;
                }
                target = Some(if end { found_end } else { cursor });
            } else {
                if characters.char(cursor).is_whitespace() {
                    while cursor < character_count && characters.char(cursor).is_whitespace() {
                        cursor += 1;
                    }
                } else {
                    let kind = word_kind(characters.char(cursor));
                    let mut group_end = cursor;
                    while group_end + 1 < character_count
                        && word_kind(characters.char(group_end + 1)) == kind
                    {
                        group_end += 1;
                    }
                    if end && cursor < group_end {
                        cursor = group_end;
                        target = Some(cursor);
                        continue;
                    }
                    cursor = group_end.saturating_add(1);
                    while cursor < character_count && characters.char(cursor).is_whitespace() {
                        cursor += 1;
                    }
                }

                if cursor >= character_count {
                    break;
                }
                if end {
                    let kind = word_kind(characters.char(cursor));
                    while cursor + 1 < character_count
                        && word_kind(characters.char(cursor + 1)) == kind
                    {
                        cursor += 1;
                    }
                }
                target = Some(cursor);
            }
        }

        target.map(|index| self.buffer.char_idx_to_position(index))
    }

    /// Resolves `{` or `}`, treating only genuinely empty lines as paragraph boundaries.
    #[must_use]
    pub fn paragraph_target(&self, count: u16, backward: bool) -> Option<TextPosition> {
        self.paragraph_motion(count, backward)
            .map(|motion| motion.position)
    }

    /// Resolves `(` or `)`, including paragraph boundaries and closing punctuation.
    #[must_use]
    pub fn sentence_target(&self, count: u16, backward: bool) -> Option<TextPosition> {
        self.sentence_motion(count, backward)
            .map(|motion| motion.position)
    }

    /// Resolves an exclusive paragraph operator motion and its effective register shape.
    #[must_use]
    pub fn paragraph_range(&self, count: u16, backward: bool) -> Option<(TextRange, bool)> {
        self.boundary_motion_range(self.paragraph_motion(count, backward)?, backward)
    }

    /// Resolves an exclusive sentence operator motion and its effective register shape.
    #[must_use]
    pub fn sentence_range(&self, count: u16, backward: bool) -> Option<(TextRange, bool)> {
        self.boundary_motion_range(self.sentence_motion(count, backward)?, backward)
    }

    fn paragraph_motion(&self, count: u16, backward: bool) -> Option<BoundaryMotion> {
        if self.buffer.is_empty() {
            return None;
        }

        let last_line = self.buffer.last_navigable_line();
        let mut line = self.cursor.line.min(last_line);
        let contents = self.buffer.contents_snapshot();
        let is_empty = |candidate: usize| self.buffer.line_is_empty(candidate);

        for _ in 0..count.max(1) {
            if backward {
                if line == 0 {
                    return Some(BoundaryMotion {
                        position: TextPosition::new(0, 0),
                        ends_at_buffer: false,
                    });
                }

                let mut skip_empty = is_empty(line);
                let mut boundary = 0;
                for (offset, candidate_line) in contents.lines_at(line).reversed().enumerate() {
                    let candidate = line - offset - 1;
                    let empty = Buffer::line_slice_is_empty(candidate_line);
                    if skip_empty && empty && candidate > 0 {
                        continue;
                    }
                    skip_empty = false;
                    if empty || candidate == 0 {
                        boundary = candidate;
                        break;
                    }
                }
                line = boundary;
            } else {
                let mut skip_empty = is_empty(line);
                let mut boundary = None;
                for (offset, candidate_line) in contents.lines_at(line + 1).enumerate() {
                    let candidate = line + offset + 1;
                    if candidate > last_line {
                        break;
                    }
                    let empty = Buffer::line_slice_is_empty(candidate_line);
                    if skip_empty && empty {
                        continue;
                    }
                    skip_empty = false;
                    if empty {
                        boundary = Some(candidate);
                        break;
                    }
                }
                let Some(boundary) = boundary else {
                    return Some(BoundaryMotion {
                        position: self.last_cursor_position(),
                        ends_at_buffer: true,
                    });
                };
                line = boundary;
            }
        }

        Some(BoundaryMotion {
            position: TextPosition::new(line, 0),
            ends_at_buffer: false,
        })
    }

    fn sentence_motion(&self, count: u16, backward: bool) -> Option<BoundaryMotion> {
        if self.buffer.is_empty() {
            return None;
        }

        let mut cursor = self.buffer.position_to_char_idx(self.cursor);
        if !backward && count <= 1 && self.cursor.line == self.buffer.last_navigable_line() {
            let contents = self.buffer.contents_snapshot();
            let mut remaining = contents.chars_at(cursor);
            if remaining.next().is_some_and(|character| {
                !character.is_whitespace() && !matches!(character, '.' | '!' | '?')
            }) && remaining.all(|character| !matches!(character, '.' | '!' | '?' | '\n' | '\r'))
            {
                return Some(BoundaryMotion {
                    position: self.last_cursor_position(),
                    ends_at_buffer: true,
                });
            }
        }
        let units = self.sentence_units();

        for _ in 0..count.max(1) {
            if backward {
                let mut index = units.partition_point(|unit| unit.start < cursor);
                while index > 0 && units[index - 1].kind == SentenceUnitKind::Whitespace {
                    index -= 1;
                }
                let Some(previous) = index.checked_sub(1).and_then(|index| units.get(index)) else {
                    return Some(BoundaryMotion {
                        position: TextPosition::new(0, 0),
                        ends_at_buffer: false,
                    });
                };
                cursor = previous.start;
            } else {
                let mut index = units.partition_point(|unit| unit.start <= cursor);
                while units
                    .get(index)
                    .is_some_and(|unit| unit.kind == SentenceUnitKind::Whitespace)
                {
                    index += 1;
                }
                let Some(next) = units.get(index) else {
                    return Some(BoundaryMotion {
                        position: self.last_cursor_position(),
                        ends_at_buffer: true,
                    });
                };
                cursor = next.start;
            }
        }

        Some(BoundaryMotion {
            position: self.buffer.char_idx_to_position(cursor),
            ends_at_buffer: false,
        })
    }

    fn boundary_motion_range(
        &self,
        motion: BoundaryMotion,
        backward: bool,
    ) -> Option<(TextRange, bool)> {
        let cursor = self.cursor;
        let target = motion.position;
        let first_non_blank = self
            .buffer
            .get(cursor.line)
            .map(|line| {
                trim_line_ending(&line)
                    .chars()
                    .position(|character| !character.is_whitespace())
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        if backward {
            if cursor == target {
                return None;
            }
            let linewise = target.line < cursor.line
                && target.character == 0
                && cursor.character <= first_non_blank;
            return Some((TextRange::new(target, cursor), linewise));
        }

        if motion.ends_at_buffer {
            let end = self.buffer.char_idx_to_position(self.buffer.char_len());
            if cursor == end {
                return None;
            }
            let linewise = target.line > cursor.line && cursor.character <= first_non_blank;
            let start = if linewise {
                TextPosition::new(cursor.line, 0)
            } else {
                cursor
            };
            return Some((TextRange::new(start, end), linewise));
        }

        if cursor == target {
            return None;
        }
        if target.line > cursor.line && target.character == 0 {
            if cursor.character <= first_non_blank {
                return Some((
                    TextRange::new(TextPosition::new(cursor.line, 0), target),
                    true,
                ));
            }
            let previous_line = target.line - 1;
            let end = TextPosition::new(previous_line, self.line_character_len(previous_line));
            return (cursor != end).then(|| (TextRange::new(cursor, end), false));
        }

        Some((TextRange::new(cursor, target), false))
    }

    fn last_cursor_position(&self) -> TextPosition {
        let line = self.buffer.last_navigable_line();
        let Some(contents) = self.buffer.get(line) else {
            return TextPosition::new(line, 0);
        };
        let contents = trim_line_ending(&contents);
        let character = contents.graphemes(true).next_back().map_or(0, |grapheme| {
            contents.chars().count() - grapheme.chars().count()
        });
        TextPosition::new(line, character)
    }

    /// Finds the requested character on the current logical line.
    #[must_use]
    pub fn character_match(
        &self,
        character: char,
        count: u16,
        backward: bool,
    ) -> Option<TextPosition> {
        let line = self.buffer.get(self.cursor.line)?;
        let line = trim_line_ending(&line);
        if backward {
            let prefix = char_prefix(line, self.cursor.character);
            let target_byte = prefix
                .match_indices(character)
                .rev()
                .nth(usize::from(count.saturating_sub(1)))?
                .0;
            return Some(TextPosition::new(
                self.cursor.line,
                prefix[..target_byte].chars().count(),
            ));
        }

        let search_start = self.cursor.character.saturating_add(1);
        let offset = char_suffix(line, search_start)
            .chars()
            .enumerate()
            .filter_map(|(offset, candidate)| (candidate == character).then_some(offset))
            .nth(usize::from(count.saturating_sub(1)))?;
        Some(TextPosition::new(self.cursor.line, search_start + offset))
    }

    /// Resolves a supported Vim text object in canonical scalar coordinates.
    #[must_use]
    pub fn text_object(&self, scope: TextObjectScope, kind: TextObjectKind) -> Option<TextRange> {
        self.text_object_with_count(scope, kind, 1)
    }

    /// Resolves a text object, honoring sentence-object counts without changing legacy objects.
    #[must_use]
    pub fn text_object_with_count(
        &self,
        scope: TextObjectScope,
        kind: TextObjectKind,
        count: u16,
    ) -> Option<TextRange> {
        match kind {
            TextObjectKind::Word => self.word_text_object(scope, false),
            TextObjectKind::BigWord => self.word_text_object(scope, true),
            TextObjectKind::Sentence => self.sentence_text_object(scope, count),
            TextObjectKind::Paragraph => self.paragraph_text_object(scope),
            TextObjectKind::Delimited { open, close } => {
                self.delimited_text_object(scope, open, close)
            }
            TextObjectKind::Quote(quote) => self.quote_text_object(scope, quote),
        }
    }

    fn word_text_object(&self, scope: TextObjectScope, big_word: bool) -> Option<TextRange> {
        let line_index = self.cursor.line;
        let line = self.buffer.get(line_index)?;
        let chars = trim_line_ending(&line).chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return None;
        }

        let unit_kinds = trim_line_ending(&line)
            .graphemes(true)
            .flat_map(|grapheme| {
                let first = grapheme.chars().next().unwrap_or_default();
                let kind = if big_word && !first.is_whitespace() {
                    Some(TextUnitKind::Keyword)
                } else {
                    text_unit_kind(first)
                };
                grapheme.chars().map(move |_| kind)
            })
            .collect::<Vec<_>>();
        let unit_kind = |index: usize| unit_kinds.get(index).copied().flatten();
        let cursor = self.cursor.character.min(chars.len().saturating_sub(1));
        let target = if unit_kind(cursor).is_some() {
            cursor
        } else {
            (cursor..chars.len())
                .find(|index| unit_kind(*index).is_some())
                .or_else(|| (0..=cursor).rev().find(|index| unit_kind(*index).is_some()))?
        };

        let kind = unit_kind(target)?;
        let mut start = target;
        while start > 0 && unit_kind(start - 1) == Some(kind) {
            start -= 1;
        }
        let mut end = target + 1;
        while end < chars.len() && unit_kind(end) == Some(kind) {
            end += 1;
        }

        if scope == TextObjectScope::Around {
            if end < chars.len() && chars[end].is_whitespace() {
                while end < chars.len() && chars[end].is_whitespace() {
                    end += 1;
                }
            } else {
                while start > 0 && chars[start - 1].is_whitespace() {
                    start -= 1;
                }
            }
        }

        Some(TextRange::new(
            TextPosition::new(line_index, start),
            TextPosition::new(line_index, end),
        ))
    }

    fn sentence_text_object(&self, scope: TextObjectScope, count: u16) -> Option<TextRange> {
        let units = self.sentence_units();
        let cursor = self.buffer.position_to_char_idx(self.cursor);
        let current = units
            .iter()
            .position(|unit| unit.start <= cursor && cursor < unit.end)?;
        let mut first = current;
        let mut last = current;

        if scope == TextObjectScope::Inner {
            for _ in 1..count.max(1) {
                let Some(next) = units.get(last + 1) else {
                    break;
                };
                if next.kind == SentenceUnitKind::Paragraph
                    && units[current].kind != SentenceUnitKind::Paragraph
                {
                    break;
                }
                last += 1;
            }
        } else if units[current].kind == SentenceUnitKind::Whitespace {
            for _ in 0..count.max(1) {
                if units
                    .get(last + 1)
                    .is_some_and(|unit| unit.kind != SentenceUnitKind::Whitespace)
                {
                    last += 1;
                }
                if last + 1 < units.len() && count > 1 {
                    last += 1;
                }
            }
        } else {
            for index in 0..count.max(1) {
                if index > 0 {
                    let Some(next) = units.get(last + 1) else {
                        break;
                    };
                    if next.kind != SentenceUnitKind::Whitespace {
                        last += 1;
                    }
                }
                if units
                    .get(last + 1)
                    .is_some_and(|unit| unit.kind == SentenceUnitKind::Whitespace)
                {
                    last += 1;
                }
            }
            if last + 1 == units.len()
                && first > 0
                && units[first - 1].kind == SentenceUnitKind::Whitespace
            {
                first -= 1;
            }
        }

        Some(TextRange::new(
            self.buffer.char_idx_to_position(units[first].start),
            self.buffer.char_idx_to_position(units[last].end),
        ))
    }

    fn sentence_units(&self) -> Vec<SentenceUnit> {
        let snapshot = self.buffer.contents_snapshot();
        let contents = snapshot.to_string();
        let characters = contents.chars().collect::<Vec<_>>();
        if characters.is_empty() {
            return Vec::new();
        }

        let mut char_index = 0;
        let paragraph_boundaries = snapshot
            .lines()
            .take(self.buffer.last_navigable_line() + 1)
            .filter_map(|line| {
                let start = char_index;
                char_index += line.len_chars();
                Buffer::line_slice_is_empty(line).then_some(start)
            })
            .collect::<Vec<_>>();
        let mut units = Vec::with_capacity(paragraph_boundaries.len().saturating_mul(4));
        let mut start = 0;
        let mut paragraph_index = 0;

        while start < characters.len() {
            while paragraph_boundaries
                .get(paragraph_index)
                .is_some_and(|boundary| *boundary < start)
            {
                paragraph_index += 1;
            }
            if paragraph_boundaries.get(paragraph_index) == Some(&start) {
                let mut end = start;
                while end < characters.len()
                    && paragraph_boundaries.get(paragraph_index) == Some(&end)
                {
                    if characters[end] == '\r' {
                        end += 1;
                    }
                    if characters.get(end) == Some(&'\n') {
                        end += 1;
                        paragraph_index += 1;
                    } else {
                        break;
                    }
                }
                if end == start {
                    break;
                }
                units.push(SentenceUnit {
                    start,
                    end,
                    kind: SentenceUnitKind::Paragraph,
                });
                start = end;
                continue;
            }

            let boundary = paragraph_boundaries
                .get(paragraph_index)
                .copied()
                .unwrap_or(characters.len());
            let mut end = boundary;
            for index in start..boundary {
                if !matches!(characters[index], '.' | '!' | '?') {
                    continue;
                }
                let mut candidate = index + 1;
                while matches!(characters.get(candidate), Some(')' | ']' | '"' | '\'')) {
                    candidate += 1;
                }
                if candidate < characters.len()
                    && !matches!(characters[candidate], ' ' | '\t' | '\r' | '\n')
                {
                    continue;
                }
                end = candidate;
                if characters.get(end) == Some(&'\r') {
                    end += 1;
                }
                if characters.get(end) == Some(&'\n') {
                    end += 1;
                }
                break;
            }

            units.push(SentenceUnit {
                start,
                end,
                kind: SentenceUnitKind::Text,
            });
            start = end;
            while start < characters.len()
                && characters[start].is_whitespace()
                && paragraph_boundaries.get(paragraph_index) != Some(&start)
            {
                start += 1;
            }
            if start > end {
                units.push(SentenceUnit {
                    start: end,
                    end: start,
                    kind: SentenceUnitKind::Whitespace,
                });
            }
        }

        units
    }

    fn paragraph_text_object(&self, scope: TextObjectScope) -> Option<TextRange> {
        if self.buffer.is_empty() {
            return None;
        }

        let last_line = self.buffer.last_navigable_line();
        let cursor_line = self.cursor.line.min(last_line);
        let is_blank = |line: usize| {
            self.buffer
                .get(line)
                .is_some_and(|text| trim_line_ending(&text).trim().is_empty())
        };
        let mut first_line = cursor_line;
        let mut last_exclusive = cursor_line + 1;
        let cursor_is_blank = is_blank(cursor_line);

        while first_line > 0 && is_blank(first_line - 1) == cursor_is_blank {
            first_line -= 1;
        }
        while last_exclusive <= last_line && is_blank(last_exclusive) == cursor_is_blank {
            last_exclusive += 1;
        }
        if scope == TextObjectScope::Around {
            if cursor_is_blank {
                while last_exclusive <= last_line && !is_blank(last_exclusive) {
                    last_exclusive += 1;
                }
            } else {
                while last_exclusive <= last_line && is_blank(last_exclusive) {
                    last_exclusive += 1;
                }
            }
        }

        let ends_at_eof = last_exclusive > last_line;
        let start = if ends_at_eof && first_line > 0 {
            if scope == TextObjectScope::Around {
                while first_line > 0 && is_blank(first_line - 1) {
                    first_line -= 1;
                }
                if first_line > 0 {
                    let previous_line = first_line - 1;
                    TextPosition::new(previous_line, self.line_character_len(previous_line))
                } else {
                    TextPosition::new(0, 0)
                }
            } else {
                TextPosition::new(first_line - 1, self.line_character_len(first_line - 1))
            }
        } else {
            TextPosition::new(first_line, 0)
        };
        let end = if ends_at_eof {
            TextPosition::new(last_line, self.line_character_len(last_line))
        } else {
            TextPosition::new(last_exclusive, 0)
        };
        (start != end).then(|| TextRange::new(start, end))
    }

    fn delimited_text_object(
        &self,
        scope: TextObjectScope,
        open: char,
        close: char,
    ) -> Option<TextRange> {
        let characters = self.buffer.contents().chars().collect::<Vec<_>>();
        let cursor = self.buffer.position_to_char_idx(self.cursor);
        let mut stack = Vec::new();
        let mut best_pair = None;

        for (index, character) in characters.iter().copied().enumerate() {
            if character == open {
                stack.push(index);
            } else if character == close {
                let Some(open_index) = stack.pop() else {
                    continue;
                };
                if open_index <= cursor
                    && cursor <= index
                    && best_pair.is_none_or(|(best_open_index, _)| open_index > best_open_index)
                {
                    best_pair = Some((open_index, index));
                }
            }
        }

        let (open_index, close_index) = best_pair?;
        let (start, end) = match scope {
            TextObjectScope::Inner => (open_index + 1, close_index),
            TextObjectScope::Around => (open_index, close_index + 1),
        };
        Some(TextRange::new(
            self.buffer.char_idx_to_position(start),
            self.buffer.char_idx_to_position(end),
        ))
    }

    fn quote_text_object(&self, scope: TextObjectScope, quote: char) -> Option<TextRange> {
        let line = self.buffer.get(self.cursor.line)?;
        let characters = trim_line_ending(&line).chars().collect::<Vec<_>>();
        let quote_positions = characters
            .iter()
            .enumerate()
            .filter_map(|(index, character)| {
                (*character == quote && !is_escaped_quote(&characters, index)).then_some(index)
            })
            .collect::<Vec<_>>();

        for pair in quote_positions.chunks(2) {
            if let [start, end] = pair {
                if *start <= self.cursor.character && self.cursor.character <= *end {
                    let (start, end) = match scope {
                        TextObjectScope::Inner => (start + 1, *end),
                        TextObjectScope::Around => (*start, end + 1),
                    };
                    return Some(TextRange::new(
                        TextPosition::new(self.cursor.line, start),
                        TextPosition::new(self.cursor.line, end),
                    ));
                }
            }
        }
        None
    }

    fn line_character_len(&self, line: usize) -> usize {
        self.buffer
            .get(line)
            .map(|contents| trim_line_ending(&contents).chars().count())
            .unwrap_or_default()
    }
}

pub(crate) fn text_object_kind_for_key(character: char) -> Option<TextObjectKind> {
    match character {
        'w' => Some(TextObjectKind::Word),
        'W' => Some(TextObjectKind::BigWord),
        's' => Some(TextObjectKind::Sentence),
        'p' => Some(TextObjectKind::Paragraph),
        '(' | ')' | 'b' => Some(TextObjectKind::Delimited {
            open: '(',
            close: ')',
        }),
        '[' | ']' => Some(TextObjectKind::Delimited {
            open: '[',
            close: ']',
        }),
        '{' | '}' | 'B' => Some(TextObjectKind::Delimited {
            open: '{',
            close: '}',
        }),
        '<' | '>' => Some(TextObjectKind::Delimited {
            open: '<',
            close: '>',
        }),
        '"' | 'q' => Some(TextObjectKind::Quote('"')),
        '\'' | '`' => Some(TextObjectKind::Quote(character)),
        _ => None,
    }
}

fn text_unit_kind(character: char) -> Option<TextUnitKind> {
    if character.is_whitespace() {
        None
    } else if character.is_alphanumeric() || character == '_' {
        Some(TextUnitKind::Keyword)
    } else if character.is_ascii_punctuation() {
        Some(TextUnitKind::Punctuation)
    } else {
        Some(TextUnitKind::Symbol)
    }
}

fn is_escaped_quote(characters: &[char], index: usize) -> bool {
    let mut slash_count = 0;
    let mut previous = index;
    while previous > 0 {
        previous -= 1;
        if characters[previous] != '\\' {
            break;
        }
        slash_count += 1;
    }
    slash_count % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::{MotionResolver, TextObjectKind, TextObjectScope};
    use crate::{buffer::Buffer, undo::TextPosition};

    #[test]
    fn shared_word_motion_preserves_change_whitespace_and_grapheme_groups() {
        let buffer = Buffer::new(None, "e\u{301}cho,  next".to_string());
        let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 0));

        assert_eq!(
            buffer.text_in_range(resolver.word_range(1, true, false).unwrap()),
            "e\u{301}cho"
        );
        assert_eq!(
            buffer.text_in_range(resolver.word_range(1, false, false).unwrap()),
            "e\u{301}cho"
        );
        assert_eq!(
            buffer.text_in_range(resolver.word_range(1, false, true).unwrap()),
            "e\u{301}cho,  "
        );
    }

    #[test]
    fn shared_word_motions_preserve_large_unicode_rope_offsets() {
        let prefix = "ordinary_identifier ".repeat(512);
        let offset = prefix.len();
        let buffer = Buffer::new(None, format!("{prefix}e\u{301}cho,  終わり"));
        let resolver = MotionResolver::new(&buffer, TextPosition::new(0, offset));

        assert_eq!(
            buffer.text_in_range(resolver.word_range(1, true, false).unwrap()),
            "e\u{301}cho"
        );
        assert_eq!(
            resolver.word_target(1, false, false, true),
            Some(TextPosition::new(0, offset + 8))
        );
        assert_eq!(
            MotionResolver::new(&buffer, TextPosition::new(0, offset + 8))
                .word_target(1, true, false, true),
            Some(TextPosition::new(0, offset))
        );
    }

    #[test]
    fn paragraph_motions_stop_on_empty_lines_but_ignore_whitespace_only_lines() {
        let buffer = Buffer::new(None, "alpha\n   \nbeta\n\ngamma\n\n\ndelta".to_string());
        let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 2));

        assert_eq!(
            resolver.paragraph_target(1, false),
            Some(TextPosition::new(3, 0))
        );
        assert_eq!(
            resolver.paragraph_target(2, false),
            Some(TextPosition::new(5, 0))
        );
        assert_eq!(
            resolver.paragraph_target(3, false),
            Some(TextPosition::new(7, 4))
        );

        let resolver = MotionResolver::new(&buffer, TextPosition::new(7, 2));
        assert_eq!(
            resolver.paragraph_target(1, true),
            Some(TextPosition::new(6, 0))
        );
        assert_eq!(
            resolver.paragraph_target(2, true),
            Some(TextPosition::new(3, 0))
        );
    }

    #[test]
    fn sentence_motions_include_closers_paragraph_boundaries_and_unicode() {
        let buffer = Buffer::new(None, "Olá.)\"  👨‍👩‍👧 e\u{301}lan!\n\nNext? Final".to_string());
        let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 0));

        assert_eq!(
            resolver.sentence_target(1, false),
            Some(TextPosition::new(0, 8))
        );
        assert_eq!(
            resolver.sentence_target(2, false),
            Some(TextPosition::new(1, 0))
        );
        assert_eq!(
            resolver.sentence_target(3, false),
            Some(TextPosition::new(2, 0))
        );
        assert_eq!(
            resolver.sentence_target(4, false),
            Some(TextPosition::new(2, 6))
        );
    }

    #[test]
    fn sentence_objects_distinguish_inner_text_around_whitespace_and_counts() {
        let buffer = Buffer::new(None, "One.  Two! Three?".to_string());
        let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 0));

        for (scope, count, expected) in [
            (TextObjectScope::Inner, 1, "One."),
            (TextObjectScope::Inner, 2, "One.  "),
            (TextObjectScope::Inner, 3, "One.  Two!"),
            (TextObjectScope::Around, 1, "One.  "),
            (TextObjectScope::Around, 2, "One.  Two! "),
        ] {
            let range = resolver
                .text_object_with_count(scope, TextObjectKind::Sentence, count)
                .unwrap();
            assert_eq!(buffer.text_in_range(range), expected);
        }

        let whitespace = MotionResolver::new(&buffer, TextPosition::new(0, 4));
        assert_eq!(
            buffer.text_in_range(
                whitespace
                    .text_object(TextObjectScope::Inner, TextObjectKind::Sentence)
                    .unwrap()
            ),
            "  "
        );
        assert_eq!(
            buffer.text_in_range(
                whitespace
                    .text_object(TextObjectScope::Around, TextObjectKind::Sentence)
                    .unwrap()
            ),
            "  Two!"
        );
    }

    #[test]
    fn boundary_operator_motions_apply_exclusive_linewise_rules() {
        let buffer = Buffer::new(None, "  alpha\n\nbeta".to_string());
        let line_start = MotionResolver::new(&buffer, TextPosition::new(0, 2));
        let (range, linewise) = line_start.paragraph_range(1, false).unwrap();
        assert!(linewise);
        assert_eq!(buffer.text_in_range(range), "  alpha\n");

        let mid_line = MotionResolver::new(&buffer, TextPosition::new(0, 4));
        let (range, linewise) = mid_line.paragraph_range(1, false).unwrap();
        assert!(!linewise);
        assert_eq!(buffer.text_in_range(range), "pha");

        let at_end = MotionResolver::new(&buffer, TextPosition::new(2, 3));
        let (range, linewise) = at_end.sentence_range(1, false).unwrap();
        assert!(!linewise);
        assert_eq!(buffer.text_in_range(range), "a");
    }

    #[test]
    fn final_sentence_ranges_preserve_unicode_whitespace_and_punctuation_boundaries() {
        for (text, character, expected) in [
            ("first.\n\nfinal words", 0, "final words"),
            ("first.\n\n  final words", 2, "final words"),
            ("first.\n\n漢字 👋 tail", 0, "漢字 👋 tail"),
            ("first.\n\nfinal. another", 0, "final. "),
        ] {
            let buffer = Buffer::new(None, text.to_string());
            let line = buffer.last_navigable_line();
            let resolver = MotionResolver::new(&buffer, TextPosition::new(line, character));
            let (range, _) = resolver.sentence_range(1, false).unwrap();
            assert_eq!(buffer.text_in_range(range), expected, "{text:?}");
        }

        let buffer = Buffer::new(None, "first.\n\n  final words".to_string());
        let whitespace = MotionResolver::new(&buffer, TextPosition::new(2, 0));
        let (range, _) = whitespace.sentence_range(1, false).unwrap();
        assert_eq!(buffer.text_in_range(range), "  final words");
    }

    #[test]
    fn shared_text_objects_choose_innermost_pair_and_ignore_escaped_quotes() {
        let buffer = Buffer::new(None, "outer (before (inside) after)".to_string());
        let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 17));
        let range = resolver
            .text_object(
                TextObjectScope::Inner,
                TextObjectKind::Delimited {
                    open: '(',
                    close: ')',
                },
            )
            .unwrap();
        assert_eq!(buffer.text_in_range(range), "inside");

        let quoted = Buffer::new(None, "prefix \"a \\\" quote\" suffix".to_string());
        let resolver = MotionResolver::new(&quoted, TextPosition::new(0, 10));
        let range = resolver
            .text_object(TextObjectScope::Inner, TextObjectKind::Quote('"'))
            .unwrap();
        assert_eq!(quoted.text_in_range(range), "a \\\" quote");
    }
}
