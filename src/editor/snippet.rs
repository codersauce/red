//! Expansion and insert-mode navigation for language-server completion snippets.
//!
//! Snippet offsets are Unicode scalar positions, like buffer and undo ranges. Once a
//! completion is applied they become edit anchors at the canonical mutation boundary,
//! keeping later placeholders aligned while earlier arguments are replaced.

use std::collections::BTreeMap;

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

#[cfg(feature = "helix")]
impl ParsedSnippet {
    pub(super) fn insert_commit_character(&mut self, ch: char) {
        let offset = self
            .final_cursor
            .unwrap_or_else(|| self.text.chars().count());
        let byte = self
            .text
            .char_indices()
            .nth(offset)
            .map_or(self.text.len(), |(i, _)| i);
        self.text.insert(byte, ch);
        for stop in &mut self.placeholders {
            if stop.start >= offset {
                stop.start += 1;
                stop.end += 1;
            } else if stop.end > offset {
                stop.end += 1;
            }
        }
        self.final_cursor = Some(offset + 1);
    }
}

#[derive(Debug, Clone)]
struct SnippetStop {
    index: usize,
    start: EditAnchor,
    end: EditAnchor,
}

#[derive(Debug, Clone)]
struct SnippetInstance {
    stops: Vec<SnippetStop>,
    final_cursor: EditAnchor,
}

#[derive(Debug, Clone)]
pub(super) struct SnippetSession {
    buffer_id: BufferId,
    instances: Vec<SnippetInstance>,
    primary: usize,
    active: usize,
    region_start: EditAnchor,
    region_end: EditAnchor,
    selected: bool,
}

pub(super) fn parse_snippet(source: &str) -> ParsedSnippet {
    parse_with_defaults(source, &BTreeMap::new())
}

#[cfg(feature = "helix")]
pub(super) fn parse_felix_snippet(source: &str) -> ParsedSnippet {
    let parsed = parse_snippet(source);
    if !parsed
        .placeholders
        .windows(2)
        .any(|pair| pair[0].index == pair[1].index)
    {
        return parsed;
    }
    let mut defaults = BTreeMap::new();
    for stop in &parsed.placeholders {
        if stop.start < stop.end {
            defaults.entry(stop.index).or_insert_with(|| {
                parsed
                    .text
                    .chars()
                    .skip(stop.start)
                    .take(stop.end - stop.start)
                    .collect()
            });
        }
    }
    parse_with_defaults(source, &defaults)
}

