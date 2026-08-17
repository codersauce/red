//! Expansion and insert-mode navigation for language-server completion snippets.
//!
//! Snippet offsets are Unicode scalar positions, like buffer and undo ranges. Once a
//! completion is applied they become edit anchors at the canonical mutation boundary,
//! keeping later placeholders aligned while earlier arguments are replaced.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::{
    buffer::BufferId,
    config::KeyAction,
    undo::{AppliedTextEdit, TextPosition, TextRange},
};

use super::{Action, AnchorAffinity, EditAnchor, Editor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedPlaceholder {
    pub(super) index: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ParsedSnippet {
    pub(super) text: String,
    pub(super) placeholders: Vec<ParsedPlaceholder>,
    pub(super) final_cursor: Option<usize>,
}

#[derive(Debug, Clone)]
struct SnippetStop {
    index: usize,
    start: EditAnchor,
    end: EditAnchor,
}

#[derive(Debug, Clone)]
pub(super) struct SnippetSession {
    buffer_id: BufferId,
    stops: Vec<SnippetStop>,
    active: usize,
    final_cursor: EditAnchor,
    region_start: EditAnchor,
    region_end: EditAnchor,
    selected: bool,
}

pub(super) fn parse_snippet(source: &str) -> ParsedSnippet {
    let characters = source.chars().collect::<Vec<_>>();
    let mut parsed = ParsedSnippet::default();
    parse_fragment(&characters, 0, &mut parsed, false);
    parsed
        .placeholders
        .sort_by_key(|placeholder| placeholder.index);
    parsed
}

fn parse_fragment(
    characters: &[char],
    mut position: usize,
    parsed: &mut ParsedSnippet,
    stop_at_brace: bool,
) -> Option<usize> {
    while let Some(&character) = characters.get(position) {
        if stop_at_brace && character == '}' {
            return Some(position + 1);
        }

        if character == '\\' {
            match characters.get(position + 1) {
                Some(next @ ('$' | '}' | '\\')) => {
                    parsed.text.push(*next);
                    position += 2;
                }
                _ => {
                    parsed.text.push(character);
                    position += 1;
                }
            }
            continue;
        }

        if character != '$' {
            parsed.text.push(character);
            position += 1;
            continue;
        }

        match characters.get(position + 1) {
            Some('$') => {
                parsed.text.push('$');
                position += 2;
            }
            Some(digit) if digit.is_ascii_digit() => {
                let (next, index) = parse_index(characters, position + 1);
                record_placeholder(parsed, index, parsed.text.chars().count(), None);
                position = next;
            }
            Some('{') => {
                if characters
                    .get(position + 2)
                    .is_none_or(|character| !character.is_ascii_digit())
                {
                    if let Some(relative_end) = characters[position + 2..]
                        .iter()
                        .position(|character| *character == '}')
                    {
                        let end = position + 2 + relative_end;
                        parsed.text.extend(characters[position..=end].iter());
                        position = end + 1;
                    } else {
                        parsed.text.push('$');
                        position += 1;
                    }
                    continue;
                }
                let text_length = parsed.text.len();
                let placeholder_count = parsed.placeholders.len();
                let previous_final = parsed.final_cursor;
                if let Some(next) = parse_braced_placeholder(characters, position + 2, parsed) {
                    position = next;
                } else {
                    parsed.text.truncate(text_length);
                    parsed.placeholders.truncate(placeholder_count);
                    parsed.final_cursor = previous_final;
                    parsed.text.push('$');
                    position += 1;
                }
            }
            _ => {
                parsed.text.push('$');
                position += 1;
            }
        }
    }

    (!stop_at_brace).then_some(position)
}

fn parse_index(characters: &[char], start: usize) -> (usize, usize) {
    let mut position = start;
    let mut index = 0usize;
    while let Some(character) = characters
        .get(position)
        .filter(|value| value.is_ascii_digit())
    {
        index = index
            .saturating_mul(10)
            .saturating_add((*character as u8 - b'0') as usize);
        position += 1;
    }
    (position, index)
}

fn parse_braced_placeholder(
    characters: &[char],
    start: usize,
    parsed: &mut ParsedSnippet,
) -> Option<usize> {
    if !characters.get(start)?.is_ascii_digit() {
        return None;
    }

    let (position, index) = parse_index(characters, start);
    let placeholder_start = parsed.text.chars().count();
    match characters.get(position)? {
        '}' => {
            record_placeholder(parsed, index, placeholder_start, None);
            Some(position + 1)
        }
        ':' => {
            let next = parse_fragment(characters, position + 1, parsed, true)?;
            record_placeholder(
                parsed,
                index,
                placeholder_start,
                Some(parsed.text.chars().count()),
            );
            Some(next)
        }
        '|' => {
            let mut current = position + 1;
            let mut first_choice = true;
            while let Some(&character) = characters.get(current) {
                if character == '|' && characters.get(current + 1) == Some(&'}') {
                    record_placeholder(
                        parsed,
                        index,
                        placeholder_start,
                        Some(parsed.text.chars().count()),
                    );
                    return Some(current + 2);
                }
                if character == ',' {
                    first_choice = false;
                } else if character == '\\' {
                    current += 1;
                    if first_choice {
                        parsed.text.push(*characters.get(current)?);
                    }
                } else if first_choice {
                    parsed.text.push(character);
                }
                current += 1;
            }
            None
        }
        _ => None,
    }
}

fn record_placeholder(parsed: &mut ParsedSnippet, index: usize, start: usize, end: Option<usize>) {
    if index == 0 {
        parsed.final_cursor = Some(start);
    } else {
        parsed.placeholders.push(ParsedPlaceholder {
            index,
            start,
            end: end.unwrap_or(start),
        });
    }
}

impl Editor {
    pub(super) fn activate_snippet_session(&mut self, start: TextPosition, parsed: &ParsedSnippet) {
        if parsed.placeholders.is_empty() {
            return;
        }

        let base = self.current_buffer().position_to_char_idx(start);
        let mut stops = parsed
            .placeholders
            .iter()
            .map(|placeholder| SnippetStop {
                index: placeholder.index,
                start: self.anchor_at_char(base + placeholder.start, AnchorAffinity::Left),
                end: self.anchor_at_char(base + placeholder.end, AnchorAffinity::Right),
            })
            .collect::<Vec<_>>();
        stops.sort_by_key(|stop| stop.index);
        stops.dedup_by_key(|stop| stop.index);

        let length = parsed.text.chars().count();
        self.snippet_session = Some(SnippetSession {
            buffer_id: self.current_buffer().id(),
            stops,
            active: 0,
            final_cursor: self.anchor_at_char(
                base + parsed.final_cursor.unwrap_or(length),
                AnchorAffinity::Right,
            ),
            region_start: self.anchor_at_char(base, AnchorAffinity::Left),
            region_end: self.anchor_at_char(base + length, AnchorAffinity::Right),
            selected: true,
        });
    }

    pub(super) fn selected_snippet_range(&self) -> Option<TextRange> {
        let session = self.snippet_session.as_ref()?;
        if !self.is_insert() || !session.selected || session.buffer_id != self.current_buffer().id()
        {
            return None;
        }

        let stop = session.stops.get(session.active)?;
        Some(TextRange::new(
            self.current_buffer()
                .char_idx_to_position(stop.start.char_index),
            self.current_buffer()
                .char_idx_to_position(stop.end.char_index),
        ))
    }

    pub(super) fn take_selected_snippet_range(&mut self) -> Option<TextRange> {
        let range = self.selected_snippet_range()?;
        self.snippet_session.as_mut()?.selected = false;
        Some(range)
    }

    pub(super) fn snippet_final_cursor_position(&self) -> Option<TextPosition> {
        let session = self.snippet_session.as_ref()?;
        (session.buffer_id == self.current_buffer().id()).then(|| {
            self.current_buffer()
                .char_idx_to_position(session.final_cursor.char_index)
        })
    }

    pub(super) fn transform_snippet_anchors(&mut self, edit: AppliedTextEdit) {
        let buffer_id = self.current_buffer().id();
        let Some(session) = self.snippet_session.as_mut() else {
            return;
        };
        if session.buffer_id != buffer_id {
            return;
        }

        let transform = |anchor: &mut EditAnchor| {
            Self::transform_anchor_for_edit(
                anchor,
                edit.start_char,
                edit.end_char,
                edit.new_char_len,
            );
        };
        for stop in &mut session.stops {
            transform(&mut stop.start);
            transform(&mut stop.end);
        }
        transform(&mut session.final_cursor);
        transform(&mut session.region_start);
        transform(&mut session.region_end);
    }

    pub(super) fn handle_snippet_event(&mut self, event: &Event) -> Option<KeyAction> {
        if !self.is_insert()
            || self.panel_manager.focused_panel_id().is_some()
            || self
                .snippet_session
                .as_ref()
                .is_none_or(|session| session.buffer_id != self.current_buffer().id())
        {
            return None;
        }

        let action = match event {
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            }) => Action::NextSnippetPlaceholder,
            Event::Key(KeyEvent {
                code: KeyCode::BackTab,
                ..
            }) => Action::PreviousSnippetPlaceholder,
            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                ..
            }) if self
                .selected_snippet_range()
                .is_some_and(|range| range.start != range.end) =>
            {
                Action::DeleteSnippetPlaceholder
            }
            _ => return None,
        };

        if let Some(dialog) = &self.current_dialog {
            // An invisible, zero-match completion must not swallow snippet Tab.
            // Keep ordinary completion acceptance and other dialog keys intact.
            return (dialog.is_empty_completion()
                && matches!(
                    action,
                    Action::NextSnippetPlaceholder | Action::PreviousSnippetPlaceholder
                ))
            .then(|| KeyAction::Multiple(vec![Action::CloseDialog, action]));
        }
        Some(KeyAction::Single(action))
    }

    pub(super) fn navigate_snippet_placeholder(&mut self, backwards: bool) {
        let Some(session) = self.snippet_session.as_mut() else {
            return;
        };

        if backwards {
            session.active = session.active.saturating_sub(1);
        } else if session.active + 1 == session.stops.len() {
            let final_cursor = session.final_cursor.char_index;
            self.snippet_session = None;
            let position = self.current_buffer().char_idx_to_position(final_cursor);
            self.move_to_insert_text_position(position);
            return;
        } else {
            session.active += 1;
        }

        session.selected = true;
        let char_index = session.stops[session.active].start.char_index;
        let position = self.current_buffer().char_idx_to_position(char_index);
        self.move_to_insert_text_position(position);
    }

    pub(super) fn finish_snippet_after_action(&mut self, action: &Action) {
        let Some(session) = self.snippet_session.as_ref() else {
            return;
        };
        let cursor = self
            .current_buffer()
            .position_to_char_idx(self.cursor_text_position());
        if !self.is_insert()
            || session.buffer_id != self.current_buffer().id()
            || cursor < session.region_start.char_index
            || cursor > session.region_end.char_index
        {
            self.snippet_session = None;
        } else if Self::action_is_pure_motion(action)
            || matches!(action, Action::SetCursor(..) | Action::MoveTo(..))
        {
            if let Some(session) = self.snippet_session.as_mut() {
                session.selected = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_snippet, ParsedPlaceholder};

    #[test]
    fn parses_ordered_unicode_placeholders_and_final_cursor() {
        let parsed = parse_snippet("spawn(${2:位置}, ${1:🚀})$0");
        assert_eq!(parsed.text, "spawn(位置, 🚀)");
        assert_eq!(
            parsed.placeholders,
            vec![
                ParsedPlaceholder {
                    index: 1,
                    start: 10,
                    end: 11,
                },
                ParsedPlaceholder {
                    index: 2,
                    start: 6,
                    end: 8,
                },
            ]
        );
        assert_eq!(parsed.final_cursor, Some(12));
    }

    #[test]
    fn parses_nested_multiline_placeholders_choices_and_escapes() {
        let parsed = parse_snippet("${1:first ${2:line\\}}}\n${3|alpha,beta|} \\$ $$ $10");
        assert_eq!(parsed.text, "first line}\nalpha $ $ ");
        assert_eq!(
            parsed
                .placeholders
                .iter()
                .map(|placeholder| placeholder.index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 10]
        );
    }

    #[test]
    fn preserves_malformed_or_unknown_snippet_syntax() {
        let parsed = parse_snippet("${1:unfinished $VARIABLE ${name}");
        assert_eq!(parsed.text, "${1:unfinished $VARIABLE ${name}");
        assert!(parsed.placeholders.is_empty());
    }
}
