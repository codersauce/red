use regex::RegexBuilder;

use crate::{
    buffer::Buffer,
    undo::{TextPosition, TextRange},
};

/// Half-open range in document-wide Unicode scalar coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CharRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl CharRange {
    pub(crate) const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Ordered occurrences and selected ranges for one Ctrl-N session.
#[derive(Debug, Clone)]
pub(crate) struct SelectionSet {
    candidates: Vec<CharRange>,
    selections: Vec<CharRange>,
    active_candidate: usize,
}

impl SelectionSet {
    /// Selects the exact keyword run or single punctuation/whitespace scalar under `cursor`.
    pub(crate) fn from_cursor(
        buffer: &Buffer,
        cursor: TextPosition,
        ignorecase: bool,
        smartcase: bool,
    ) -> Option<Self> {
        let contents = buffer.contents();
        let characters = contents.chars().collect::<Vec<_>>();
        let cursor_index = buffer.position_to_char_idx(cursor);
        let current = *characters.get(cursor_index)?;
        if is_line_ending(current) {
            return None;
        }

        let mut start = cursor_index;
        let mut end = cursor_index + 1;
        if is_keyword_char(current) {
            while start > 0 && is_keyword_char(characters[start - 1]) {
                start -= 1;
            }
            while end < characters.len() && is_keyword_char(characters[end]) {
                end += 1;
            }
        }

        let needle = characters[start..end].iter().collect::<String>();
        let case_insensitive = ignorecase && !(smartcase && needle.chars().any(char::is_uppercase));
        let regex = RegexBuilder::new(&regex::escape(&needle))
            .case_insensitive(case_insensitive)
            .build()
            .ok()?;
        let keyword = needle.chars().all(is_keyword_char);
        let candidates = buffer
            .regex_matches(&regex)
            .into_iter()
            .filter_map(|match_| {
                let start =
                    buffer.position_to_char_idx(TextPosition::new(match_.start_y, match_.start_x));
                let end =
                    buffer.position_to_char_idx(TextPosition::new(match_.end_y, match_.end_x));
                let has_keyword_neighbor = keyword
                    && (start
                        .checked_sub(1)
                        .and_then(|index| characters.get(index))
                        .is_some_and(|character| is_keyword_char(*character))
                        || characters
                            .get(end)
                            .is_some_and(|character| is_keyword_char(*character)));
                (!has_keyword_neighbor).then_some(CharRange::new(start, end))
            })
            .collect::<Vec<_>>();
        let active_candidate = candidates
            .iter()
            .position(|candidate| *candidate == CharRange::new(start, end))?;

        Some(Self {
            candidates,
            selections: vec![CharRange::new(start, end)],
            active_candidate,
        })
    }

    /// Adds the next occurrence, wrapping at the end of the document.
    pub(crate) fn select_next(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.active_candidate = (self.active_candidate + 1) % self.candidates.len();
        let candidate = self.candidates[self.active_candidate];
        if !self.selections.contains(&candidate) {
            self.selections.push(candidate);
            self.selections.sort_by_key(|range| range.start);
        }
    }

    pub(crate) fn ranges(&self) -> &[CharRange] {
        &self.selections
    }

    pub(crate) fn active_range(&self) -> CharRange {
        self.candidates[self.active_candidate]
    }

    pub(crate) fn replace_ranges(&mut self, ranges: Vec<CharRange>, active: usize) {
        self.candidates = ranges.clone();
        self.selections = ranges;
        self.active_candidate = active.min(self.candidates.len().saturating_sub(1));
    }

    pub(crate) fn text_ranges(&self, buffer: &Buffer) -> Vec<TextRange> {
        self.selections
            .iter()
            .map(|range| {
                TextRange::new(
                    buffer.char_idx_to_position(range.start),
                    buffer.char_idx_to_position(range.end),
                )
            })
            .collect()
    }
}

fn is_keyword_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn is_line_ending(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_at(contents: &str, character: usize, case_insensitive: bool) -> SelectionSet {
        let buffer = Buffer::new(None, contents.to_string());
        SelectionSet::from_cursor(
            &buffer,
            TextPosition::new(0, character),
            case_insensitive,
            false,
        )
        .unwrap()
    }

    #[test]
    fn keyword_matches_are_whole_words_and_wrap() {
        let mut set = set_at("foo foo_bar foo", 1, false);
        assert_eq!(set.ranges(), &[CharRange::new(0, 3)]);

        set.select_next();
        assert_eq!(
            set.ranges(),
            &[CharRange::new(0, 3), CharRange::new(12, 15)]
        );
        assert_eq!(set.active_range(), CharRange::new(12, 15));

        set.select_next();
        assert_eq!(set.active_range(), CharRange::new(0, 3));
    }

    #[test]
    fn punctuation_and_whitespace_select_one_scalar() {
        let punctuation = set_at("foo.bar", 3, false);
        assert_eq!(punctuation.ranges(), &[CharRange::new(3, 4)]);

        let whitespace = set_at("foo bar", 3, false);
        assert_eq!(whitespace.ranges(), &[CharRange::new(3, 4)]);
    }

    #[test]
    fn case_insensitive_matching_preserves_ranges() {
        let mut set = set_at("foo Foo FOO", 1, true);
        set.select_next();
        set.select_next();
        assert_eq!(
            set.ranges(),
            &[
                CharRange::new(0, 3),
                CharRange::new(4, 7),
                CharRange::new(8, 11)
            ]
        );
    }

    #[test]
    fn smartcase_keeps_uppercase_needles_case_sensitive() {
        let buffer = Buffer::new(None, "Foo foo Foo".to_string());
        let mut set =
            SelectionSet::from_cursor(&buffer, TextPosition::new(0, 1), true, true).unwrap();
        set.select_next();
        assert_eq!(set.ranges(), &[CharRange::new(0, 3), CharRange::new(8, 11)]);
    }

    #[test]
    fn unicode_ranges_use_scalar_offsets() {
        let mut set = set_at("café café", 2, false);
        set.select_next();
        assert_eq!(set.ranges(), &[CharRange::new(0, 4), CharRange::new(5, 9)]);
    }
}