fn parse_with_defaults(source: &str, defaults: &BTreeMap<usize, String>) -> ParsedSnippet {
    let characters = source.chars().collect::<Vec<_>>();
    let mut parsed = ParsedSnippet::default();
    parse_fragment(&characters, 0, &mut parsed, false, defaults);
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
    defaults: &BTreeMap<usize, String>,
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
                record_mirror(parsed, index, defaults);
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
                if let Some(next) =
                    parse_braced_placeholder(characters, position + 2, parsed, defaults)
                {
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
    defaults: &BTreeMap<usize, String>,
) -> Option<usize> {
    if !characters.get(start)?.is_ascii_digit() {
        return None;
    }

    let (position, index) = parse_index(characters, start);
    let placeholder_start = parsed.text.chars().count();
    match characters.get(position)? {
        '}' => {
            record_mirror(parsed, index, defaults);
            Some(position + 1)
        }
        ':' => {
            let next = parse_fragment(characters, position + 1, parsed, true, defaults)?;
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

fn record_mirror(parsed: &mut ParsedSnippet, index: usize, defaults: &BTreeMap<usize, String>) {
    let start = parsed.text.chars().count();
    if let Some(value) = defaults.get(&index) {
        parsed.text.push_str(value);
    }
    record_placeholder(parsed, index, start, Some(parsed.text.chars().count()));
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
        let base = self.current_buffer().position_to_char_idx(start);
        self.activate_snippet_instances(&[base], 0, parsed, false);
    }

    pub(super) fn activate_snippet_instances(
        &mut self,
        bases: &[usize],
        primary: usize,
        parsed: &ParsedSnippet,
        keep_mirrors: bool,
    ) {
        self.snippet_session = None;
        if parsed.placeholders.is_empty() {
            return;
        }
        let length = parsed.text.chars().count();
        let instances = bases
            .iter()
            .map(|&base| {
                let mut stops: Vec<_> = parsed
                    .placeholders
                    .iter()
                    .map(|placeholder| SnippetStop {
                        index: placeholder.index,
                        start: self.anchor_at_char(base + placeholder.start, AnchorAffinity::Left),
                        end: self.anchor_at_char(base + placeholder.end, AnchorAffinity::Right),
                    })
                    .collect();
                if !keep_mirrors {
                    stops.dedup_by_key(|stop| stop.index);
                }
                SnippetInstance {
                    stops,
                    final_cursor: self.anchor_at_char(
                        base + parsed.final_cursor.unwrap_or(length),
                        AnchorAffinity::Right,
                    ),
                }
            })
            .collect();
        self.snippet_session = Some(SnippetSession {
            buffer_id: self.current_buffer().id(),
            instances,
            primary,
            active: 0,
            region_start: self.anchor_at_char(bases[primary], AnchorAffinity::Left),
            region_end: self.anchor_at_char(bases[primary] + length, AnchorAffinity::Right),
            selected: true,
        });
    }

    pub(super) fn selected_snippet_range(&self) -> Option<TextRange> {
        let session = self.snippet_session.as_ref()?;
        if !self.is_insert() || !session.selected || session.buffer_id != self.current_buffer().id()
        {
            return None;
        }

        let stop = session.instances[session.primary]
            .stops
            .get(session.active)?;
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
                .char_idx_to_position(session.instances[session.primary].final_cursor.char_index)
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
        for instance in &mut session.instances {
            for stop in &mut instance.stops {
                transform(&mut stop.start);
                transform(&mut stop.end);
            }
            transform(&mut instance.final_cursor);
        }
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

        #[cfg(feature = "helix")]
        if self.felix_active_selection().is_some() {
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

        let stops = &session.instances[session.primary].stops;
        let index = stops[session.active].index;
        let next = if backwards {
            let prev = stops
                .partition_point(|stop| stop.index < index)
                .saturating_sub(1);
            stops.partition_point(|stop| stop.index < stops[prev].index)
        } else {
            stops.partition_point(|stop| stop.index <= index)
        };
        if next == stops.len() {
            let final_cursor = session.instances[session.primary].final_cursor.char_index;
            #[cfg(feature = "helix")]
            if self.felix_active_selection().is_some() {
                self.select_felix_snippet_stop(true);
                self.snippet_session = None;
                return;
            }
            self.snippet_session = None;
            let position = self.current_buffer().char_idx_to_position(final_cursor);
            self.move_to_insert_text_position(position);
            return;
        }
        session.active = next;

        session.selected = true;
        let char_index = session.instances[session.primary].stops[session.active]
            .start
            .char_index;
        #[cfg(feature = "helix")]
        if self.felix_active_selection().is_some() {
            self.select_felix_snippet_stop(false);
            return;
        }
        let position = self.current_buffer().char_idx_to_position(char_index);
        self.move_to_insert_text_position(position);
    }

    #[cfg(feature = "helix")]
    pub(super) fn select_felix_snippet_stop(&mut self, final_stop: bool) {
        let Some(session) = &mut self.snippet_session else {
            return;
        };
        // While typing into a stop, an adjacent stop on either side must
        // stay outside the inserted text even when a deletion joins them.
        let active = session.instances[session.primary].stops[session.active].index;
        let mut ranges = Vec::new();
        let mut primary = 0;
        for (i, instance) in session.instances.iter_mut().enumerate() {
            if i == session.primary {
                primary = ranges.len();
            }
            if final_stop {
                let pos = instance.final_cursor.char_index;
                ranges.push(felix_core::Range {
                    anchor: pos,
                    head: pos,
                });
                continue;
            }
            let targets: Vec<_> = instance
                .stops
                .iter()
                .filter(|stop| stop.index == active)
                .map(|stop| stop.start.char_index..stop.end.char_index)
                .collect();
            for stop in &mut instance.stops {
                stop.start.affinity = if stop.index != active
                    && targets.iter().any(|r| r.end == stop.start.char_index)
                {
                    AnchorAffinity::Right
                } else {
                    AnchorAffinity::Left
                };
                stop.end.affinity = if stop.index != active
                    && targets.iter().any(|r| r.start == stop.end.char_index)
                {
                    AnchorAffinity::Left
                } else {
                    AnchorAffinity::Right
                };
            }
            ranges.extend(targets.into_iter().map(|r| felix_core::Range {
                anchor: r.start,
                head: r.end,
            }));
        }
        let text = self.current_buffer().contents_snapshot();
        if let Some(state) = felix_core::Selections::new(text.slice(..), ranges, primary) {
            self.set_felix_active_selection(state);
        }
    }

    #[cfg(feature = "helix")]
    pub(super) fn prepare_felix_snippet_input(&mut self, key: &str) -> bool {
        if !self.is_insert() || self.current_buffer().directory.is_some() {
            return false;
        }
        let buffer_id = self.current_buffer().id();
        let Some(session) = self.snippet_session.as_mut() else {
            return false;
        };
        if session.buffer_id != buffer_id {
            self.snippet_session = None;
            return false;
        }
        if !session.selected || key == "<tab>" {
            return false;
        }
        session.selected = false;
        if key.chars().count() != 1 {
            return false;
        }
        let active = session.instances[session.primary].stops[session.active].index;
        let mut ranges = Vec::new();
        for instance in &mut session.instances {
            let targets: Vec<_> = instance
                .stops
                .iter()
                .filter(|stop| stop.index == active)
                .map(|stop| stop.start.char_index..stop.end.char_index)
                .filter(|r| !r.is_empty())
                .collect();
            // Replacing the enclosing default removes its nested placeholders.
            instance.stops.retain(|stop| {
                stop.index == active
                    || !targets.iter().any(|r| {
                        r.start <= stop.start.char_index
                            && stop.end.char_index <= r.end
                            && (stop.start.char_index != stop.end.char_index
                                || (r.start < stop.start.char_index && stop.end.char_index < r.end))
                    })
            });
            ranges.extend(targets);
        }
        session.active = session.instances[session.primary]
            .stops
            .partition_point(|stop| stop.index < active);
        ranges.sort_unstable_by_key(|r| r.start);
        // An intervening edit can collapse originally disjoint placeholders.
        // Applying overlapping deletions in sequence would delete unrelated text.
        if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
            self.snippet_session = None;
            return false;
        }
        for range in ranges.iter().rev() {
            let buffer = self.current_buffer();
            self.replace_range(
                TextRange::new(
                    buffer.char_idx_to_position(range.start),
                    buffer.char_idx_to_position(range.end),
                ),
                "",
            );
        }
        self.select_felix_snippet_stop(false);
        !ranges.is_empty()
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

    #[cfg(feature = "helix")]
    #[test]
    fn commit_character_moves_following_stops_in_character_coordinates() {
        let mut parsed = parse_snippet("${1:λ}$0${2:位置}$3");
        parsed.insert_commit_character('🚀');
        assert_eq!(parsed.text, "λ🚀位置");
        assert_eq!(parsed.final_cursor, Some(2));
        let ranges: Vec<_> = parsed.placeholders.iter().map(|p| p.start..p.end).collect();
        assert_eq!(ranges, [0..1, 2..4, 4..4]);
    }

    #[test]
    fn preserves_malformed_or_unknown_snippet_syntax() {
        let parsed = parse_snippet("${1:unfinished $VARIABLE ${name}");
        assert_eq!(parsed.text, "${1:unfinished $VARIABLE ${name}");
        assert!(parsed.placeholders.is_empty());
    }
}
