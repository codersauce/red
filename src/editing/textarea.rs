//! Host-independent, rope-backed Vim editing for embedded text surfaces.

use std::collections::HashMap;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    apply_transactional_replacement, text_object_kind_for_key, CharacterMotion, MotionResolver,
    TextObjectKind, TextObjectScope,
};
use crate::{
    buffer::Buffer,
    editor::Mode,
    text_layout::{LayoutOptions, TextLayout},
    undo::{CursorSnapshot, TextPosition, TextRange},
    unicode_utils::{char_to_grapheme, grapheme_len, grapheme_to_byte, trim_line_ending},
};

const DEFAULT_MAX_BYTES: usize = 128 * 1024;
const MAX_MACRO_EVENTS: usize = 1_000;

/// Result of handling one input event in a host-owned editing surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAreaOutcome {
    /// Text, cursor position, selection, mode, or pending command changed.
    Changed,
    /// The key does not belong to the text-editing surface.
    Unhandled,
}

/// A yank, delete, or change result that can be shared with a host register bank.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterContent {
    /// Exact selected UTF-8 text.
    pub text: String,
    /// Whether the operation selected complete logical lines.
    pub linewise: bool,
}

/// Interaction state that belongs to one focused view, not to its surrounding editor.
#[derive(Debug, Clone)]
pub struct EditState {
    mode: Mode,
    cursor: usize,
    preferred_column: Option<usize>,
    selection_anchor: Option<usize>,
    count: Option<u16>,
    pending: Option<PendingInput>,
    last_character_motion: Option<(CharacterMotion, char)>,
    last_change: Option<Vec<char>>,
    recording: Option<(char, Vec<char>)>,
    last_macro: Option<char>,
    search: Option<SearchState>,
    last_search: Option<String>,
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            mode: Mode::Insert,
            cursor: 0,
            preferred_column: None,
            selection_anchor: None,
            count: None,
            pending: None,
            last_character_motion: None,
            last_change: None,
            recording: None,
            last_macro: None,
            search: None,
            last_search: None,
        }
    }
}

impl EditState {
    /// Returns this surface's independent Vim mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Returns the absolute extended-grapheme cursor index.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the selection's absolute extended-grapheme anchor, if present.
    #[must_use]
    pub const fn selection_anchor(&self) -> Option<usize> {
        self.selection_anchor
    }

    /// Returns whether a count, operator, character search, or text object is incomplete.
    #[must_use]
    pub fn has_pending_input(&self) -> bool {
        self.pending.is_some() || self.count.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Delete,
    Change,
    Yank,
}

#[derive(Debug, Clone)]
enum PendingInput {
    Operator {
        operator: Operator,
        operator_count: u16,
        motion_count: Option<u16>,
        keys: Vec<char>,
    },
    Character {
        motion: CharacterMotion,
        count: u16,
        operator: Option<Operator>,
        keys: Vec<char>,
    },
    TextObject {
        operator: Option<Operator>,
        count: u16,
        scope: TextObjectScope,
        keys: Vec<char>,
    },
    GPrefix {
        operator: Option<Operator>,
        count: u16,
        keys: Vec<char>,
    },
    Replace {
        count: u16,
        keys: Vec<char>,
    },
    MacroRecord,
    MacroPlay {
        count: u16,
    },
}

#[derive(Debug, Clone)]
struct SearchState {
    pattern: String,
    origin: usize,
    backward: bool,
}

/// Fileless text document with independent Vim state, registers, and undo history.
///
/// This type deliberately has no access to files, application commands, LSP, plugin
/// callbacks, or terminal output. A host decides how to draw it and which unhandled
/// keys submit, cancel, or transfer focus.
#[derive(Debug)]
pub struct TextArea {
    buffer: Buffer,
    state: EditState,
    max_bytes: usize,
    register: RegisterContent,
    macro_registers: HashMap<char, Vec<char>>,
    replaying: bool,
    insert_recipe: Option<Vec<char>>,
}

impl TextArea {
    /// Creates an unnamed, multiline editing surface in Insert mode.
    #[must_use]
    pub fn new(text: impl AsRef<str>) -> Self {
        Self::with_max_bytes(text, DEFAULT_MAX_BYTES)
    }

    /// Creates an editing surface with an explicit upper bound on UTF-8 bytes.
    #[must_use]
    pub fn with_max_bytes(text: impl AsRef<str>, max_bytes: usize) -> Self {
        let normalized = normalize_newlines(text.as_ref());
        let text = if normalized.len() <= max_bytes {
            normalized
        } else {
            String::new()
        };
        let mut area = Self {
            buffer: unnamed_buffer(&text),
            state: EditState {
                cursor: grapheme_len(&text),
                ..EditState::default()
            },
            max_bytes,
            register: RegisterContent::default(),
            macro_registers: HashMap::new(),
            replaying: false,
            insert_recipe: None,
        };
        area.sync_buffer_cursor();
        area
    }

    /// Returns the unnamed, rope-backed document and its branching undo history.
    #[must_use]
    pub const fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Returns the complete document without synthesizing a trailing newline.
    #[must_use]
    pub fn text(&self) -> String {
        self.buffer.contents()
    }

    /// Returns this surface's interaction state.
    #[must_use]
    pub const fn state(&self) -> &EditState {
        &self.state
    }

    /// Returns the absolute extended-grapheme cursor position.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.state.cursor
    }

