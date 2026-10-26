//! Unicode-aware Vim motion and text-object resolution over an ordinary buffer.

use crate::{
    buffer::Buffer,
    undo::{TextPosition, TextRange},
};
pub use vimlet::{CharacterMotion, TextObjectKind, TextObjectScope};

fn word_motion(destination: vimlet::WordDestination, big_word: bool) -> vimlet::Motion {
    let class = if big_word {
        vimlet::WordClass::BigWord
    } else {
        vimlet::WordClass::Word
    };
    vimlet::Motion::Word { destination, class }
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
        self.word_operator_range(count, change_word, big_word)
            .map(|(range, _)| range)
    }

    pub(crate) fn word_operator_range(
        &self,
        count: u16,
        change_word: bool,
        big_word: bool,
    ) -> Option<(TextRange, bool)> {
        let motion = word_motion(vimlet::WordDestination::NextStart, big_word);
        let operator = if change_word {
            vimlet::Operator::Change
        } else {
            vimlet::Operator::Delete
        };
        self.operator_range(motion, count, operator)
    }

    fn operator_range(
        &self,
        motion: vimlet::Motion,
        count: u16,
        operator: vimlet::Operator,
    ) -> Option<(TextRange, bool)> {
        let rope = self.buffer.contents_snapshot();
        let resolver = vimlet::MotionResolver::new(
            &rope,
            vimlet::ByteOffset(self.buffer.position_to_byte_idx(self.cursor)),
        );
        let target = resolver.operator_range(motion, usize::from(count), operator)?;
        let range = target.ranges.first()?;
        Some((
            TextRange::new(
                self.buffer.byte_idx_to_position(range.start.0)?,
                self.buffer.byte_idx_to_position(range.end.0)?,
            ),
            target.shape == vimlet::Shape::Line,
        ))
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
        use vimlet::WordDestination::{NextEnd, NextStart, PreviousEnd, PreviousStart};

        let destination = match (backward, end) {
            (false, false) => NextStart,
            (false, true) => NextEnd,
            (true, false) => PreviousStart,
            (true, true) => PreviousEnd,
        };
        let rope = self.buffer.contents_snapshot();
        let cursor = vimlet::ByteOffset(self.buffer.position_to_byte_idx(self.cursor));
        let resolver = vimlet::MotionResolver::new(&rope, cursor);
        let target = resolver.resolve(word_motion(destination, big_word), usize::from(count))?;
        let position = resolver.normal_cursor(target.position.0);
        (position != cursor.0)
            .then(|| self.buffer.byte_idx_to_position(position))
            .flatten()
    }

    /// Resolves `{` or `}`, treating only genuinely empty lines as paragraph boundaries.
    #[must_use]
    pub fn paragraph_target(&self, count: u16, backward: bool) -> Option<TextPosition> {
        self.boundary_target(vimlet::Motion::Paragraph { backward }, count)
    }

    /// Resolve a sentence boundary in Red's scalar coordinates.
    #[must_use]
    pub fn sentence_target(&self, count: u16, backward: bool) -> Option<TextPosition> {
        self.boundary_target(vimlet::Motion::Sentence { backward }, count)
    }

    fn boundary_target(&self, motion: vimlet::Motion, count: u16) -> Option<TextPosition> {
        let rope = self.buffer.contents_snapshot();
        let resolver = vimlet::MotionResolver::new(
            &rope,
            vimlet::ByteOffset(self.buffer.position_to_byte_idx(self.cursor)),
        );
        let target = resolver.resolve(motion, usize::from(count))?;
        let last = self.buffer.last_navigable_line();
        let end = vimlet::TextSource::line_end(&rope, rope.line_to_byte(last));
        self.buffer
            .byte_idx_to_position(resolver.normal_cursor(target.position.0.min(end)))
    }

    /// Resolve a paragraph operator and preserve its register shape.
    #[must_use]
    pub fn paragraph_range(&self, count: u16, backward: bool) -> Option<(TextRange, bool)> {
        self.operator_range(
            vimlet::Motion::Paragraph { backward },
            count,
            vimlet::Operator::Delete,
        )
    }

    /// Resolve a sentence operator and preserve its register shape.
    #[must_use]
    pub fn sentence_range(&self, count: u16, backward: bool) -> Option<(TextRange, bool)> {
        self.operator_range(
            vimlet::Motion::Sentence { backward },
            count,
            vimlet::Operator::Delete,
        )
    }

    /// Finds the requested character on the current logical line.
    #[must_use]
    pub fn character_match(
        &self,
        character: char,
        count: u16,
        backward: bool,
    ) -> Option<TextPosition> {
        let contents = self.buffer.contents_snapshot();
        let source = contents.get_line(self.cursor.line)?;
        let mut length = source.len_chars();
        if source.get_char(length.saturating_sub(1)) == Some('\n') {
            length -= 1;
        }
        if source.get_char(length.saturating_sub(1)) == Some('\r') {
            length -= 1;
        }
        let line = source.slice(..length);
        if backward {
            let end = self.cursor.character.min(length);
            let offset = line
                .chars_at(end)
                .reversed()
                .enumerate()
                .filter_map(|(offset, candidate)| (candidate == character).then_some(offset))
                .nth(usize::from(count.saturating_sub(1)))?;
            return Some(TextPosition::new(self.cursor.line, end - offset - 1));
        }

        let search_start = self.cursor.character.saturating_add(1).min(length);
        let offset = line
            .chars_at(search_start)
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

    /// Resolve a counted Vim text object and convert its byte endpoints to scalar positions.
    #[must_use]
    pub fn text_object_with_count(
        &self,
        scope: TextObjectScope,
        kind: TextObjectKind,
        count: u16,
    ) -> Option<TextRange> {
        let rope = self.buffer.contents_snapshot();
        let resolver = vimlet::MotionResolver::new(
            &rope,
            vimlet::ByteOffset(self.buffer.position_to_byte_idx(self.cursor)),
        );
        let byte = self.buffer.position_to_byte_idx(self.cursor);
        let start = resolver.previous(byte);
        let resolver = if byte > 0 && resolver.next(start) > byte {
            vimlet::MotionResolver::new(&rope, vimlet::ByteOffset(start))
        } else {
            resolver
        };
        let target = resolver.text_object(scope, kind, usize::from(count))?;
        let range = target.ranges.first()?;
        Some(TextRange::new(
            self.buffer.byte_idx_to_position(range.start.0)?,
            self.buffer.byte_idx_to_position(range.end.0)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{MotionResolver, TextObjectKind, TextObjectScope};
    use crate::{buffer::Buffer, undo::TextPosition};
    use unicode_segmentation::UnicodeSegmentation;

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
    fn indexed_character_search_preserves_unicode_counts_directions_and_line_endings() {
        for text in [
            "aXaXaX trailing",
            "λ終λ終λ終 source",
            "e\u{301}cho 👋 e\u{301}clair\r\nsecond aXaX",
            "\r\nnext",
            "",
        ] {
            let buffer = Buffer::new(None, text.to_string());
            for line in [0, 1, 2, 9] {
                let source = buffer.get(line);
                let characters = source
                    .as_deref()
                    .map(crate::unicode_utils::trim_line_ending)
                    .map(|line| line.chars().collect::<Vec<_>>());
                for cursor in [0, 1, 2, 5, 12, usize::MAX] {
                    let resolver = MotionResolver::new(&buffer, TextPosition::new(line, cursor));
                    for target in ['a', 'X', '終', '👋', '\n', '\r', '\u{301}'] {
                        for count in [0_u16, 1, 2, 3, 7] {
                            for backward in [false, true] {
                                let expected = characters.as_ref().and_then(|characters| {
                                    let matching = usize::from(count.saturating_sub(1));
                                    if backward {
                                        characters
                                            .iter()
                                            .take(cursor.min(characters.len()))
                                            .enumerate()
                                            .rev()
                                            .filter_map(|(index, candidate)| {
                                                (*candidate == target).then_some(index)
                                            })
                                            .nth(matching)
                                    } else {
                                        let start = cursor.saturating_add(1).min(characters.len());
                                        characters
                                            .iter()
                                            .enumerate()
                                            .skip(start)
                                            .filter_map(|(index, candidate)| {
                                                (*candidate == target).then_some(index)
                                            })
                                            .nth(matching)
                                    }
                                });
                                assert_eq!(
                                    resolver
                                        .character_match(target, count, backward)
                                        .map(|position| (position.line, position.character)),
                                    expected.map(|character| (line, character)),
                                    "{text:?} line={line} cursor={cursor} target={target:?} count={count} backward={backward}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn indexed_final_cursor_preserves_unicode_graphemes_crlf_and_motion_boundaries() {
        for source in [
            "ordinary final source",
            "first\r\nfinal\r\n",
            "first\nfinal e\u{301}",
            "first\nfinal 👨‍👩‍👧",
            "first\nfinal 🇧🇷",
            "first\nfinal 終",
        ] {
            let buffer = Buffer::new(None, source.to_string());
            let line = buffer.last_navigable_line();
            let text = buffer.get(line).unwrap();
            let text = crate::unicode_utils::trim_line_ending(&text);
            let expected = text.graphemes(true).next_back().map_or(0, |grapheme| {
                text.chars().count() - grapheme.chars().count()
            });
            let resolver = MotionResolver::new(&buffer, TextPosition::new(line, expected));
            assert_eq!(
                resolver.paragraph_target(/*count*/ 1, /*backward*/ false),
                Some(TextPosition::new(line, expected)),
                "paragraph {source:?}"
            );
            assert_eq!(
                resolver.sentence_target(/*count*/ 1, /*backward*/ false),
                Some(TextPosition::new(line, expected)),
                "sentence {source:?}"
            );
        }

        let empty = Buffer::new(None, String::new());
        let resolver = MotionResolver::new(&empty, TextPosition::new(/*line*/ 0, /*character*/ 0));
        assert_eq!(
            resolver.sentence_target(1, false),
            Some(TextPosition::new(/*line*/ 0, /*character*/ 0))
        );
    }

    #[test]
    fn indexed_quote_objects_preserve_escapes_across_ascii_and_unicode_rope_chunks() {
        for prefix in ["a", "λ"] {
            for width in [0, 511, 512, 1023, 1024, 2047, 2048] {
                for slash_count in 0..=3 {
                    let leading = prefix.repeat(width);
                    let source = format!(
                        "{leading}\"selected {}\" tail\" remainder\r\n",
                        "\\".repeat(slash_count)
                    );
                    let buffer = Buffer::new(None, source.clone());
                    let line = crate::unicode_utils::trim_line_ending(&source);
                    let characters = line.chars().collect::<Vec<_>>();
                    let cursor = leading.chars().count().saturating_add(1);
                    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, cursor));
                    let quotes = characters
                        .iter()
                        .enumerate()
                        .filter_map(|(index, character)| {
                            if *character != '"' {
                                return None;
                            }
                            let escaped = characters[..index]
                                .iter()
                                .rev()
                                .take_while(|value| **value == '\\')
                                .count();
                            escaped.is_multiple_of(2).then_some(index)
                        })
                        .collect::<Vec<_>>();
                    for scope in [TextObjectScope::Inner, TextObjectScope::Around] {
                        let expected = quotes.chunks(2).find_map(|pair| match pair {
                            [start, end] if *start <= cursor && cursor <= *end => {
                                Some(match scope {
                                    TextObjectScope::Inner => (start + 1, *end),
                                    TextObjectScope::Around => {
                                        let trailing = characters[end + 1..]
                                            .iter()
                                            .take_while(|c| c.is_whitespace())
                                            .count();
                                        (*start, end + 1 + trailing)
                                    }
                                })
                            }
                            _ => None,
                        });
                        assert_eq!(
                            resolver
                                .text_object(scope, TextObjectKind::Quote('"'))
                                .map(|range| (range.start.character, range.end.character)),
                            expected,
                            "prefix={prefix:?} width={width} slashes={slash_count} scope={scope:?}"
                        );
                    }
                }
            }
        }
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
    fn indexed_ascii_word_operators_match_unicode_reference_semantics() {
        for text in [
            "",
            "word",
            "word  next",
            "  next",
            "\tword",
            "word,next",
            ",,word",
            "word\r\nnext",
            "\r\nnext",
            "\nnext",
            "word  \nnext",
            "first second\r\n  third, fourth",
            "first\n\n\tsecond third",
        ] {
            let ascii = Buffer::new(Some("ascii.txt".to_string()), text.to_string());
            let unicode = Buffer::new(Some("unicode.txt".to_string()), format!("λ\n{text}"));
            for character in [0, 1, text.len().saturating_sub(1), text.len(), usize::MAX] {
                for change_word in [false, true] {
                    for big_word in [false, true] {
                        for count in [1, 2, 3, 5] {
                            let actual =
                                MotionResolver::new(&ascii, TextPosition::new(0, character))
                                    .word_range(count, change_word, big_word)
                                    .map(|range| {
                                        (
                                            range.start.line,
                                            range.start.character,
                                            range.end.line,
                                            range.end.character,
                                            ascii.text_in_range(range),
                                        )
                                    });
                            let expected =
                                MotionResolver::new(&unicode, TextPosition::new(1, character))
                                    .word_range(count, change_word, big_word)
                                    .map(|range| {
                                        (
                                            range.start.line - 1,
                                            range.start.character,
                                            range.end.line - 1,
                                            range.end.character,
                                            unicode.text_in_range(range),
                                        )
                                    });
                            assert_eq!(
                                actual, expected,
                                "{text:?} count={count} column={character} change={change_word} big={big_word}"
                            );
                        }
                    }
                }
            }
        }
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
    fn indexed_line_lengths_preserve_unicode_crlf_and_linewise_paragraph_ranges() {
        for (text, character, expected, linewise) in [
            ("alpha\n\nbeta", 2, "pha", false),
            ("  alpha\n\nbeta", 2, "  alpha\n", true),
            ("\talpha\n\nbeta", 0, "\talpha\n", true),
            ("e\u{301}clair 👋\n\n漢字", 2, "clair 👋", false),
            ("alpha\r\n\r\nbeta", 2, "pha", false),
        ] {
            let buffer = Buffer::new(None, text.to_string());
            let resolver = MotionResolver::new(&buffer, TextPosition::new(0, character));
            let (range, actual_linewise) = resolver.paragraph_range(1, false).unwrap();
            assert_eq!(buffer.text_in_range(range), expected, "{text:?}");
            assert_eq!(actual_linewise, linewise, "{text:?}");
            for line in 0..=buffer.len() {
                let expected = buffer
                    .get(line)
                    .map(|value| {
                        crate::unicode_utils::trim_line_ending(&value)
                            .chars()
                            .count()
                    })
                    .unwrap_or_default();
                assert_eq!(buffer.line_char_len_without_ending(line), expected);
            }
        }
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

    #[test]
    fn indexed_text_objects_match_nested_and_escaped_reference_at_every_cursor() {
        for text in [
            "",
            "plain text",
            "(alpha)",
            "(outer (inner) tail)",
            "before [one [two] three] after",
            ")unmatched (open",
            "prefix \"first\" between \"second\"",
            "prefix \"a \\\" quote\" suffix",
            r#"prefix "even \\" next "tail""#,
            "e\u{301} (👨‍👩‍👧 [漢字]) \"🇧🇷 quoted\"",
        ] {
            let buffer = Buffer::new(None, text.to_string());
            let characters = text.chars().collect::<Vec<_>>();
            for cursor in 0..=characters.len() {
                let resolver = MotionResolver::new(&buffer, TextPosition::new(0, cursor));
                for scope in [TextObjectScope::Inner, TextObjectScope::Around] {
                    for (opening, closing) in [('(', ')'), ('[', ']')] {
                        let mut stack = Vec::new();
                        let mut expected: Option<(usize, usize)> = None;
                        for (index, character) in characters.iter().copied().enumerate() {
                            if character == opening {
                                stack.push(index);
                            } else if character == closing {
                                if let Some(start) = stack.pop() {
                                    if start <= cursor
                                        && cursor <= index
                                        && expected.is_none_or(|(best, _)| start > best)
                                    {
                                        expected = Some((start, index));
                                    }
                                }
                            }
                        }
                        let expected = expected.map(|(start, end)| match scope {
                            TextObjectScope::Inner => (start + 1, end),
                            TextObjectScope::Around => (start, end + 1),
                        });
                        let actual = resolver
                            .text_object(
                                scope,
                                TextObjectKind::Delimited {
                                    open: opening,
                                    close: closing,
                                },
                            )
                            .map(|range| (range.start.character, range.end.character));
                        assert_eq!(actual, expected, "{text:?} at {cursor} {scope:?}");
                    }

                    let byte = buffer.position_to_byte_idx(TextPosition::new(0, cursor));
                    let expected = vimlet::MotionResolver::new(&text, vimlet::ByteOffset(byte))
                        .text_object(scope, TextObjectKind::Quote('"'), 1)
                        .map(|range| {
                            let r = range.ranges[0];
                            (
                                text[..r.start.0].chars().count(),
                                text[..r.end.0].chars().count(),
                            )
                        });
                    let actual = resolver
                        .text_object(scope, TextObjectKind::Quote('"'))
                        .map(|range| (range.start.character, range.end.character));
                    assert_eq!(actual, expected, "{text:?} quote at {cursor} {scope:?}");
                }
            }
        }
    }

    #[test]
    fn indexed_word_objects_preserve_punctuation_whitespace_and_unicode_boundaries() {
        for text in [
            "",
            "word",
            "alpha beta",
            "  leading and trailing  ",
            "alpha,beta... gamma",
            "\talpha\tbeta\t",
            "___ symbols + punctuation",
        ] {
            let buffer = Buffer::new(Some("words.txt".into()), text.to_string());
            let chars = text.chars().collect::<Vec<_>>();
            for cursor in 0..=chars.len() + 1 {
                for big_word in [false, true] {
                    for scope in [TextObjectScope::Inner, TextObjectScope::Around] {
                        let byte = buffer.position_to_byte_idx(TextPosition::new(0, cursor));
                        let kind = if big_word {
                            TextObjectKind::BigWord
                        } else {
                            TextObjectKind::Word
                        };
                        let actual = MotionResolver::new(&buffer, TextPosition::new(0, cursor))
                            .text_object(scope, kind)
                            .map(|r| (r.start.character, r.end.character));
                        let expected = vimlet::MotionResolver::new(&text, vimlet::ByteOffset(byte))
                            .text_object(scope, kind, 1)
                            .map(|range| {
                                let r = range.ranges[0];
                                (
                                    text[..r.start.0].chars().count(),
                                    text[..r.end.0].chars().count(),
                                )
                            });
                        assert_eq!(
                            actual, expected,
                            "{text:?} at {cursor}, big={big_word}, {scope:?}"
                        );
                    }
                }
            }
        }

        let unicode = Buffer::new(None, "e\u{301}clair, 👨‍👩‍👧 tail".to_string());
        let resolver = MotionResolver::new(&unicode, TextPosition::new(0, 1));
        let range = resolver
            .text_object(TextObjectScope::Inner, TextObjectKind::Word)
            .unwrap();
        assert_eq!(unicode.text_in_range(range), "e\u{301}clair");
    }

    #[test]
    fn indexed_paragraph_objects_preserve_whitespace_unicode_and_selection_scopes() {
        for text in [
            "",
            "one paragraph",
            "first\nsecond\n\nthird",
            "first\n   \n\t\nsecond\n\nthird",
            "\n\nleading\nnext\n\n",
            "alpha\r\n\r\nbeta\r\ngamma",
            "漢字\n👨‍👩‍👧\n\u{3000}\n🇧🇷 tail",
        ] {
            let buffer = Buffer::new(None, text.to_string());
            for line in 0..=buffer.len() + 1 {
                let resolver = MotionResolver::new(&buffer, TextPosition::new(line, 0));
                for scope in [TextObjectScope::Inner, TextObjectScope::Around] {
                    let actual = resolver.text_object(scope, TextObjectKind::Paragraph);
                    if buffer.is_empty() {
                        assert!(actual.is_none());
                        continue;
                    }
                    let Some(range) = actual else {
                        continue;
                    };
                    assert!(
                        (range.start.line, range.start.character)
                            <= (range.end.line, range.end.character),
                        "{text:?} at {line} {scope:?}"
                    );
                    let selected = buffer.text_in_range(range);
                    assert!(!selected.is_empty(), "{text:?} at {line} {scope:?}");
                }
            }
        }

        let buffer = Buffer::new(None, "first\nsecond\n\nthird".to_string());
        let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 0));
        let inner = resolver
            .text_object(TextObjectScope::Inner, TextObjectKind::Paragraph)
            .unwrap();
        let around = resolver
            .text_object(TextObjectScope::Around, TextObjectKind::Paragraph)
            .unwrap();
        assert_eq!(buffer.text_in_range(inner), "first\nsecond\n");
        assert_eq!(buffer.text_in_range(around), "first\nsecond\n\n");
    }

    #[test]
    fn indexed_final_sentence_objects_preserve_punctuation_unicode_and_counts() {
        for text in [
            "final unterminated sentence",
            "first.\n\nfinal words and whitespace  ",
            "first.\n\n漢字 👨‍👩‍👧 e\u{301}clair",
        ] {
            let buffer = Buffer::new(None, text.to_string());
            let line = buffer.last_navigable_line();
            let expected = buffer.get(line).unwrap();
            for character in [0, 1, expected.chars().count().saturating_sub(1)] {
                let resolver = MotionResolver::new(&buffer, TextPosition::new(line, character));
                for scope in [TextObjectScope::Inner, TextObjectScope::Around] {
                    let range = resolver
                        .text_object(scope, TextObjectKind::Sentence)
                        .unwrap();
                    assert_eq!(buffer.text_in_range(range), expected, "{text:?} {scope:?}");
                }
            }
        }

        let punctuated = Buffer::new(None, "first.\n\nfinal. next".to_string());
        let resolver = MotionResolver::new(&punctuated, TextPosition::new(2, 0));
        let inner = resolver
            .text_object(TextObjectScope::Inner, TextObjectKind::Sentence)
            .unwrap();
        let around = resolver
            .text_object(TextObjectScope::Around, TextObjectKind::Sentence)
            .unwrap();
        assert_eq!(punctuated.text_in_range(inner), "final.");
        assert_eq!(punctuated.text_in_range(around), "final. ");
    }
}
