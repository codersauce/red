//! Buffer-backed Vim-style editing shared by floating and docked agent composers.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    buffer::Buffer,
    undo::{CursorSnapshot, TextPosition, TextRange},
    unicode_utils::{char_to_byte, char_to_grapheme, grapheme_to_char, trim_line_ending},
};

/// Largest prompt that remains safely below the app-server's JSON frame limit.
pub(crate) const MAX_PROMPT_BYTES: usize = 128 * 1024;
const MAX_PROMPT_HISTORY: usize = 50;

const OVERSIZED_STATUS: &str = "Prompt exceeds 128 KiB";
const EMPTY_STATUS: &str = "Prompt is empty";

/// Vim mode of an agent prompt's independent, in-memory editor buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ModalComposerMode {
    /// Navigate and operate on the draft without inserting typed commands.
    Normal,
    /// Insert characters and multiline text into the draft.
    #[default]
    Insert,
    /// Select text using the same motions as normal mode.
    Visual,
}

impl ModalComposerMode {
    /// Returns a compact, user-facing Vim-mode label.
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
        }
    }
}

/// Result of handling one key or bracketed paste in the shared prompt editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalComposerOutcome {
    /// Text, cursor position, mode, history, or validation state changed.
    Changed,
    /// The nonempty prompt should be submitted by the owning surface.
    Submit,
    /// The owning surface should handle this key, such as `Ctrl-C`.
    Unhandled,
    /// The operation was rejected without changing the existing draft.
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOperator {
    Delete,
    Change,
    Yank,
}

/// An independent real editor buffer with Vim motions, operators, and undo history.
///
/// The synthetic file name is never registered with the editor and never touches disk;
/// it keeps `Buffer::new` from manufacturing a newline for an empty unnamed buffer.
#[derive(Debug)]
pub(crate) struct ModalComposer {
    buffer: Buffer,
    cursor: TextPosition,
    mode: ModalComposerMode,
    visual_anchor: Option<TextPosition>,
    preferred_column: Option<usize>,
    pending_operator: Option<PendingOperator>,
    pending_text_object: bool,
    pending_g: bool,
    register: String,
    history: Vec<String>,
    history_position: Option<usize>,
    history_draft: Option<String>,
    validation_status: Option<&'static str>,
}

impl ModalComposer {
    /// Creates an insert-mode scratch buffer, retaining only safe history entries.
    #[must_use]
    pub(crate) fn new(text: &str, history: Vec<String>) -> Self {
        let normalized = normalize_newlines(text);
        let initial_too_large = normalized.len() > MAX_PROMPT_BYTES;
        let contents = if initial_too_large { "" } else { &normalized };
        let original_history_len = history.len();
        let history = history
            .into_iter()
            .filter_map(|entry| {
                let entry = normalize_newlines(&entry);
                (entry.len() <= MAX_PROMPT_BYTES).then_some(entry)
            })
            .collect::<Vec<_>>();
        let history_too_large = history.len() != original_history_len;
        let buffer = Self::scratch_buffer(contents);
        let cursor = buffer.char_idx_to_position(contents.chars().count());

        Self {
            buffer,
            cursor,
            mode: ModalComposerMode::Insert,
            visual_anchor: None,
            preferred_column: None,
            pending_operator: None,
            pending_text_object: false,
            pending_g: false,
            register: String::new(),
            history,
            history_position: None,
            history_draft: None,
            validation_status: (initial_too_large || history_too_large).then_some(OVERSIZED_STATUS),
        }
    }

    fn scratch_buffer(contents: &str) -> Buffer {
        Buffer::new(
            Some("red-buffer://agent-composer".to_string()),
            contents.to_string(),
        )
    }

    /// Returns the complete authoritative draft from the real editor buffer.
    #[must_use]
    pub(crate) fn contents(&self) -> String {
        self.buffer.contents()
    }

    /// Returns the cursor's grapheme column and zero-based logical line.
    #[must_use]
    pub(crate) fn cursor(&self) -> (usize, usize) {
        let line = self.line_text(self.cursor.line);
        (
            char_to_grapheme(&line, self.cursor.character),
            self.cursor.line,
        )
    }

    /// Returns the cursor's grapheme offset in the complete multiline draft.
    #[must_use]
    pub(crate) fn cursor_grapheme_index(&self) -> usize {
        let contents = self.buffer.contents();
        let character = self.buffer.position_to_char_idx(self.cursor);
        let byte = char_to_byte(&contents, character);
        crate::unicode_utils::grapheme_len(&contents[..byte])
    }

    /// Places the cursor at a complete grapheme in the multiline prompt.
    pub(crate) fn set_cursor_grapheme_index(&mut self, index: usize) {
        let contents = self.contents();
        let index = index.min(crate::unicode_utils::grapheme_len(&contents));
        let character = grapheme_to_char(&contents, index);
        self.cursor = self.buffer.char_idx_to_position(character);
        self.preferred_column = None;
        self.clamp_cursor();
    }

    /// Returns the currently active Vim mode.
    #[must_use]
    pub(crate) fn mode(&self) -> ModalComposerMode {
        self.mode
    }