    /// Returns the current Normal, Insert, Visual, or Search mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.state.mode
    }

    /// Returns the most recent local yank, delete, or change register.
    #[must_use]
    pub fn register(&self) -> &RegisterContent {
        &self.register
    }

    /// Replaces the local register, allowing a host to share its global clipboard.
    pub fn set_register(&mut self, register: RegisterContent) {
        self.register = register;
    }

    /// Updates the surface-local mode without changing any surrounding editor.
    pub fn set_mode(&mut self, mode: Mode) {
        if !matches!(
            mode,
            Mode::Normal | Mode::Insert | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        ) {
            return;
        }
        if self.state.mode == Mode::Insert && mode != Mode::Insert {
            self.finish_insert_recipe();
        }
        self.state.mode = mode;
        self.state.pending = None;
        self.state.count = None;
        if matches!(mode, Mode::Normal | Mode::Insert) {
            self.state.selection_anchor = None;
        }
    }

    /// Moves the cursor to a bounded, absolute extended-grapheme position.
    pub fn set_cursor(&mut self, cursor: usize) {
        self.state.cursor = cursor.min(grapheme_len(&self.text()));
        self.state.preferred_column = None;
        self.sync_buffer_cursor();
    }

    /// Replaces the complete text as one undoable transaction.
    pub fn set_text(&mut self, text: &str) -> bool {
        let text = normalize_newlines(text);
        if text.len() > self.max_bytes {
            return false;
        }
        let end = grapheme_len(&self.text());
        self.replace_graphemes(0, end, &text, grapheme_len(&text), "replace text area")
    }

    /// Inserts normalized text as one undoable transaction.
    pub fn insert(&mut self, text: &str) -> bool {
        let text = normalize_newlines(text);
        if text.is_empty() {
            return false;
        }
        let cursor = self.state.cursor;
        let mut prefix = self.text();
        prefix.truncate(grapheme_to_byte(&prefix, cursor));
        prefix.push_str(&text);
        let resulting_cursor = grapheme_len(&prefix);
        self.replace_graphemes(cursor, cursor, &text, resulting_cursor, "insert text")
    }

    /// Removes the complete extended grapheme immediately before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.state.cursor == 0 {
            return false;
        }
        let start = self.state.cursor - 1;
        self.replace_graphemes(start, self.state.cursor, "", start, "delete grapheme")
    }

    /// Removes the complete extended grapheme directly under the cursor.
    pub fn delete(&mut self) -> bool {
        if self.state.cursor >= grapheme_len(&self.text()) {
            return false;
        }
        let cursor = self.state.cursor;
        self.replace_graphemes(cursor, cursor + 1, "", cursor, "delete grapheme")
    }

    /// Removes whitespace and the word immediately before the insertion cursor.
    pub fn delete_previous_word(&mut self) -> bool {
        if self.state.cursor == 0 {
            return false;
        }
        let text = self.text();
        let end = grapheme_to_byte(&text, self.state.cursor);
        let mut start = self.state.cursor;
        let mut seen_word = false;
        for grapheme in text[..end].graphemes(true).rev() {
            let whitespace = grapheme.chars().all(char::is_whitespace);
            if seen_word && whitespace {
                break;
            }
            seen_word |= !whitespace;
            start -= 1;
        }
        self.replace_graphemes(start, self.state.cursor, "", start, "delete previous word")
    }

    /// Undoes one transaction and restores its exact grapheme cursor snapshot.
    pub fn undo(&mut self) -> bool {
        self.finish_insert_recipe();
        let mut history = std::mem::take(&mut self.buffer.undo_history);
        let restored = history.undo(&mut self.buffer);
        self.buffer.undo_history = history;
        let Some((cursor, _)) = restored else {
            return false;
        };
        self.restore_cursor(cursor);
        self.buffer.refresh_dirty_from_history();
        true
    }

    /// Redoes one transaction from the selected undo-tree branch.
    pub fn redo(&mut self) -> bool {
        self.finish_insert_recipe();
        let mut history = std::mem::take(&mut self.buffer.undo_history);
        let restored = history.redo(&mut self.buffer);
        self.buffer.undo_history = history;
        let Some((cursor, _)) = restored else {
            return false;
        };
        self.restore_cursor(cursor);
        self.buffer.refresh_dirty_from_history();
        true
    }

    /// Clears text and pending commands while preserving the configured byte limit.
    pub fn clear(&mut self) {
        self.buffer = unnamed_buffer("");
        self.state.cursor = 0;
        self.state.preferred_column = None;
        self.state.pending = None;
        self.state.selection_anchor = None;
        self.state.count = None;
        self.insert_recipe = None;
    }

    /// Applies one editing event, leaving submission and focus decisions to the host.
    pub fn handle_event(&mut self, event: &Event, wrap_width: usize) -> TextAreaOutcome {
        self.handle_event_with_layout_options(event, LayoutOptions::grapheme(wrap_width.max(1)))
    }

    /// Applies an event using the host's display policy for visual-row motions.
    /// Logical-line operators and the document itself are independent of this policy.
    pub fn handle_event_with_layout_options(
        &mut self,
        event: &Event,
        layout: LayoutOptions,
    ) -> TextAreaOutcome {
        match event {
            Event::Paste(text) => {
                self.state.pending = None;
                self.state.count = None;
                if self.insert(text) {
                    TextAreaOutcome::Changed
                } else {
                    TextAreaOutcome::Unhandled
                }
            }
            Event::Key(key) => self.handle_key(*key, layout),
            _ => TextAreaOutcome::Unhandled,
        }
    }

    fn handle_key(&mut self, key: KeyEvent, layout: LayoutOptions) -> TextAreaOutcome {
        if self.state.mode == Mode::Search {
            return self.handle_search_key(key);
        }

        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Esc => {
                if self.state.mode == Mode::Insert {
                    let cursor = self
                        .state
                        .cursor
                        .saturating_sub(1)
                        .max(self.current_line_start());
                    self.set_cursor(cursor);
                    self.finish_insert_recipe();
                    self.set_mode(Mode::Normal);
                    return TextAreaOutcome::Changed;
                }
                if matches!(
                    self.state.mode,
                    Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                ) || self.state.pending.is_some()
                    || self.state.count.is_some()
                {
                    self.set_mode(Mode::Normal);
                    return TextAreaOutcome::Changed;
                }
                return TextAreaOutcome::Unhandled;
            }
            KeyCode::Char('r' | 'R') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.redo();
                return TextAreaOutcome::Changed;
            }
            KeyCode::Char('v' | 'V') if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.state.mode != Mode::Insert {
                    self.toggle_visual(Mode::VisualBlock);
                    return TextAreaOutcome::Changed;
                }
                return TextAreaOutcome::Unhandled;
            }
            KeyCode::Left => {
                self.move_horizontal(-1);
                return TextAreaOutcome::Changed;
            }
            KeyCode::Right => {
                self.move_horizontal(1);
                return TextAreaOutcome::Changed;
            }
            KeyCode::Up => {
                self.move_vertical(-1, layout);
                return TextAreaOutcome::Changed;
            }
            KeyCode::Down => {
                self.move_vertical(1, layout);
                return TextAreaOutcome::Changed;
            }
            KeyCode::Home => {
                self.set_cursor(0);
                return TextAreaOutcome::Changed;
            }
            KeyCode::End => {
                self.set_cursor(grapheme_len(&self.text()));
                return TextAreaOutcome::Changed;
            }
            KeyCode::Backspace => {
                self.backspace();
                return TextAreaOutcome::Changed;
            }
            KeyCode::Delete => {
                self.delete();
                return TextAreaOutcome::Changed;
            }
            KeyCode::Enter | KeyCode::Char('\n') if self.state.mode == Mode::Insert => {
                self.record_insert_character('\n');
                self.insert("\n");
                return TextAreaOutcome::Changed;
            }
            KeyCode::Tab if self.state.mode == Mode::Insert => {
                self.record_insert_character('\t');
                self.insert("\t");
                return TextAreaOutcome::Changed;
            }
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.state.mode == Mode::Insert {
                    self.record_insert_character(character);
                    return if self.insert(&character.to_string()) {
                        TextAreaOutcome::Changed
                    } else {
                        TextAreaOutcome::Unhandled
                    };
                }
                return self.handle_normal_character(character, layout);
            }
            _ => {}
        }

        TextAreaOutcome::Unhandled
    }

    fn handle_normal_character(
        &mut self,
        character: char,
        layout: LayoutOptions,
    ) -> TextAreaOutcome {
        if let Some((_, recorded)) = self.state.recording.as_mut() {
            if character != 'q' {
                recorded.push(character);
            }
        }

        if let Some(pending) = self.state.pending.take() {
            return self.handle_pending(pending, character, layout);
        }

        if character.is_ascii_digit() && (character != '0' || self.state.count.is_some()) {
            let digit = character.to_digit(10).unwrap_or_default() as u16;
            self.state.count = Some(
                self.state
                    .count
                    .unwrap_or_default()
                    .saturating_mul(10)
                    .saturating_add(digit),
            );
            return TextAreaOutcome::Changed;
        }

        let explicit_count = self.state.count.is_some();
        let count = self.state.count.take().unwrap_or(1);
        if self.is_visual() && matches!(character, 'i' | 'a') {
            self.state.pending = Some(PendingInput::TextObject {
                operator: None,
                count,
                scope: if character == 'i' {
                    TextObjectScope::Inner
                } else {
                    TextObjectScope::Around
                },
                keys: vec![character],
            });
            return TextAreaOutcome::Changed;
        }
        match character {
            'i' => self.enter_insert(vec!['i'], false),
            'I' => {
                self.move_to_first_non_blank();
                self.enter_insert(vec!['I'], false);
            }
            'a' => {
                self.set_cursor(
                    self.state
                        .cursor
                        .saturating_add(1)
                        .min(self.current_line_end()),
                );
                self.enter_insert(vec!['a'], false);
            }
            'A' => {
                self.set_cursor(self.current_line_end());
                self.enter_insert(vec!['A'], false);
            }
            'o' | 'O' => self.open_line(character == 'o', character),
            'h' => self.repeat_motion(count, |area| area.move_horizontal(-1)),
            'l' => self.repeat_motion(count, |area| area.move_horizontal(1)),
            'j' => self.repeat_motion(count, |area| area.move_vertical(1, layout)),
            'k' => self.repeat_motion(count, |area| area.move_vertical(-1, layout)),
            '0' => self.set_cursor(self.current_line_start()),
            '^' => self.move_to_first_non_blank(),
            '$' => self.move_to_line_end(count),
            'w' | 'W' | 'b' | 'B' | 'e' | 'E' => {
                self.move_word(character, count);
            }
            'g' => {
                self.state.pending = Some(PendingInput::GPrefix {
                    operator: None,
                    count,
                    keys: vec!['g'],
                });
            }
            'G' => self.move_to_line(if !explicit_count {
                self.last_editable_line()
            } else {
                usize::from(count.saturating_sub(1))
            }),
            'f' | 't' | 'F' | 'T' => {
                self.state.pending = Some(PendingInput::Character {
                    motion: character_motion(character),
                    count,
                    operator: None,
                    keys: vec![character],
                });
            }
            ';' | ',' => self.repeat_character_motion(character == ',', count),
            'd' | 'c' | 'y' => {
                if self.is_visual() {
                    self.apply_selection(operator_for_character(character), vec![character]);
                } else {
                    self.state.pending = Some(PendingInput::Operator {
                        operator: operator_for_character(character),
                        operator_count: count,
                        motion_count: None,
                        keys: vec![character],
                    });
                }
            }
            'D' | 'C' | 'Y' => {
                let operator = match character {
                    'D' => Operator::Delete,
                    'C' => Operator::Change,
                    _ => Operator::Yank,
                };
                let start = self.cursor_position();
                let end_line = start
                    .line
                    .saturating_add(usize::from(count.saturating_sub(1)))
                    .min(self.last_editable_line());
                let end = TextPosition::new(end_line, self.line_character_len(end_line));
                self.apply_operator(operator, TextRange::new(start, end), false, vec![character]);
            }
            'x' if self.is_visual() => self.apply_selection(Operator::Delete, vec!['x']),
            'x' => self.delete_characters(count, false, vec!['x']),
            'X' => self.delete_characters(count, true, vec!['X']),
            's' => self.change_characters(count, vec!['s']),
            'S' => self.operate_current_lines(Operator::Change, count, vec!['S']),
            'p' | 'P' => self.paste(character == 'P', count, vec![character]),
            'u' => {
                for _ in 0..count {
                    if !self.undo() {
                        break;
                    }
                }
                self.set_mode(Mode::Normal);
            }
            'U' => {
                for _ in 0..count {
                    if !self.redo() {
                        break;
                    }
                }
                self.set_mode(Mode::Normal);
            }
            'v' => self.toggle_visual(Mode::Visual),
            'V' => self.toggle_visual(Mode::VisualLine),
            'r' => {
                self.state.pending = Some(PendingInput::Replace {
                    count,
                    keys: vec!['r'],
                });
            }
            '~' => self.toggle_case(count, vec!['~']),
            'J' => self.join_lines(count.max(2), false, vec!['J']),
            '.' => self.repeat_last_change(count, layout),
            '/' | '?' => {
                self.state.search = Some(SearchState {
                    pattern: String::new(),
                    origin: self.state.cursor,
                    backward: character == '?',
                });
                self.state.mode = Mode::Search;
            }
            'n' | 'N' => self.repeat_search(character == 'N', count),
            'q' => {
                if self.state.recording.is_some() {
                    if let Some((register, recorded)) = self.state.recording.take() {
                        self.macro_registers.insert(register, recorded);
                    }
                } else {
                    self.state.pending = Some(PendingInput::MacroRecord);
                }
            }
            '@' => self.state.pending = Some(PendingInput::MacroPlay { count }),
            '%' => self.move_to_matching_delimiter(),
            _ => return TextAreaOutcome::Unhandled,
        }
        TextAreaOutcome::Changed
    }

    fn handle_pending(
        &mut self,
        pending: PendingInput,
        character: char,
        layout: LayoutOptions,
    ) -> TextAreaOutcome {
        match pending {
            PendingInput::Operator {
                operator,
                operator_count,
                motion_count,
                mut keys,
            } => {
                keys.push(character);
                if character.is_ascii_digit() && (character != '0' || motion_count.is_some()) {
                    let digit = character.to_digit(10).unwrap_or_default() as u16;
                    let motion_count = motion_count
                        .unwrap_or_default()
                        .saturating_mul(10)
                        .saturating_add(digit);
                    self.state.pending = Some(PendingInput::Operator {
                        operator,
                        operator_count,
                        motion_count: Some(motion_count),
                        keys,
                    });
                    return TextAreaOutcome::Changed;
                }

                let count = operator_count.saturating_mul(motion_count.unwrap_or(1));
                if matches!(
                    (operator, character),
                    (Operator::Delete, 'd') | (Operator::Change, 'c') | (Operator::Yank, 'y')
                ) {
                    self.operate_current_lines(operator, count, keys);
                } else if matches!(character, 'i' | 'a') {
                    self.state.pending = Some(PendingInput::TextObject {
                        operator: Some(operator),
                        count,
                        scope: if character == 'i' {
                            TextObjectScope::Inner
                        } else {
                            TextObjectScope::Around
                        },
                        keys,
                    });
                } else if matches!(character, 'f' | 't' | 'F' | 'T') {
                    self.state.pending = Some(PendingInput::Character {
                        motion: character_motion(character),
                        count,
                        operator: Some(operator),
                        keys,
                    });
                } else if character == 'g' {
                    self.state.pending = Some(PendingInput::GPrefix {
                        operator: Some(operator),
                        count,
                        keys,
                    });
                } else if let Some((range, linewise)) =
                    self.operator_motion_range(character, count, operator)
                {
                    self.apply_operator(operator, range, linewise, keys);
                }
            }
            PendingInput::Character {
                motion,
                count,
                operator,
                mut keys,
            } => {
                keys.push(character);
                self.state.last_character_motion = Some((motion, character));
                if let Some(target) = self.character_target(motion, character, count) {
                    if let Some(operator) = operator {
                        let range = self.character_operator_range(motion, target);
                        self.apply_operator(operator, range, false, keys);
                    } else {
                        self.move_to_position(target);
                    }
                }
            }
            PendingInput::TextObject {
                operator,
                count,
                scope,
                mut keys,
            } => {
                keys.push(character);
                if let Some(kind) = text_object_kind_for_key(character) {
                    if let Some(range) = self.resolver().text_object(scope, kind) {
                        if let Some(operator) = operator {
                            self.apply_operator(
                                operator,
                                range,
                                kind == TextObjectKind::Paragraph,
                                keys,
                            );
                        } else {
                            self.select_range(range, kind == TextObjectKind::Paragraph);
                        }
                    }
                } else if count > 0 {
                    self.state.count = None;
                }
            }
            PendingInput::GPrefix {
                operator,
                count,
                mut keys,
            } => {
                keys.push(character);
                match character {
                    'g' => {
                        let line = if count > 1 { usize::from(count - 1) } else { 0 };
                        if let Some(operator) = operator {
                            let range = self.linewise_range_to(line);
                            self.apply_operator(operator, range, true, keys);
                        } else {
                            self.move_to_line(line);
                        }
                    }
                    'e' | 'E' => {
                        if let Some(operator) = operator {
                            if let Some((range, _)) =
                                self.operator_motion_range(character, count, operator)
                            {
                                self.apply_operator(operator, range, false, keys);
                            }
                        } else if let Some(target) =
                            self.resolver()
                                .word_target(count, true, true, character == 'E')
                        {
                            self.move_to_position(target);
                        }
                    }
                    'J' if operator.is_none() => self.join_lines(count.max(2), true, keys),
                    'j' if operator.is_none() => {
                        self.repeat_motion(count, |area| area.move_vertical(1, layout));
                    }
                    'k' if operator.is_none() => {
                        self.repeat_motion(count, |area| area.move_vertical(-1, layout));
                    }
                    '0' if operator.is_none() => self.set_cursor(self.current_line_start()),
                    '$' if operator.is_none() => self.set_cursor(self.current_line_last_grapheme()),
                    _ => {}
                }
            }
            PendingInput::Replace { count, mut keys } => {
                keys.push(character);
                self.replace_characters(character, count, keys);
            }
            PendingInput::MacroRecord => {
                if character.is_ascii_alphanumeric() {
                    let register = character.to_ascii_lowercase();
                    let recorded = if character.is_ascii_uppercase() {
                        self.macro_registers.remove(&register).unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    self.state.recording = Some((register, recorded));
                }
            }
            PendingInput::MacroPlay { count } => {
                let register = if character == '@' {
                    self.state.last_macro
                } else {
                    Some(character.to_ascii_lowercase())
                };
                if let Some(register) = register {
                    self.state.last_macro = Some(register);
                    self.play_macro(register, count, layout);
                }
            }
        }
        TextAreaOutcome::Changed
    }

    fn operator_motion_range(
        &self,
        motion: char,
        count: u16,
        operator: Operator,
    ) -> Option<(TextRange, bool)> {
        let start = self.cursor_position();
        match motion {
            'w' | 'W' => self
                .resolver()
                .word_range(count, operator == Operator::Change, motion == 'W')
                .map(|range| (range, false)),
            'b' | 'B' => self
                .resolver()
                .word_target(count, true, false, motion == 'B')
                .map(|target| (TextRange::new(target, start), false)),
            'e' | 'E' => {
                let target = self
                    .resolver()
                    .word_target(count, false, true, motion == 'E')
                    .unwrap_or(start);
                let end = self
                    .buffer
                    .char_idx_to_position(self.buffer.position_to_char_idx(target) + 1);
                Some((TextRange::new(start, end), false))
            }
            'h' | 'l' => {
                let cursor = self.state.cursor;
                let target = if motion == 'h' {
                    cursor
                        .saturating_sub(usize::from(count))
                        .max(self.current_line_start())
                } else {
                    cursor
                        .saturating_add(usize::from(count))
                        .min(self.current_line_end())
                };
                if target == cursor {
                    None
                } else {
                    Some((
                        self.range_for_graphemes(cursor.min(target), cursor.max(target)),
                        false,
                    ))
                }
            }
            '0' | '^' => {
                let target = if motion == '^' {
                    self.first_non_blank_cursor()
                } else {
                    self.current_line_start()
                };
                (target != self.state.cursor)
                    .then(|| (self.range_for_graphemes(target, self.state.cursor), false))
            }
            '$' => {
                let line = start
                    .line
                    .saturating_add(usize::from(count.saturating_sub(1)))
                    .min(self.last_editable_line());
                Some((
                    TextRange::new(
                        start,
                        TextPosition::new(line, self.line_character_len(line)),
                    ),
                    false,
                ))
            }
            'j' | 'k' => {
                let target = if motion == 'k' {
                    start.line.saturating_sub(usize::from(count))
                } else {
                    start
                        .line
                        .saturating_add(usize::from(count))
                        .min(self.last_editable_line())
                };
                (target != start.line).then(|| (self.linewise_range_to(target), true))
            }
            'G' => {
                let line = if count == 1 {
                    self.last_editable_line()
                } else {
                    usize::from(count.saturating_sub(1))
                };
                Some((self.linewise_range_to(line), true))
            }
            _ => None,
        }
    }

    fn operate_current_lines(&mut self, operator: Operator, count: u16, keys: Vec<char>) {
        let line = self.cursor_position().line;
        let last = line
            .saturating_add(usize::from(count.saturating_sub(1)))
            .min(self.last_editable_line());
        let range = self.linewise_range(line, last);
        self.apply_operator(operator, range, true, keys);
    }

    fn linewise_range_to(&self, line: usize) -> TextRange {
        let current = self.cursor_position().line;
        self.linewise_range(current.min(line), current.max(line))
    }

    fn linewise_range(&self, first: usize, last: usize) -> TextRange {
        let last = last.min(self.last_editable_line());
        if last < self.last_editable_line()
            || self
                .buffer
                .get(last)
                .is_some_and(|line| line.ends_with('\n'))
        {
            TextRange::new(TextPosition::new(first, 0), TextPosition::new(last + 1, 0))
        } else if first > 0 {
            TextRange::new(
                TextPosition::new(first - 1, self.line_character_len(first - 1)),
                TextPosition::new(last, self.line_character_len(last)),
            )
        } else {
            TextRange::new(
                TextPosition::new(first, 0),
                TextPosition::new(last, self.line_character_len(last)),
            )
        }
    }

    fn apply_operator(
        &mut self,
        operator: Operator,
        range: TextRange,
        linewise: bool,
        keys: Vec<char>,
    ) {
        let text = self.buffer.text_in_range(range);
        if text.is_empty() {
            return;
        }
        self.register = RegisterContent { text, linewise };
        if operator == Operator::Yank {
            self.set_mode(Mode::Normal);
            return;
        }

        let start = self.grapheme_index_for_position(range.start);
        let end = self.grapheme_index_for_position(range.end);
        if operator == Operator::Change {
            self.begin_insert_transaction("operator edit");
        }
        if !self.replace_graphemes(start, end, "", start, "operator edit") {
            self.buffer.undo_history.cancel_transaction_if_empty();
            return;
        }
        if operator == Operator::Change {
            self.enter_insert(keys, true);
        } else {
            self.record_change(keys);
            self.clamp_normal_cursor();
            self.set_mode(Mode::Normal);
        }
    }

    fn apply_selection(&mut self, operator: Operator, keys: Vec<char>) {
        let Some(anchor) = self.state.selection_anchor else {
            return;
        };
        let linewise = self.state.mode == Mode::VisualLine;
        if self.state.mode == Mode::VisualBlock {
            self.apply_block_selection(operator, anchor, keys);
            return;
        }

        let range = if linewise {
            let first = self.position_for_grapheme(anchor).line;
            let last = self.cursor_position().line;
            self.linewise_range(first.min(last), first.max(last))
        } else {
            let first = anchor.min(self.state.cursor);
            let last = anchor.max(self.state.cursor).saturating_add(1);
            self.range_for_graphemes(first, last.min(grapheme_len(&self.text())))
        };
        self.apply_operator(operator, range, linewise, keys);
    }

    fn apply_block_selection(&mut self, operator: Operator, anchor: usize, keys: Vec<char>) {
        let start = self.position_for_grapheme(anchor);
        let end = self.cursor_position();
        let first_line = start.line.min(end.line);
        let last_line = start.line.max(end.line);
        let first_column = start.character.min(end.character);
        let last_column = start.character.max(end.character).saturating_add(1);
        let mut selected = Vec::new();
        let before = self.cursor_snapshot();
        if operator != Operator::Yank {
            self.buffer
                .undo_history
                .begin_transaction("visual block", before);
        }
        for line in first_line..=last_line {
            let line_len = self.line_character_len(line);
            if first_column >= line_len {
                continue;
            }
            let range = TextRange::new(
                TextPosition::new(line, first_column),
                TextPosition::new(line, last_column.min(line_len)),
            );
            selected.push(self.buffer.text_in_range(range));
            if operator != Operator::Yank {
                apply_transactional_replacement(&mut self.buffer, range, "");
            }
        }
        self.register = RegisterContent {
            text: selected.join("\n"),
            linewise: false,
        };
        if operator != Operator::Yank {
            self.move_to_position(TextPosition::new(first_line, first_column));
            if operator == Operator::Change {
                self.enter_insert(keys, true);
            } else {
                let after = self.cursor_snapshot();
                self.buffer.undo_history.commit_transaction(after);
                self.buffer.refresh_dirty_from_history();
                self.record_change(keys);
                self.set_mode(Mode::Normal);
            }
        } else {
            self.set_mode(Mode::Normal);
        }
    }

    fn move_word(&mut self, motion: char, count: u16) {
        let backward = matches!(motion, 'b' | 'B');
        let end = matches!(motion, 'e' | 'E');
        let big_word = matches!(motion, 'W' | 'B' | 'E');
        if let Some(position) = self.resolver().word_target(count, backward, end, big_word) {
            self.move_to_position(position);
        }
    }

    fn character_target(
        &self,
        motion: CharacterMotion,
        character: char,
        count: u16,
    ) -> Option<TextPosition> {
        let backward = matches!(
            motion,
            CharacterMotion::FindBackward | CharacterMotion::TillBackward
        );
        let mut target = self
            .resolver()
            .character_match(character, count, backward)?;
        match motion {
            CharacterMotion::Till => target.character = target.character.saturating_sub(1),
            CharacterMotion::TillBackward => target.character = target.character.saturating_add(1),
            CharacterMotion::Find | CharacterMotion::FindBackward => {}
        }
        Some(target)
    }

    fn character_operator_range(&self, motion: CharacterMotion, target: TextPosition) -> TextRange {
        let cursor = self.cursor_position();
        match motion {
            CharacterMotion::Find | CharacterMotion::Till => {
                let end = TextPosition::new(target.line, target.character + 1);
                TextRange::new(cursor, end)
            }
            CharacterMotion::FindBackward | CharacterMotion::TillBackward => {
                TextRange::new(target, cursor)
            }
        }
    }

    fn repeat_character_motion(&mut self, reverse: bool, count: u16) {
        let Some((mut motion, character)) = self.state.last_character_motion else {
            return;
        };
        if reverse {
            motion = match motion {
                CharacterMotion::Find => CharacterMotion::FindBackward,
                CharacterMotion::Till => CharacterMotion::TillBackward,
                CharacterMotion::FindBackward => CharacterMotion::Find,
                CharacterMotion::TillBackward => CharacterMotion::Till,
            };
        }
        if let Some(target) = self.character_target(motion, character, count) {
            self.move_to_position(target);
        }
    }

    fn delete_characters(&mut self, count: u16, backward: bool, keys: Vec<char>) {
        let cursor = self.state.cursor;
        let (start, end) = if backward {
            (
                cursor
                    .saturating_sub(usize::from(count))
                    .max(self.current_line_start()),
                cursor,
            )
        } else {
            (
                cursor,
                cursor
                    .saturating_add(usize::from(count))
                    .min(self.current_line_end()),
            )
        };
        if start == end {
            return;
        }
        self.register = RegisterContent {
            text: self
                .buffer
                .text_in_range(self.range_for_graphemes(start, end)),
            linewise: false,
        };
        if self.replace_graphemes(start, end, "", start, "delete characters") {
            self.clamp_normal_cursor();
            self.record_change(keys);
        }
    }

    fn change_characters(&mut self, count: u16, keys: Vec<char>) {
        let start = self.state.cursor;
        let end = start
            .saturating_add(usize::from(count))
            .min(self.current_line_end());
        if start == end {
            return;
        }
        let range = self.range_for_graphemes(start, end);
        self.apply_operator(Operator::Change, range, false, keys);
    }

    fn replace_characters(&mut self, character: char, count: u16, keys: Vec<char>) {
        if self.is_visual() {
            let Some(anchor) = self.state.selection_anchor else {
                return;
            };
            let start = anchor.min(self.state.cursor);
            let end = anchor.max(self.state.cursor).saturating_add(1);
            let replacement = character.to_string().repeat(end.saturating_sub(start));
            if self.replace_graphemes(start, end, &replacement, start, "replace selection") {
                self.record_change(keys);
                self.set_mode(Mode::Normal);
            }
            return;
        }

        let start = self.state.cursor;
        let end = start.saturating_add(usize::from(count));
        if end > self.current_line_end() {
            return;
        }
        let replacement = character.to_string().repeat(usize::from(count));
        let cursor = end.saturating_sub(1);
        if self.replace_graphemes(start, end, &replacement, cursor, "replace characters") {
            self.record_change(keys);
        }
    }

    fn paste(&mut self, before: bool, count: u16, keys: Vec<char>) {
        if self.register.text.is_empty() {
            return;
        }
        if self.is_visual() {
            let Some(anchor) = self.state.selection_anchor else {
                return;
            };
            let start = anchor.min(self.state.cursor);
            let end = anchor.max(self.state.cursor).saturating_add(1);
            let text = self.register.text.repeat(usize::from(count));
            if self.replace_graphemes(start, end, &text, start, "paste selection") {
                self.record_change(keys);
                self.set_mode(Mode::Normal);
            }
            return;
        }

        let text = self.register.text.repeat(usize::from(count));
        let position = if self.register.linewise {
            if before {
                self.current_line_start()
            } else {
                let end = self.current_line_end();
                if end < grapheme_len(&self.text()) {
                    end + 1
                } else {
                    end
                }
            }
        } else if before {
            self.state.cursor
        } else {
            self.state
                .cursor
                .saturating_add(1)
                .min(self.current_line_end())
        };
        let inserted = if self.register.linewise && !text.ends_with('\n') {
            format!("{text}\n")
        } else {
            text
        };
        let cursor = position
            .saturating_add(grapheme_len(&inserted))
            .saturating_sub(1);
        if self.replace_graphemes(position, position, &inserted, cursor, "paste") {
            self.record_change(keys);
            self.clamp_normal_cursor();
        }
    }

    fn toggle_case(&mut self, count: u16, keys: Vec<char>) {
        let start = self.state.cursor;
        let end = start
            .saturating_add(usize::from(count))
            .min(self.current_line_end());
        if start == end {
            return;
        }
        let range = self.range_for_graphemes(start, end);
        let original = self.buffer.text_in_range(range);
        let transformed = original
            .chars()
            .flat_map(|character| {
                if character.is_lowercase() {
                    character.to_uppercase().collect::<Vec<_>>()
                } else {
                    character.to_lowercase().collect::<Vec<_>>()
                }
            })
            .collect::<String>();
        if self.replace_graphemes(
            start,
            end,
            &transformed,
            end.saturating_sub(1),
            "toggle case",
        ) {
            self.record_change(keys);
        }
    }

    fn join_lines(&mut self, count: u16, keep_spaces: bool, keys: Vec<char>) {
        let first = self.cursor_position().line;
        let last = first
            .saturating_add(usize::from(count.saturating_sub(1)))
            .min(self.last_editable_line());
        if first == last {
            return;
        }
        let start = TextPosition::new(first, 0);
        let end = TextPosition::new(last, self.line_character_len(last));
        let original = self.buffer.text_in_range(TextRange::new(start, end));
        let mut lines = original.split('\n');
        let mut joined = lines.next().unwrap_or_default().to_string();
        let join_cursor = grapheme_len(&joined);
        for line in lines {
            if keep_spaces {
                joined.push_str(line);
            } else {
                if !joined.ends_with(char::is_whitespace) && !line.trim_start().starts_with(')') {
                    joined.push(' ');
                }
                joined.push_str(line.trim_start());
            }
        }
        let absolute_start = self.grapheme_index_for_position(start);
        let absolute_end = self.grapheme_index_for_position(end);
        if self.replace_graphemes(
            absolute_start,
            absolute_end,
            &joined,
            absolute_start.saturating_add(join_cursor),
            "join lines",
        ) {
            self.record_change(keys);
            self.clamp_normal_cursor();
        }
    }

    fn open_line(&mut self, below: bool, key: char) {
        let position = if below {
            self.current_line_end()
        } else {
            self.current_line_start()
        };
        let cursor = if below { position + 1 } else { position };
        let before = self.cursor_snapshot();
        if self.replace_graphemes(position, position, "\n", cursor, "open line") {
            self.buffer
                .undo_history
                .begin_transaction("insert text", before);
            self.enter_insert(vec![key], true);
        }
    }

    fn toggle_visual(&mut self, mode: Mode) {
        if self.state.mode == mode {
            self.set_mode(Mode::Normal);
            return;
        }
        if !self.is_visual() {
            self.state.selection_anchor = Some(self.state.cursor);
        }
        self.state.mode = mode;
        self.state.pending = None;
    }

    fn select_range(&mut self, range: TextRange, linewise: bool) {
        self.state.selection_anchor = Some(self.grapheme_index_for_position(range.start));
        let end = self
            .grapheme_index_for_position(range.end)
            .saturating_sub(1);
        self.set_cursor(end);
        self.state.mode = if linewise {
            Mode::VisualLine
        } else {
            Mode::Visual
        };
    }

    fn move_to_matching_delimiter(&mut self) {
        let text = self.text();
        let cursor_byte = grapheme_to_byte(&text, self.state.cursor);
        let Some(character) = text[cursor_byte..].chars().next() else {
            return;
        };
        let (open, close, forward) = match character {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => return,
        };
        let chars = text.chars().collect::<Vec<_>>();
        let cursor = text[..cursor_byte].chars().count();
        let mut depth = 0usize;
        let indices: Box<dyn Iterator<Item = usize>> = if forward {
            Box::new(cursor..chars.len())
        } else {
            Box::new((0..=cursor).rev())
        };
        for index in indices {
            if chars[index] == if forward { open } else { close } {
                depth += 1;
            } else if chars[index] == if forward { close } else { open } {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    self.move_to_position(self.buffer.char_idx_to_position(index));
                    break;
                }
            }
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> TextAreaOutcome {
        match key.code {
            KeyCode::Esc => {
                if let Some(search) = self.state.search.take() {
                    self.set_cursor(search.origin);
                }
                self.state.mode = Mode::Normal;
                TextAreaOutcome::Changed
            }
            KeyCode::Enter => {
                if let Some(search) = self.state.search.take() {
                    if !search.pattern.is_empty() {
                        self.state.last_search = Some(search.pattern.clone());
                        self.find_search(&search.pattern, search.backward);
                    }
                }
                self.state.mode = Mode::Normal;
                TextAreaOutcome::Changed
            }
            KeyCode::Backspace => {
                if let Some(search) = self.state.search.as_mut() {
                    search.pattern.pop();
                }
                TextAreaOutcome::Changed
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(search) = self.state.search.as_mut() {
                    search.pattern.push(character);
                }
                TextAreaOutcome::Changed
            }
            _ => TextAreaOutcome::Unhandled,
        }
    }

    fn repeat_search(&mut self, backward: bool, count: u16) {
        let Some(pattern) = self.state.last_search.clone() else {
            return;
        };
        for _ in 0..count {
            self.find_search(&pattern, backward);
        }
    }

    fn find_search(&mut self, pattern: &str, backward: bool) {
        let contents = self.text();
        let cursor = grapheme_to_byte(&contents, self.state.cursor);
        let found = if backward {
            contents[..cursor].rfind(pattern).or_else(|| {
                contents[cursor..]
                    .rfind(pattern)
                    .map(|index| cursor + index)
            })
        } else {
            let start =
                cursor.saturating_add(contents[cursor..].chars().next().map_or(0, char::len_utf8));
            contents[start..]
                .find(pattern)
                .map(|index| start + index)
                .or_else(|| contents[..start].find(pattern))
        };
        if let Some(index) = found {
            self.set_cursor(grapheme_len(&contents[..index]));
        }
    }

    fn enter_insert(&mut self, keys: Vec<char>, existing_change: bool) {
        self.begin_insert_transaction("insert text");
        self.state.mode = Mode::Insert;
        self.state.selection_anchor = None;
        self.insert_recipe = Some(keys);
        if existing_change && !self.replaying {
            self.state.last_change = self.insert_recipe.clone();
        }
    }

    fn record_insert_character(&mut self, character: char) {
        if let Some(recipe) = self.insert_recipe.as_mut() {
            recipe.push(character);
        }
    }

    fn finish_insert_recipe(&mut self) {
        if self.buffer.undo_history.is_transaction_active() {
            let after = self.cursor_snapshot();
            self.buffer.undo_history.commit_transaction(after);
            self.buffer.refresh_dirty_from_history();
        }
        if let Some(mut recipe) = self.insert_recipe.take() {
            if recipe.len() > 1 {
                recipe.push('\u{1b}');
                self.record_change(recipe);
            }
        }
    }

    fn record_change(&mut self, keys: Vec<char>) {
        if !self.replaying && !keys.is_empty() {
            self.state.last_change = Some(keys);
        }
    }

    fn repeat_last_change(&mut self, count: u16, layout: LayoutOptions) {
        let Some(recipe) = self.state.last_change.clone() else {
            return;
        };
        let previous_replaying = self.replaying;
        self.replaying = true;
        for _ in 0..count {
            self.replay_keys(&recipe, layout);
        }
        self.replaying = previous_replaying;
        self.state.last_change = Some(recipe);
    }

    fn play_macro(&mut self, register: char, count: u16, layout: LayoutOptions) {
        let Some(recipe) = self.macro_registers.get(&register).cloned() else {
            return;
        };
        let previous_replaying = self.replaying;
        self.replaying = true;
        for _ in 0..count {
            self.replay_keys(&recipe, layout);
        }
        self.replaying = previous_replaying;
    }

    fn replay_keys(&mut self, recipe: &[char], layout: LayoutOptions) {
        for character in recipe.iter().take(MAX_MACRO_EVENTS) {
            let code = if *character == '\u{1b}' {
                KeyCode::Esc
            } else {
                KeyCode::Char(*character)
            };
            self.handle_key(KeyEvent::new(code, KeyModifiers::NONE), layout);
        }
    }

    fn repeat_motion(&mut self, count: u16, mut motion: impl FnMut(&mut Self)) {
        for _ in 0..count {
            motion(self);
        }
    }

    fn move_horizontal(&mut self, direction: isize) {
        let cursor = self.state.cursor.saturating_add_signed(direction);
        let cursor = if self.state.mode != Mode::Insert {
            cursor.clamp(self.current_line_start(), self.current_line_last_grapheme())
        } else {
            cursor
        };
        self.set_cursor(cursor);
    }

    fn move_vertical(&mut self, direction: isize, options: LayoutOptions) {
        let layout = TextLayout::new(&self.text(), options);
        let Some(position) = layout.position(self.state.cursor) else {
            return;
        };
        let row = position.row;
        let column = position.column;
        let target = row.saturating_add_signed(direction);
        if target == row {
            return;
        }
        let preferred = *self.state.preferred_column.get_or_insert(column);
        if let Some(index) = layout.nearest_offset_on_row(target, preferred) {
            self.state.cursor = index;
            if self.state.mode != Mode::Insert {
                self.clamp_normal_cursor();
            }
            self.sync_buffer_cursor();
        }
    }

    fn move_to_line(&mut self, line: usize) {
        let line = line.min(self.last_editable_line());
        self.move_to_position(TextPosition::new(line, 0));
    }

    fn move_to_line_end(&mut self, count: u16) {
        let line = self
            .cursor_position()
            .line
            .saturating_add(usize::from(count.saturating_sub(1)))
            .min(self.last_editable_line());
        let end = self.line_character_len(line).saturating_sub(1);
        self.move_to_position(TextPosition::new(line, end));
    }

    fn move_to_first_non_blank(&mut self) {
        self.set_cursor(self.first_non_blank_cursor());
    }

    fn first_non_blank_cursor(&self) -> usize {
        let line = self.cursor_position().line;
        let Some(text) = self.buffer.get(line) else {
            return self.current_line_start();
        };
        let prefix = trim_line_ending(&text)
            .graphemes(true)
            .take_while(|grapheme| grapheme.chars().all(char::is_whitespace))
            .count();
        self.current_line_start().saturating_add(prefix)
    }

    fn current_line_start(&self) -> usize {
        let text = self.text();
        let byte = grapheme_to_byte(&text, self.state.cursor);
        text[..byte]
            .rfind('\n')
            .map_or(0, |index| grapheme_len(&text[..index + 1]))
    }

    fn current_line_end(&self) -> usize {
        let text = self.text();
        let byte = grapheme_to_byte(&text, self.state.cursor);
        text[byte..].find('\n').map_or_else(
            || grapheme_len(&text),
            |index| grapheme_len(&text[..byte + index]),
        )
    }

    fn current_line_last_grapheme(&self) -> usize {
        self.current_line_end()
            .saturating_sub(1)
            .max(self.current_line_start())
    }

    fn clamp_normal_cursor(&mut self) {
        if self.state.mode != Mode::Insert {
            self.state.cursor = self
                .state
                .cursor
                .clamp(self.current_line_start(), self.current_line_last_grapheme());
            self.sync_buffer_cursor();
        }
    }

    fn is_visual(&self) -> bool {
        matches!(
            self.state.mode,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
    }

    fn resolver(&self) -> MotionResolver<'_> {
        MotionResolver::new(&self.buffer, self.cursor_position())
    }

    fn cursor_position(&self) -> TextPosition {
        self.position_for_grapheme(self.state.cursor)
    }

    fn position_for_grapheme(&self, index: usize) -> TextPosition {
        let text = self.text();
        let byte = grapheme_to_byte(&text, index);
        self.buffer
            .char_idx_to_position(text[..byte].chars().count())
    }

    fn grapheme_index_for_position(&self, position: TextPosition) -> usize {
        let text = self.text();
        let index = self.buffer.position_to_char_idx(position);
        let byte = text
            .char_indices()
            .nth(index)
            .map_or(text.len(), |(byte, _)| byte);
        grapheme_len(&text[..byte])
    }

    fn move_to_position(&mut self, position: TextPosition) {
        self.set_cursor(self.grapheme_index_for_position(position));
        if self.state.mode != Mode::Insert {
            self.clamp_normal_cursor();
        }
    }

    fn range_for_graphemes(&self, start: usize, end: usize) -> TextRange {
        TextRange::new(
            self.position_for_grapheme(start),
            self.position_for_grapheme(end),
        )
    }

    fn line_character_len(&self, line: usize) -> usize {
        self.buffer
            .get(line)
            .map(|contents| trim_line_ending(&contents).chars().count())
            .unwrap_or_default()
    }

    fn last_editable_line(&self) -> usize {
        self.buffer.len()
    }

    fn replace_graphemes(
        &mut self,
        start: usize,
        end: usize,
        replacement: &str,
        cursor: usize,
        label: &str,
    ) -> bool {
        let contents = self.text();
        let start_byte = grapheme_to_byte(&contents, start);
        let end_byte = grapheme_to_byte(&contents, end);
        let previous = &contents[start_byte..end_byte];
        if previous == replacement
            || contents
                .len()
                .saturating_sub(previous.len())
                .saturating_add(replacement.len())
                > self.max_bytes
        {
            return false;
        }

        let range = TextRange::new(
            self.buffer
                .char_idx_to_position(contents[..start_byte].chars().count()),
            self.buffer
                .char_idx_to_position(contents[..end_byte].chars().count()),
        );
        let started_transaction = !self.buffer.undo_history.is_transaction_active();
        if started_transaction {
            let before = self.cursor_snapshot();
            self.buffer.undo_history.begin_transaction(label, before);
        }
        apply_transactional_replacement(&mut self.buffer, range, replacement);
        self.state.cursor = cursor.min(grapheme_len(&self.text()));
        self.state.preferred_column = None;
        self.sync_buffer_cursor();
        if started_transaction {
            let after = self.cursor_snapshot();
            self.buffer.undo_history.commit_transaction(after);
            self.buffer.refresh_dirty_from_history();
        }
        true
    }

    fn begin_insert_transaction(&mut self, label: &str) {
        if !self.buffer.undo_history.is_transaction_active() {
            let before = self.cursor_snapshot();
            self.buffer.undo_history.begin_transaction(label, before);
        }
    }

    fn cursor_snapshot(&self) -> CursorSnapshot {
        let position = self.cursor_position();
        let line = self.buffer.get(position.line).unwrap_or_default();
        CursorSnapshot::new(
            char_to_grapheme(&line, position.character),
            position.line,
            0,
        )
    }

    fn sync_buffer_cursor(&mut self) {
        let snapshot = self.cursor_snapshot();
        self.buffer.pos = (snapshot.x, snapshot.y);
    }

    fn restore_cursor(&mut self, snapshot: CursorSnapshot) {
        let prefix = self.buffer.line_range_contents(0, snapshot.y);
        let line = self.buffer.get(snapshot.y).unwrap_or_default();
        self.state.cursor = grapheme_len(&prefix)
            .saturating_add(snapshot.x.min(grapheme_len(&line)))
            .min(grapheme_len(&self.text()));
        self.state.preferred_column = None;
        self.sync_buffer_cursor();
    }
}

fn character_motion(character: char) -> CharacterMotion {
    match character {
        'f' => CharacterMotion::Find,
        't' => CharacterMotion::Till,
        'F' => CharacterMotion::FindBackward,
        'T' => CharacterMotion::TillBackward,
        _ => unreachable!("character search is validated by the input parser"),
    }
}

fn operator_for_character(character: char) -> Operator {
    match character {
        'd' => Operator::Delete,
        'c' => Operator::Change,
        'y' => Operator::Yank,
        _ => unreachable!("operator is validated by the input parser"),
    }
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn unnamed_buffer(text: &str) -> Buffer {
    if !text.is_empty() {
        return Buffer::new(None, text.to_string());
    }
    let mut buffer = Buffer::new(None, "\n".to_string());
    buffer.replace_range_raw(
        TextRange::new(TextPosition::new(0, 0), TextPosition::new(1, 0)),
        "",
    );
    buffer.dirty = false;
    buffer
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::{TextArea, TextAreaOutcome};
    use crate::editor::Mode;

    fn keys(area: &mut TextArea, input: &str) {
        for character in input.chars() {
            let code = if character == '\u{1b}' {
                KeyCode::Esc
            } else {
                KeyCode::Char(character)
            };
            assert_eq!(
                area.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 80),
                TextAreaOutcome::Changed,
                "key {character:?}"
            );
        }
    }

    fn normal(text: &str) -> TextArea {
        let mut area = TextArea::new(text);
        area.set_cursor(0);
        area.set_mode(Mode::Normal);
        area
    }

    #[test]
    fn operators_counts_text_objects_and_change_preserve_editor_word_semantics() {
        let mut area = normal("first,  second third");
        keys(&mut area, "2dw");
        assert_eq!(area.text(), "second third");
        keys(&mut area, "u");
        assert_eq!(area.text(), "first,  second third");

        let mut area = normal("alpha (first second) omega");
        keys(&mut area, "f(ci(");
        assert_eq!(area.text(), "alpha () omega");
        assert_eq!(area.mode(), Mode::Insert);
        keys(&mut area, "new\u{1b}");
        assert_eq!(area.text(), "alpha (new) omega");
    }

    #[test]
    fn character_search_and_reverse_repeat_share_exact_line_boundaries() {
        let mut area = normal("alpha beta gamma");
        keys(&mut area, "fa;");
        assert_eq!(area.cursor(), 9);
        keys(&mut area, ",");
        assert_eq!(area.cursor(), 4);

        let mut area = normal("alpha beta gamma");
        keys(&mut area, "dta");
        assert_eq!(area.text(), "a beta gamma");
    }

    #[test]
    fn visual_selection_yank_paste_and_dot_repeat_are_surface_local() {
        let mut area = normal("first second third");
        keys(&mut area, "viwy");
        assert_eq!(area.register().text, "first");
        keys(&mut area, "wp");
        assert_eq!(area.text(), "first sfirstecond third");

        let mut area = normal("one two three");
        keys(&mut area, "dw.");
        assert_eq!(area.text(), "three");
    }

    #[test]
    fn unicode_text_objects_and_visual_line_edits_preserve_undo() {
        let mut area = normal("e\u{301}clair 👨‍👩‍👧 tail");
        keys(&mut area, "diw");
        assert_eq!(area.text(), " 👨‍👩‍👧 tail");
        keys(&mut area, "u");
        assert_eq!(area.text(), "e\u{301}clair 👨‍👩‍👧 tail");

        let mut area = normal("first\nsecond\nthird");
        keys(&mut area, "Vjd");
        assert_eq!(area.text(), "third");
        keys(&mut area, "u");
        assert_eq!(area.text(), "first\nsecond\nthird");
    }

    #[test]
    fn macros_and_local_search_do_not_invoke_editor_or_plugin_commands() {
        let mut area = normal("one two three");
        keys(&mut area, "qadwq@a");
        assert_eq!(area.text(), "three");

        let mut area = normal("one two one");
        keys(&mut area, "/one");
        assert_eq!(area.mode(), Mode::Search);
        assert_eq!(
            area.handle_event(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                80,
            ),
            TextAreaOutcome::Changed
        );
        assert_eq!(area.cursor(), 8);
        assert_eq!(area.mode(), Mode::Normal);
        assert_eq!(
            area.handle_event(
                &Event::Key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE)),
                80,
            ),
            TextAreaOutcome::Unhandled
        );
    }
}