    /// Returns a validation message for an empty or oversized operation.
    #[must_use]
    pub(crate) fn validation_status(&self) -> Option<&'static str> {
        self.validation_status
    }

    /// Returns the current inclusive visual selection as a half-open buffer range.
    #[must_use]
    pub(crate) fn selection_range(&self) -> Option<TextRange> {
        let anchor = self.visual_anchor?;
        let (start, last) = if Self::position_key(anchor) <= Self::position_key(self.cursor) {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some(TextRange::new(start, self.next_position(last)))
    }

    /// Replaces the draft without changing the caller's current Vim mode.
    ///
    /// Returns `false` and preserves the old draft when `text` is oversized.
    pub(crate) fn set_contents(&mut self, text: &str) -> bool {
        let normalized = normalize_newlines(text);
        if normalized.len() > MAX_PROMPT_BYTES {
            self.validation_status = Some(OVERSIZED_STATUS);
            return false;
        }
        self.buffer = Self::scratch_buffer(&normalized);
        self.cursor = self.buffer.char_idx_to_position(normalized.chars().count());
        self.visual_anchor = None;
        self.preferred_column = None;
        self.pending_operator = None;
        self.pending_text_object = false;
        self.pending_g = false;
        self.validation_status = None;
        self.clamp_cursor();
        true
    }

    /// Inserts a complete bracketed paste as one bounded, undoable edit.
    pub(crate) fn handle_paste(&mut self, text: &str) -> ModalComposerOutcome {
        self.insert_text(text)
    }

    /// Handles editor-native Vim navigation and mode-aware prompt submission.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ModalComposerOutcome {
        if matches!(key.code, KeyCode::Enter | KeyCode::Char('\n' | '\r'))
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return self.submit();
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.handle_control_key(key.code);
        }

        match self.mode {
            ModalComposerMode::Insert => self.handle_insert_key(key),
            ModalComposerMode::Normal => self.handle_normal_key(key),
            ModalComposerMode::Visual => self.handle_visual_key(key),
        }
    }

    /// Takes a validated prompt, remembers it, and starts a fresh insert-mode draft.
    pub(crate) fn take_submission(&mut self) -> Option<String> {
        if self.submit() != ModalComposerOutcome::Submit {
            return None;
        }

        let text = self.contents();
        self.history.retain(|entry| entry != &text);
        self.history.insert(0, text.clone());
        self.history.truncate(MAX_PROMPT_HISTORY);
        self.history_position = None;
        self.history_draft = None;
        self.mode = ModalComposerMode::Insert;
        self.set_contents("");
        Some(text)
    }

    /// Recalls the next older prompt while retaining the unsubmitted draft.
    pub(crate) fn history_back(&mut self) -> ModalComposerOutcome {
        if self.history.is_empty() {
            return ModalComposerOutcome::Changed;
        }
        let position = match self.history_position {
            Some(position) => (position + 1).min(self.history.len() - 1),
            None => {
                self.history_draft = Some(self.contents());
                0
            }
        };
        self.history_position = Some(position);
        let entry = self.history[position].clone();
        self.set_contents(&entry);
        self.history_position = Some(position);
        ModalComposerOutcome::Changed
    }

    /// Recalls a newer prompt, eventually restoring the original draft.
    pub(crate) fn history_forward(&mut self) -> ModalComposerOutcome {
        let Some(position) = self.history_position else {
            return ModalComposerOutcome::Changed;
        };
        if position == 0 {
            let draft = self.history_draft.take().unwrap_or_default();
            self.set_contents(&draft);
            self.history_position = None;
        } else {
            let next = position - 1;
            let entry = self.history[next].clone();
            self.set_contents(&entry);
            self.history_position = Some(next);
        }
        ModalComposerOutcome::Changed
    }

    fn handle_control_key(&mut self, code: KeyCode) -> ModalComposerOutcome {
        match code {
            KeyCode::Char('p' | 'P') => self.history_back(),
            KeyCode::Char('n' | 'N') => self.history_forward(),
            KeyCode::Char('j' | 'J') => self.insert_text("\n"),
            KeyCode::Char('w' | 'W') if self.mode == ModalComposerMode::Insert => {
                self.delete_previous_word()
            }
            KeyCode::Char('r' | 'R') if self.mode == ModalComposerMode::Normal => self.redo(),
            _ => ModalComposerOutcome::Unhandled,
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) -> ModalComposerOutcome {
        match key.code {
            KeyCode::Esc => {
                self.finish_transaction();
                self.mode = ModalComposerMode::Normal;
                self.clamp_cursor();
                ModalComposerOutcome::Changed
            }
            KeyCode::Enter => self.insert_text("\n"),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.move_horizontal(-1),
            KeyCode::Right => self.move_horizontal(1),
            KeyCode::Up => self.move_vertical(-1),
            KeyCode::Down => self.move_vertical(1),
            KeyCode::Home => self.move_line_start(),
            KeyCode::End => self.move_line_end(),
            KeyCode::Tab => self.insert_text("\t"),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert_text(&character.to_string())
            }
            _ => ModalComposerOutcome::Unhandled,
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> ModalComposerOutcome {
        if self.pending_operator.is_some() {
            return self.handle_operator_key(key);
        }
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => {
                self.pending_g = false;
                ModalComposerOutcome::Changed
            }
            KeyCode::Left | KeyCode::Char('h') => self.move_horizontal(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_horizontal(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_vertical(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_vertical(1),
            KeyCode::Home | KeyCode::Char('0') => self.move_line_start(),
            KeyCode::End | KeyCode::Char('$') => self.move_line_end(),
            KeyCode::Char('^') => self.move_first_non_whitespace(),
            KeyCode::Char('w') => self.move_word_forward(),
            KeyCode::Char('b') => self.move_word_backward(),
            KeyCode::Char('e') => self.move_word_end(),
            KeyCode::Char('i') => self.enter_insert(),
            KeyCode::Char('a') => {
                self.move_horizontal(1);
                self.enter_insert()
            }
            KeyCode::Char('I') => {
                self.move_first_non_whitespace();
                self.enter_insert()
            }
            KeyCode::Char('A') => {
                self.move_line_end();
                self.enter_insert()
            }
            KeyCode::Char('o') => {
                self.move_line_end();
                self.mode = ModalComposerMode::Insert;
                self.insert_text("\n")
            }
            KeyCode::Char('O') => {
                let line = self.cursor.line;
                self.cursor = TextPosition::new(line, 0);
                self.mode = ModalComposerMode::Insert;
                let outcome = self.insert_text("\n");
                self.cursor = TextPosition::new(line, 0);
                outcome
            }
            KeyCode::Char('v') => {
                self.visual_anchor = Some(self.cursor);
                self.mode = ModalComposerMode::Visual;
                ModalComposerOutcome::Changed
            }
            KeyCode::Char('d') => self.begin_operator(PendingOperator::Delete),
            KeyCode::Char('c') => self.begin_operator(PendingOperator::Change),
            KeyCode::Char('y') => self.begin_operator(PendingOperator::Yank),
            KeyCode::Char('x') | KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('p') => self.paste_register(),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('g') => {
                if self.pending_g {
                    self.pending_g = false;
                    self.cursor = TextPosition::new(0, 0);
                } else {
                    self.pending_g = true;
                }
                ModalComposerOutcome::Changed
            }
            KeyCode::Char('G') => {
                self.pending_g = false;
                self.cursor = TextPosition::new(self.buffer.len(), 0);
                self.clamp_cursor();
                ModalComposerOutcome::Changed
            }
            _ => {
                self.pending_g = false;
                ModalComposerOutcome::Unhandled
            }
        }
    }

    fn handle_visual_key(&mut self, key: KeyEvent) -> ModalComposerOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('v') => {
                self.visual_anchor = None;
                self.mode = ModalComposerMode::Normal;
                self.clamp_cursor();
                ModalComposerOutcome::Changed
            }
            KeyCode::Char('d' | 'x') => self.apply_visual_operator(PendingOperator::Delete),
            KeyCode::Char('c') => self.apply_visual_operator(PendingOperator::Change),
            KeyCode::Char('y') => self.apply_visual_operator(PendingOperator::Yank),
            KeyCode::Left | KeyCode::Char('h') => self.move_horizontal(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_horizontal(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_vertical(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_vertical(1),
            KeyCode::Home | KeyCode::Char('0') => self.move_line_start(),
            KeyCode::End | KeyCode::Char('$') => self.move_line_end(),
            KeyCode::Char('w') => self.move_word_forward(),
            KeyCode::Char('b') => self.move_word_backward(),
            KeyCode::Char('e') => self.move_word_end(),
            _ => ModalComposerOutcome::Unhandled,
        }
    }

    fn submit(&mut self) -> ModalComposerOutcome {
        if self.contents().trim().is_empty() {
            self.validation_status = Some(EMPTY_STATUS);
            return ModalComposerOutcome::Rejected;
        }
        self.finish_transaction();
        self.validation_status = None;
        ModalComposerOutcome::Submit
    }

    fn enter_insert(&mut self) -> ModalComposerOutcome {
        self.mode = ModalComposerMode::Insert;
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn insert_text(&mut self, text: &str) -> ModalComposerOutcome {
        let normalized = normalize_newlines(text);
        if normalized.is_empty() {
            return ModalComposerOutcome::Changed;
        }
        if normalized.len() > MAX_PROMPT_BYTES.saturating_sub(self.buffer.byte_len()) {
            self.validation_status = Some(OVERSIZED_STATUS);
            return ModalComposerOutcome::Rejected;
        }
        let range = TextRange::insertion(self.cursor);
        self.replace_range(range, &normalized, "insert prompt text");
        ModalComposerOutcome::Changed
    }

    fn backspace(&mut self) -> ModalComposerOutcome {
        let end = self.cursor;
        let Some(start) = self.previous_position(end) else {
            return ModalComposerOutcome::Changed;
        };
        self.replace_range(TextRange::new(start, end), "", "delete prompt text");
        ModalComposerOutcome::Changed
    }

    fn delete_forward(&mut self) -> ModalComposerOutcome {
        let start = self.cursor;
        let end = self.next_position(start);
        if start == end {
            return ModalComposerOutcome::Changed;
        }
        self.register = self.buffer.text_in_range(TextRange::new(start, end));
        self.replace_range(TextRange::new(start, end), "", "delete prompt text");
        ModalComposerOutcome::Changed
    }

    fn delete_previous_word(&mut self) -> ModalComposerOutcome {
        let end = self.cursor;
        let Some((character, line)) = self
            .buffer
            .find_prev_word((self.cursor.character, self.cursor.line))
        else {
            if self.cursor.character == 0 && self.cursor.line == 0 {
                return ModalComposerOutcome::Changed;
            }
            self.replace_range(
                TextRange::new(TextPosition::new(0, 0), end),
                "",
                "delete previous prompt word",
            );
            return ModalComposerOutcome::Changed;
        };
        self.replace_range(
            TextRange::new(TextPosition::new(line, character), end),
            "",
            "delete previous prompt word",
        );
        ModalComposerOutcome::Changed
    }

    fn replace_range(&mut self, range: TextRange, replacement: &str, label: &str) {
        let old_text = self.buffer.text_in_range(range);
        if old_text == replacement {
            return;
        }
        let before = self.cursor_snapshot();
        self.buffer.undo_history.begin_transaction(label, before);
        let start_char = self.buffer.position_to_char_idx(range.start);
        self.buffer.undo_history.record_replace(
            range,
            start_char,
            old_text,
            replacement.to_string(),
        );
        self.buffer.replace_range_raw(range, replacement);
        self.cursor = self.buffer.range_for_text(range.start, replacement).end;
        self.preferred_column = None;
        self.validation_status = None;
        self.history_position = None;
        self.history_draft = None;
        if self.mode != ModalComposerMode::Insert {
            self.finish_transaction();
            self.clamp_cursor();
        }
    }

    fn finish_transaction(&mut self) {
        let after = self.cursor_snapshot();
        self.buffer.undo_history.commit_transaction(after);
        self.buffer.refresh_dirty_from_history();
    }

    fn undo(&mut self) -> ModalComposerOutcome {
        self.finish_transaction();
        let mut history = std::mem::take(&mut self.buffer.undo_history);
        let cursor = history.undo(&mut self.buffer).map(|(cursor, _)| cursor);
        self.buffer.undo_history = history;
        if let Some(cursor) = cursor {
            let line = self.line_text(cursor.y);
            self.cursor = TextPosition::new(cursor.y, grapheme_to_char(&line, cursor.x));
            self.buffer.refresh_dirty_from_history();
            self.clamp_cursor();
        }
        ModalComposerOutcome::Changed
    }

    fn redo(&mut self) -> ModalComposerOutcome {
        self.finish_transaction();
        let mut history = std::mem::take(&mut self.buffer.undo_history);
        let cursor = history.redo(&mut self.buffer).map(|(cursor, _)| cursor);
        self.buffer.undo_history = history;
        if let Some(cursor) = cursor {
            let line = self.line_text(cursor.y);
            self.cursor = TextPosition::new(cursor.y, grapheme_to_char(&line, cursor.x));
            self.buffer.refresh_dirty_from_history();
            self.clamp_cursor();
        }
        ModalComposerOutcome::Changed
    }

    fn cursor_snapshot(&self) -> CursorSnapshot {
        let (x, y) = self.cursor();
        CursorSnapshot::new(x, y, 0)
    }

    fn line_text(&self, line: usize) -> String {
        self.buffer
            .get(line)
            .map(|text| trim_line_ending(&text).to_string())
            .unwrap_or_default()
    }

    fn move_horizontal(&mut self, delta: isize) -> ModalComposerOutcome {
        let line = self.line_text(self.cursor.line);
        let current = char_to_grapheme(&line, self.cursor.character);
        let last = crate::unicode_utils::grapheme_len(&line);
        let target = current.saturating_add_signed(delta).min(last);
        self.cursor.character = grapheme_to_char(&line, target);
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn move_vertical(&mut self, delta: isize) -> ModalComposerOutcome {
        let goal = self.preferred_column.unwrap_or_else(|| self.cursor().0);
        let target_line = self
            .cursor
            .line
            .saturating_add_signed(delta)
            .min(self.buffer.len());
        let line = self.line_text(target_line);
        self.cursor = TextPosition::new(target_line, grapheme_to_char(&line, goal));
        self.preferred_column = Some(goal);
        self.clamp_cursor();
        ModalComposerOutcome::Changed
    }

    fn move_line_start(&mut self) -> ModalComposerOutcome {
        self.cursor.character = 0;
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn move_line_end(&mut self) -> ModalComposerOutcome {
        self.cursor.character = self.line_text(self.cursor.line).chars().count();
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn move_first_non_whitespace(&mut self) -> ModalComposerOutcome {
        let line = self.line_text(self.cursor.line);
        self.cursor.character = line
            .chars()
            .position(|character| !character.is_whitespace())
            .unwrap_or(0);
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn move_word_forward(&mut self) -> ModalComposerOutcome {
        if let Some((character, line)) = self
            .buffer
            .find_next_word((self.cursor.character, self.cursor.line))
        {
            self.cursor = TextPosition::new(line, character);
            self.clamp_cursor();
        }
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn move_word_backward(&mut self) -> ModalComposerOutcome {
        if let Some((character, line)) = self
            .buffer
            .find_prev_word((self.cursor.character, self.cursor.line))
        {
            self.cursor = TextPosition::new(line, character);
        }
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn move_word_end(&mut self) -> ModalComposerOutcome {
        if let Some((character, line)) = self
            .buffer
            .find_word_end((self.cursor.character, self.cursor.line))
        {
            self.cursor = TextPosition::new(line, character.saturating_sub(1));
            self.clamp_cursor();
        }
        self.preferred_column = None;
        ModalComposerOutcome::Changed
    }

    fn clamp_cursor(&mut self) {
        self.cursor.line = self.cursor.line.min(self.buffer.len());
        let line = self.line_text(self.cursor.line);
        let line_end = line.chars().count();
        let maximum = if self.mode == ModalComposerMode::Insert || line_end == 0 {
            line_end
        } else {
            let graphemes = crate::unicode_utils::grapheme_len(&line);
            grapheme_to_char(&line, graphemes.saturating_sub(1))
        };
        self.cursor.character = self.cursor.character.min(maximum);
    }

    fn previous_position(&self, position: TextPosition) -> Option<TextPosition> {
        if position.character > 0 {
            let line = self.line_text(position.line);
            let grapheme = char_to_grapheme(&line, position.character);
            return Some(TextPosition::new(
                position.line,
                grapheme_to_char(&line, grapheme.saturating_sub(1)),
            ));
        }
        if position.line == 0 {
            return None;
        }
        let line = self.line_text(position.line - 1);
        Some(TextPosition::new(position.line - 1, line.chars().count()))
    }

    fn next_position(&self, position: TextPosition) -> TextPosition {
        let line = self.line_text(position.line);
        let grapheme = char_to_grapheme(&line, position.character);
        let grapheme_count = crate::unicode_utils::grapheme_len(&line);
        if grapheme < grapheme_count {
            return TextPosition::new(position.line, grapheme_to_char(&line, grapheme + 1));
        }
        if position.line < self.buffer.len() {
            TextPosition::new(position.line + 1, 0)
        } else {
            TextPosition::new(position.line, line.chars().count())
        }
    }

    fn begin_operator(&mut self, operator: PendingOperator) -> ModalComposerOutcome {
        self.pending_g = false;
        self.pending_text_object = false;
        self.pending_operator = Some(operator);
        ModalComposerOutcome::Changed
    }

    fn handle_operator_key(&mut self, key: KeyEvent) -> ModalComposerOutcome {
        let Some(operator) = self.pending_operator else {
            return ModalComposerOutcome::Unhandled;
        };
        if key.code == KeyCode::Esc {
            self.pending_operator = None;
            self.pending_text_object = false;
            return ModalComposerOutcome::Changed;
        }
        if matches!(key.code, KeyCode::Char('i' | 'a')) {
            self.pending_text_object = true;
            return ModalComposerOutcome::Changed;
        }
        let origin = self.cursor;
        if self.pending_text_object && key.code == KeyCode::Char('w') {
            self.pending_operator = None;
            self.pending_text_object = false;
            return self.apply_word_object(operator);
        }

        let repeated = matches!(
            (operator, key.code),
            (PendingOperator::Delete, KeyCode::Char('d'))
                | (PendingOperator::Change, KeyCode::Char('c'))
                | (PendingOperator::Yank, KeyCode::Char('y'))
        );
        if repeated {
            self.pending_operator = None;
            self.pending_text_object = false;
            let line_start = TextPosition::new(origin.line, 0);
            let line_end = if origin.line < self.buffer.len() {
                TextPosition::new(origin.line + 1, 0)
            } else {
                TextPosition::new(origin.line, self.line_text(origin.line).chars().count())
            };
            return self.apply_operator(operator, TextRange::new(line_start, line_end));
        }

        let destination = match key.code {
            KeyCode::Char('w') => self
                .buffer
                .find_next_word((origin.character, origin.line))
                .map(|(character, line)| TextPosition::new(line, character)),
            KeyCode::Char('b') => self
                .buffer
                .find_prev_word((origin.character, origin.line))
                .map(|(character, line)| TextPosition::new(line, character)),
            KeyCode::Char('e') => self
                .buffer
                .find_word_end((origin.character, origin.line))
                .map(|(character, line)| TextPosition::new(line, character)),
            KeyCode::Char('$') => Some(TextPosition::new(
                origin.line,
                self.line_text(origin.line).chars().count(),
            )),
            KeyCode::Char('0') => Some(TextPosition::new(origin.line, 0)),
            KeyCode::Char('j') if origin.line < self.buffer.len() => {
                Some(TextPosition::new(origin.line + 1, 0))
            }
            KeyCode::Char('k') if origin.line > 0 => Some(TextPosition::new(origin.line - 1, 0)),
            _ => None,
        };
        self.pending_operator = None;
        self.pending_text_object = false;
        let Some(destination) = destination else {
            return ModalComposerOutcome::Changed;
        };
        let (start, end) = if Self::position_key(origin) <= Self::position_key(destination) {
            (origin, destination)
        } else {
            (destination, origin)
        };
        self.apply_operator(operator, TextRange::new(start, end))
    }

    fn apply_word_object(&mut self, operator: PendingOperator) -> ModalComposerOutcome {
        let line = self.line_text(self.cursor.line);
        let graphemes = line.graphemes(true).collect::<Vec<_>>();
        if graphemes.is_empty() {
            return ModalComposerOutcome::Changed;
        }
        let index = char_to_grapheme(&line, self.cursor.character).min(graphemes.len() - 1);
        let is_keyword = |grapheme: &str| {
            grapheme
                .chars()
                .any(|character| character.is_alphanumeric() || character == '_')
        };
        let class = is_keyword(graphemes[index]);
        let mut start = index;
        while start > 0 && is_keyword(graphemes[start - 1]) == class {
            start -= 1;
        }
        let mut end = index + 1;
        while end < graphemes.len() && is_keyword(graphemes[end]) == class {
            end += 1;
        }
        self.apply_operator(
            operator,
            TextRange::new(
                TextPosition::new(self.cursor.line, grapheme_to_char(&line, start)),
                TextPosition::new(self.cursor.line, grapheme_to_char(&line, end)),
            ),
        )
    }

    fn apply_visual_operator(&mut self, operator: PendingOperator) -> ModalComposerOutcome {
        let Some(range) = self.selection_range() else {
            return ModalComposerOutcome::Changed;
        };
        self.visual_anchor = None;
        self.mode = ModalComposerMode::Normal;
        self.apply_operator(operator, range)
    }

    fn apply_operator(
        &mut self,
        operator: PendingOperator,
        range: TextRange,
    ) -> ModalComposerOutcome {
        if range.start == range.end {
            if operator == PendingOperator::Change {
                self.mode = ModalComposerMode::Insert;
            }
            return ModalComposerOutcome::Changed;
        }
        self.register = self.buffer.text_in_range(range);
        if operator == PendingOperator::Yank {
            self.cursor = range.start;
            self.clamp_cursor();
            return ModalComposerOutcome::Changed;
        }
        if operator == PendingOperator::Change {
            self.mode = ModalComposerMode::Insert;
        }
        self.replace_range(range, "", "operate on prompt text");
        if operator != PendingOperator::Change {
            self.clamp_cursor();
        }
        ModalComposerOutcome::Changed
    }

    fn paste_register(&mut self) -> ModalComposerOutcome {
        if self.register.is_empty() {
            return ModalComposerOutcome::Changed;
        }
        let text = self.register.clone();
        self.cursor = self.next_position(self.cursor);
        self.insert_text(&text)
    }

    fn position_key(position: TextPosition) -> (usize, usize) {
        (position.line, position.character)
    }
}

/// Normalizes Windows and classic Mac newline sequences for prompt buffers.
#[must_use]
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{ModalComposer, ModalComposerMode, ModalComposerOutcome, MAX_PROMPT_BYTES};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn control(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    fn modified_enter(modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, modifiers)
    }

    fn modified_enter_codes() -> [KeyCode; 3] {
        [KeyCode::Enter, KeyCode::Char('\n'), KeyCode::Char('\r')]
    }

    fn normal(composer: &mut ModalComposer) {
        assert_eq!(
            composer.handle_key(key(KeyCode::Esc)),
            ModalComposerOutcome::Changed
        );
        assert_eq!(composer.mode(), ModalComposerMode::Normal);
    }

    #[test]
    fn empty_scratch_buffer_has_no_manufactured_newline() {
        let composer = ModalComposer::new("", vec![]);
        assert_eq!(composer.contents(), "");
        assert_eq!(composer.cursor(), (0, 0));
        assert_eq!(composer.mode(), ModalComposerMode::Insert);
    }

    #[test]
    fn insert_enter_shift_enter_and_control_j_create_newlines() {
        let mut composer = ModalComposer::new("hello", vec![]);
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter)),
            ModalComposerOutcome::Changed
        );
        assert_eq!(composer.contents(), "hello\n");

        assert_eq!(
            composer.handle_key(modified_enter(KeyModifiers::SHIFT)),
            ModalComposerOutcome::Changed
        );
        assert_eq!(composer.contents(), "hello\n\n");

        assert_eq!(
            composer.handle_key(control('j')),
            ModalComposerOutcome::Changed
        );
        assert_eq!(composer.contents(), "hello\n\n\n");

        normal(&mut composer);
        assert_eq!(composer.contents(), "hello\n\n\n");
    }

    #[test]
    fn control_enter_submits_immediately_from_insert_without_adding_a_newline() {
        for code in modified_enter_codes() {
            let mut composer = ModalComposer::new("first\n漢👨‍👩‍👧", vec![]);

            assert_eq!(composer.mode(), ModalComposerMode::Insert);
            assert_eq!(
                composer.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL)),
                ModalComposerOutcome::Submit,
                "Ctrl+Enter should immediately submit {code:?} in Insert mode",
            );
            assert_eq!(composer.mode(), ModalComposerMode::Insert);
            assert_eq!(composer.contents(), "first\n漢👨‍👩‍👧");
            assert_eq!(composer.validation_status(), None);
        }
    }

    #[test]
    fn control_and_alt_enter_submit_in_every_vim_mode() {
        let modifiers = [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        ];

        for code in modified_enter_codes() {
            for modifiers in modifiers {
                for mode in [
                    ModalComposerMode::Insert,
                    ModalComposerMode::Normal,
                    ModalComposerMode::Visual,
                ] {
                    let mut composer = ModalComposer::new("hello\n漢👨‍👩‍👧", vec![]);

                    if mode != ModalComposerMode::Insert {
                        normal(&mut composer);
                        if mode == ModalComposerMode::Visual {
                            assert_eq!(
                                composer.handle_key(key(KeyCode::Char('v'))),
                                ModalComposerOutcome::Changed
                            );
                        }
                    }

                    assert_eq!(composer.mode(), mode);
                    assert_eq!(
                        composer.handle_key(KeyEvent::new(code, modifiers)),
                        ModalComposerOutcome::Submit,
                        "{code:?} should submit in {mode:?} with {modifiers:?}",
                    );
                    assert_eq!(composer.contents(), "hello\n漢👨‍👩‍👧");
                }
            }
        }
    }

    #[test]
    fn modified_enter_takes_precedence_over_a_pending_normal_operator() {
        for code in modified_enter_codes() {
            let mut composer = ModalComposer::new("keep this draft", vec![]);
            normal(&mut composer);

            assert_eq!(
                composer.handle_key(key(KeyCode::Char('d'))),
                ModalComposerOutcome::Changed
            );
            assert_eq!(
                composer.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL)),
                ModalComposerOutcome::Submit,
                "{code:?} should take precedence over a pending operator",
            );
            assert_eq!(composer.contents(), "keep this draft");
        }
    }

    #[test]
    fn unmodified_normal_enter_submits_without_losing_draft() {
        let mut composer = ModalComposer::new("hello", vec![]);
        normal(&mut composer);

        assert_eq!(
            composer.handle_key(key(KeyCode::Enter)),
            ModalComposerOutcome::Submit
        );
        assert_eq!(composer.contents(), "hello");
    }

    #[test]
    fn control_s_is_not_an_agent_submission_shortcut() {
        for mode in [
            ModalComposerMode::Insert,
            ModalComposerMode::Normal,
            ModalComposerMode::Visual,
        ] {
            let mut composer = ModalComposer::new("keep this draft", vec![]);

            if mode != ModalComposerMode::Insert {
                normal(&mut composer);
                if mode == ModalComposerMode::Visual {
                    composer.handle_key(key(KeyCode::Char('v')));
                }
            }

            assert_eq!(composer.mode(), mode);
            assert_eq!(
                composer.handle_key(control('s')),
                ModalComposerOutcome::Unhandled,
                "Ctrl+S must not submit in {mode:?}",
            );
            assert_eq!(composer.contents(), "keep this draft");
        }
    }

    #[test]
    fn modified_enter_rejects_empty_submissions_without_losing_draft() {
        for code in modified_enter_codes() {
            for modifiers in [
                KeyModifiers::CONTROL,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ] {
                let mut composer = ModalComposer::new(" \n", vec![]);

                assert_eq!(
                    composer.handle_key(KeyEvent::new(code, modifiers)),
                    ModalComposerOutcome::Rejected,
                    "empty drafts must not submit {code:?} with {modifiers:?}",
                );
                assert_eq!(composer.validation_status(), Some("Prompt is empty"));
                assert_eq!(composer.contents(), " \n");
            }
        }
    }

    #[test]
    fn normal_mode_supports_words_and_line_navigation() {
        let mut composer = ModalComposer::new("one two\nthree four", vec![]);
        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('g')));
        composer.handle_key(key(KeyCode::Char('g')));
        assert_eq!(composer.cursor(), (0, 0));
        composer.handle_key(key(KeyCode::Char('w')));
        assert_eq!(composer.cursor(), (4, 0));
        composer.handle_key(key(KeyCode::Char('j')));
        assert_eq!(composer.cursor(), (4, 1));
        composer.handle_key(key(KeyCode::Char('0')));
        assert_eq!(composer.cursor(), (0, 1));
        composer.handle_key(key(KeyCode::Char('G')));
        assert_eq!(composer.cursor().1, 1);
    }

    #[test]
    fn word_delete_is_a_real_buffer_undo_transaction() {
        let mut composer = ModalComposer::new("one two three", vec![]);
        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('0')));
        composer.handle_key(key(KeyCode::Char('d')));
        composer.handle_key(key(KeyCode::Char('w')));
        assert_eq!(composer.contents(), "two three");
        composer.handle_key(key(KeyCode::Char('u')));
        assert_eq!(composer.contents(), "one two three");
        composer.handle_key(control('r'));
        assert_eq!(composer.contents(), "two three");
    }

    #[test]
    fn change_inner_word_enters_insert_mode_and_preserves_undo() {
        let mut composer = ModalComposer::new("one target three", vec![]);
        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('0')));
        composer.handle_key(key(KeyCode::Char('w')));
        composer.handle_key(key(KeyCode::Char('c')));
        composer.handle_key(key(KeyCode::Char('i')));
        composer.handle_key(key(KeyCode::Char('w')));
        assert_eq!(composer.mode(), ModalComposerMode::Insert);
        assert_eq!(composer.contents(), "one  three");
        composer.handle_key(key(KeyCode::Char('x')));
        normal(&mut composer);
        assert_eq!(composer.contents(), "one x three");
        composer.handle_key(key(KeyCode::Char('u')));
        assert_eq!(composer.contents(), "one target three");
    }

    #[test]
    fn visual_delete_removes_the_inclusive_selected_range() {
        let mut composer = ModalComposer::new("abcdef", vec![]);
        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('0')));
        composer.handle_key(key(KeyCode::Char('v')));
        composer.handle_key(key(KeyCode::Char('l')));
        composer.handle_key(key(KeyCode::Char('l')));
        assert!(composer.selection_range().is_some());
        composer.handle_key(key(KeyCode::Char('d')));
        assert_eq!(composer.mode(), ModalComposerMode::Normal);
        assert_eq!(composer.contents(), "def");
        composer.handle_key(key(KeyCode::Char('u')));
        assert_eq!(composer.contents(), "abcdef");
    }

    #[test]
    fn unicode_backspace_and_cursor_remove_whole_graphemes() {
        let mut composer = ModalComposer::new("e\u{301}👨‍👩‍👧漢", vec![]);
        composer.handle_key(key(KeyCode::Backspace));
        assert_eq!(composer.contents(), "e\u{301}👨‍👩‍👧");
        composer.handle_key(key(KeyCode::Backspace));
        assert_eq!(composer.contents(), "e\u{301}");
        composer.handle_key(key(KeyCode::Backspace));
        assert_eq!(composer.contents(), "");
    }

    #[test]
    fn multiline_paste_normalizes_newlines_and_undoes_as_one_insert() {
        let mut composer = ModalComposer::new("prefix", vec![]);
        assert_eq!(
            composer.handle_paste("\r\nsecond\rthird"),
            ModalComposerOutcome::Changed
        );
        assert_eq!(composer.contents(), "prefix\nsecond\nthird");
        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('u')));
        assert_eq!(composer.contents(), "prefix");
    }

    #[test]
    fn history_navigation_restores_original_multiline_draft() {
        let mut composer = ModalComposer::new(
            "draft\nline",
            vec!["newer\r\nprompt".to_string(), "older".to_string()],
        );
        composer.handle_key(control('p'));
        assert_eq!(composer.contents(), "newer\nprompt");
        composer.handle_key(control('p'));
        assert_eq!(composer.contents(), "older");
        composer.handle_key(control('n'));
        assert_eq!(composer.contents(), "newer\nprompt");
        composer.handle_key(control('n'));
        assert_eq!(composer.contents(), "draft\nline");
    }

    #[test]
    fn oversized_paste_and_initial_draft_preserve_safe_state() {
        let mut composer = ModalComposer::new("draft", vec![]);
        let oversized = "x".repeat(MAX_PROMPT_BYTES);
        assert_eq!(
            composer.handle_paste(&oversized),
            ModalComposerOutcome::Rejected
        );
        assert_eq!(composer.contents(), "draft");
        assert_eq!(composer.validation_status(), Some("Prompt exceeds 128 KiB"));

        let rejected = ModalComposer::new(&"x".repeat(MAX_PROMPT_BYTES + 1), vec![]);
        assert_eq!(rejected.contents(), "");
        assert_eq!(rejected.validation_status(), Some("Prompt exceeds 128 KiB"));
    }

    #[test]
    fn opened_lines_remain_in_insert_mode() {
        let mut composer = ModalComposer::new("one\ntwo", vec![]);
        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('g')));
        composer.handle_key(key(KeyCode::Char('g')));
        composer.handle_key(key(KeyCode::Char('o')));
        assert_eq!(composer.mode(), ModalComposerMode::Insert);
        assert_eq!(composer.contents(), "one\n\ntwo");
        composer.handle_key(key(KeyCode::Char('x')));
        assert_eq!(composer.contents(), "one\nx\ntwo");
    }

    #[test]
    fn yank_and_put_use_a_shared_real_buffer_register() {
        let mut composer = ModalComposer::new("one two", vec![]);
        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('0')));
        composer.handle_key(key(KeyCode::Char('y')));
        composer.handle_key(key(KeyCode::Char('w')));
        composer.handle_key(key(KeyCode::Char('$')));
        composer.handle_key(key(KeyCode::Char('p')));
        assert_eq!(composer.contents(), "one twoone ");
    }

    #[test]
    fn global_cursor_offset_counts_complete_unicode_graphemes() {
        let composer = ModalComposer::new("e\u{301}\n👨‍👩‍👧漢", vec![]);
        assert_eq!(composer.cursor(), (2, 1));
        assert_eq!(composer.cursor_grapheme_index(), 4);
    }

    #[test]
    fn change_inner_word_never_splits_decomposed_unicode_graphemes() {
        let original = "prefix cafe\u{301} suffix";
        let mut composer = ModalComposer::new(original, vec![]);

        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('0')));
        composer.handle_key(key(KeyCode::Char('w')));
        composer.handle_key(key(KeyCode::Char('c')));
        composer.handle_key(key(KeyCode::Char('i')));
        composer.handle_key(key(KeyCode::Char('w')));

        assert_eq!(composer.mode(), ModalComposerMode::Insert);
        assert_eq!(composer.contents(), "prefix  suffix");
        assert_eq!(composer.handle_paste("漢👨‍👩‍👧"), ModalComposerOutcome::Changed);
        assert_eq!(composer.contents(), "prefix 漢👨‍👩‍👧 suffix");

        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('u')));
        assert_eq!(composer.contents(), original);
    }

    #[test]
    fn visual_selection_deletes_whole_combining_and_family_graphemes() {
        let original = "a👨‍👩‍👧e\u{301}漢z";
        let mut composer = ModalComposer::new(original, vec![]);

        normal(&mut composer);
        composer.handle_key(key(KeyCode::Char('0')));
        composer.handle_key(key(KeyCode::Char('l')));
        composer.handle_key(key(KeyCode::Char('v')));
        composer.handle_key(key(KeyCode::Char('l')));
        composer.handle_key(key(KeyCode::Char('d')));

        assert_eq!(composer.contents(), "a漢z");
        composer.handle_key(key(KeyCode::Char('u')));
        assert_eq!(composer.contents(), original);
    }

    #[test]
    fn markdown_paste_preserves_fences_unicode_and_exact_normalized_source() {
        let mut composer = ModalComposer::new("", vec![]);
        let markdown = "## Café\r\n\r\n```rust\r\nlet family = \"👨‍👩‍👧漢\";\r\n```\r\n";
        let normalized = "## Café\n\n```rust\nlet family = \"👨‍👩‍👧漢\";\n```\n";

        assert_eq!(
            composer.handle_paste(markdown),
            ModalComposerOutcome::Changed
        );
        assert_eq!(composer.contents(), normalized);
        assert_eq!(
            composer.handle_key(modified_enter(KeyModifiers::CONTROL)),
            ModalComposerOutcome::Submit
        );
        assert_eq!(composer.contents(), normalized);
    }

    #[test]
    fn taking_submission_resets_insert_mode_and_preserves_recall_history() {
        let mut composer = ModalComposer::new("first\nprompt", vec![]);
        normal(&mut composer);

        assert_eq!(composer.take_submission().as_deref(), Some("first\nprompt"));
        assert_eq!(composer.mode(), ModalComposerMode::Insert);
        assert_eq!(composer.contents(), "");

        composer.handle_paste("unfinished draft");
        composer.handle_key(control('p'));
        assert_eq!(composer.contents(), "first\nprompt");
        composer.handle_key(control('n'));
        assert_eq!(composer.contents(), "unfinished draft");
    }

    #[test]
    fn setting_multiline_mouse_cursor_never_splits_unicode_graphemes() {
        let mut composer = ModalComposer::new("a👨‍👩‍👧\ne\u{301}漢", vec![]);

        composer.set_cursor_grapheme_index(1);
        assert_eq!(composer.cursor(), (1, 0));
        composer.handle_paste("X");
        assert_eq!(composer.contents(), "aX👨‍👩‍👧\ne\u{301}漢");

        composer.set_cursor_grapheme_index(usize::MAX);
        assert_eq!(composer.cursor(), (2, 1));
        assert_eq!(composer.cursor_grapheme_index(), 6);
    }
}
