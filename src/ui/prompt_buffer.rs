//! Prompt history and submission policy around the reusable modal text area.
//!
//! Plugin composers and agent dialogs own independent [`TextArea`] values. Their
//! documents never enter the editor's file-buffer list; this wrapper adds bounded
//! prompt history plus surface-specific submit and cancellation shortcuts.

use crossterm::event::{Event, KeyCode, KeyModifiers};

use crate::{
    buffer::Buffer,
    editing::{TextArea, TextAreaOutcome},
    editor::Mode,
    unicode_utils::grapheme_len,
};

/// Largest prompt accepted by the direct Codex app-server integration.
pub(crate) const PROMPT_MAX_BYTES: usize = 128 * 1024;

/// Outcome of applying one terminal input event to an ephemeral prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptInput {
    /// The draft, cursor, history selection, editor mode, or pending motion changed.
    Changed,
    /// The current complete prompt should be validated and submitted.
    Submit,
    /// The containing surface should be cancelled or blurred.
    Cancel,
    /// The event is not a prompt-editing action.
    Unhandled,
}

/// Fileless modal text area with thread-local prompt history and submission policy.
#[derive(Debug)]
pub(crate) struct PromptBuffer {
    area: TextArea,
    history: Vec<String>,
    history_position: Option<usize>,
    history_draft: Option<String>,
}

impl PromptBuffer {
    /// Creates an unnamed prompt in insert mode with no previous history.
    pub(crate) fn new(text: impl AsRef<str>) -> Self {
        Self::with_history(text, Vec::new())
    }

    /// Creates an unnamed prompt while preserving only bounded history entries.
    pub(crate) fn with_history(text: impl AsRef<str>, history: Vec<String>) -> Self {
        Self {
            area: TextArea::with_max_bytes(text, PROMPT_MAX_BYTES),
            history: history
                .into_iter()
                .filter(|entry| entry.len() <= PROMPT_MAX_BYTES)
                .map(|entry| normalize_prompt_newlines(&entry))
                .collect(),
            history_position: None,
            history_draft: None,
        }
    }

    /// Returns the underlying unnamed editor buffer.
    #[must_use]
    pub(crate) fn buffer(&self) -> &Buffer {
        self.area.buffer()
    }

    /// Returns the exact UTF-8 draft without adding a synthetic newline.
    #[must_use]
    pub(crate) fn text(&self) -> String {
        self.area.text()
    }

    /// Returns the cursor as an absolute extended-grapheme index.
    #[must_use]
    pub(crate) const fn cursor(&self) -> usize {
        self.area.cursor()
    }

    /// Returns the prompt's independent editor mode.
    #[must_use]
    pub(crate) const fn mode(&self) -> Mode {
        self.area.mode()
    }

    /// Changes the local prompt mode without mutating the global editor.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        self.area.set_mode(mode);
    }

    /// Returns thread-local prompt history, newest submission first.
    #[must_use]
    pub(crate) fn history(&self) -> &[String] {
        &self.history
    }

    /// Moves the cursor to a bounded absolute grapheme position.
    pub(crate) fn set_cursor(&mut self, cursor: usize) {
        self.area.set_cursor(cursor);
    }

    /// Replaces the complete draft through the shared transaction boundary.
    pub(crate) fn set_text(&mut self, text: &str) -> bool {
        self.area.set_text(text)
    }

    /// Inserts normalized text as one undoable prompt transaction.
    pub(crate) fn insert(&mut self, text: &str) -> bool {
        let changed = self.area.insert(text);
        self.detach_history_after_edit(changed);
        changed
    }

    /// Removes the previous complete extended grapheme.
    pub(crate) fn backspace(&mut self) -> bool {
        let changed = self.area.backspace();
        self.detach_history_after_edit(changed);
        changed
    }

    /// Removes the complete extended grapheme under the cursor.
    pub(crate) fn delete(&mut self) -> bool {
        let changed = self.area.delete();
        self.detach_history_after_edit(changed);
        changed
    }

    /// Removes whitespace and the word immediately preceding the cursor.
    pub(crate) fn delete_previous_word(&mut self) -> bool {
        let changed = self.area.delete_previous_word();
        self.detach_history_after_edit(changed);
        changed
    }

    /// Selects the previous submitted prompt while preserving the current draft.
    pub(crate) fn history_previous(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let index = match self.history_position {
            Some(index) => (index + 1).min(self.history.len() - 1),
            None => {
                self.history_draft = Some(self.text());
                0
            }
        };
        let entry = self.history[index].clone();
        self.history_position = Some(index);
        self.set_text(&entry)
    }

    /// Moves toward newer history or restores the exact unsent draft.
    pub(crate) fn history_next(&mut self) -> bool {
        let Some(index) = self.history_position else {
            return false;
        };
        let next = if index == 0 {
            self.history_position = None;
            self.history_draft.take().unwrap_or_default()
        } else {
            self.history_position = Some(index - 1);
            self.history[index - 1].clone()
        };
        self.set_text(&next)
    }

    /// Undoes one real, branch-preserving prompt edit.
    pub(crate) fn undo(&mut self) -> bool {
        self.area.undo()
    }

    /// Redoes one real prompt transaction on the selected undo branch.
    pub(crate) fn redo(&mut self) -> bool {
        self.area.redo()
    }

    /// Clears the draft without associating it with a file or losing history.
    pub(crate) fn clear(&mut self) {
        self.area.clear();
        self.history_position = None;
        self.history_draft = None;
    }

    /// Takes a nonblank draft and adds it to bounded, deduplicated history.
    pub(crate) fn take_submission(&mut self) -> Option<String> {
        let text = self.text();
        if text.trim().is_empty() || text.len() > PROMPT_MAX_BYTES {
            return None;
        }
        self.history.retain(|entry| entry != &text);
        self.history.insert(0, text.clone());
        self.history.truncate(50);
        self.clear();
        self.set_mode(Mode::Insert);
        Some(text)
    }

    /// Applies local prompt policy before delegating modal editing to the shared engine.
    pub(crate) fn handle_event(&mut self, event: &Event, wrap_width: usize) -> PromptInput {
        let Event::Key(key) = event else {
            return self.apply_area_event(event, wrap_width);
        };
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Enter | KeyCode::Char('\n')
                if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                PromptInput::Submit
            }
            KeyCode::Char('c' | 'C') if modifiers.contains(KeyModifiers::CONTROL) => {
                PromptInput::Cancel
            }
            KeyCode::Esc
                if self.mode() == Mode::Normal && !self.area.state().has_pending_input() =>
            {
                PromptInput::Cancel
            }
            KeyCode::Enter if self.mode() == Mode::Normal => PromptInput::Submit,
            KeyCode::Char('j' | 'J') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert("\n");
                PromptInput::Changed
            }
            KeyCode::Char('p' | 'P') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_previous();
                PromptInput::Changed
            }
            KeyCode::Char('n' | 'N') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_next();
                PromptInput::Changed
            }
            KeyCode::Char('w' | 'W') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_previous_word();
                PromptInput::Changed
            }
            KeyCode::Char('r' | 'R') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.redo();
                PromptInput::Changed
            }
            KeyCode::Char('z' | 'Z') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.undo();
                PromptInput::Changed
            }
            KeyCode::Char('a' | 'A') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_cursor(0);
                PromptInput::Changed
            }
            KeyCode::Char('e' | 'E') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_cursor(grapheme_len(&self.text()));
                PromptInput::Changed
            }
            _ => self.apply_area_event(event, wrap_width),
        }
    }

    fn apply_area_event(&mut self, event: &Event, wrap_width: usize) -> PromptInput {
        let previous_revision = self.buffer().revision();
        let result = self.area.handle_event(event, wrap_width);
        self.detach_history_after_edit(self.buffer().revision() != previous_revision);
        match result {
            TextAreaOutcome::Changed => PromptInput::Changed,
            TextAreaOutcome::Unhandled => PromptInput::Unhandled,
        }
    }

    fn detach_history_after_edit(&mut self, changed: bool) {
        if changed {
            self.history_position = None;
            self.history_draft = None;
        }
    }
}

/// Normalizes terminal and clipboard line endings before prompt editing.
#[must_use]
pub(crate) fn normalize_prompt_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Keeps multiline paste from submitting or executing a single-line prompt.
#[must_use]
pub(crate) fn first_prompt_line(text: &str) -> String {
    normalize_prompt_newlines(text)
        .split('\n')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::{
        first_prompt_line, normalize_prompt_newlines, Mode, PromptBuffer, PromptInput,
        PROMPT_MAX_BYTES,
    };

    #[test]
    fn single_line_paste_shares_crlf_normalization_without_accepting_a_newline() {
        assert_eq!(
            normalize_prompt_newlines("first\r\nsecond\rthird"),
            "first\nsecond\nthird"
        );
        assert_eq!(first_prompt_line("first\r\nsecond"), "first");
        assert_eq!(first_prompt_line("\r\nsecond"), "");
        assert_eq!(first_prompt_line("👨‍👩‍👧\nignored"), "👨‍👩‍👧");
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn empty_prompt_is_a_real_unnamed_buffer_without_a_synthetic_newline() {
        let prompt = PromptBuffer::new("");

        assert!(prompt.buffer().file.is_none());
        assert_eq!(prompt.buffer().contents(), "");
        assert_eq!(prompt.text(), "");
        assert_eq!(prompt.cursor(), 0);
        assert_eq!(prompt.mode(), Mode::Insert);
        assert_eq!(prompt.buffer().undo_history.node_count(), 0);
    }

    #[test]
    fn prompt_normalizes_initial_text_history_and_pasted_newlines() {
        let mut prompt = PromptBuffer::with_history(
            "first\r\nsecond\rthird",
            vec!["history\r\nentry".to_string()],
        );

        assert_eq!(prompt.text(), "first\nsecond\nthird");
        assert_eq!(prompt.history(), ["history\nentry"]);
        assert_eq!(
            prompt.handle_event(&Event::Paste("\r\nfourth".to_string()), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first\nsecond\nthird\nfourth");
    }

    #[test]
    fn edits_commit_to_the_real_buffer_undo_and_redo_history() {
        let mut prompt = PromptBuffer::new("hello");

        assert!(prompt.insert(" world"));
        assert_eq!(prompt.text(), "hello world");
        assert_eq!(prompt.buffer().undo_history.node_count(), 1);
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "hello");
        assert_eq!(prompt.cursor(), 5);
        assert!(prompt.redo());
        assert_eq!(prompt.text(), "hello world");
        assert_eq!(prompt.cursor(), 11);
    }

    #[test]
    fn insertion_keeps_cursor_after_merged_graphemes() {
        let cases = [
            (
                "combining mark",
                "aX",
                1,
                "\u{301}",
                "a\u{301}X",
                1,
                "a\u{301}ZX",
            ),
            (
                "stacked combining marks",
                "a\u{301}X",
                1,
                "\u{327}",
                "a\u{301}\u{327}X",
                1,
                "a\u{301}\u{327}ZX",
            ),
            ("emoji modifier", "👍X", 1, "🏽", "👍🏽X", 1, "👍🏽ZX"),
            (
                "joined emoji",
                "👩X",
                1,
                "\u{200d}💻",
                "👩\u{200d}💻X",
                1,
                "👩\u{200d}💻ZX",
            ),
            (
                "multiline combining mark",
                "one\naX\ntail",
                5,
                "\u{301}",
                "one\na\u{301}X\ntail",
                5,
                "one\na\u{301}ZX\ntail",
            ),
            (
                "multiple inserted graphemes",
                "aX",
                1,
                "\u{301}b",
                "a\u{301}bX",
                2,
                "a\u{301}bZX",
            ),
        ];

        for (name, initial, cursor, inserted, merged, merged_cursor, subsequent) in cases {
            let mut prompt = PromptBuffer::new(initial);
            prompt.set_cursor(cursor);

            assert!(prompt.insert(inserted), "{name}: insert grapheme extension");
            assert_eq!(prompt.text(), merged, "{name}: preserve merged grapheme");
            assert_eq!(
                prompt.cursor(),
                merged_cursor,
                "{name}: position cursor after resulting prefix"
            );

            assert!(prompt.insert("Z"), "{name}: insert following character");
            assert_eq!(prompt.text(), subsequent, "{name}: preserve following text");
            assert_eq!(prompt.cursor(), merged_cursor + 1, "{name}: advance cursor");

            assert!(prompt.undo(), "{name}: undo following character");
            assert_eq!(prompt.text(), merged, "{name}: restore merged grapheme");
            assert_eq!(
                prompt.cursor(),
                merged_cursor,
                "{name}: restore merged cursor"
            );
            assert!(prompt.undo(), "{name}: undo grapheme extension");
            assert_eq!(prompt.text(), initial, "{name}: restore original text");
            assert_eq!(prompt.cursor(), cursor, "{name}: restore original cursor");

            assert!(prompt.redo(), "{name}: redo grapheme extension");
            assert_eq!(prompt.text(), merged, "{name}: redo merged grapheme");
            assert_eq!(prompt.cursor(), merged_cursor, "{name}: redo merged cursor");
            assert!(prompt.redo(), "{name}: redo following character");
            assert_eq!(prompt.text(), subsequent, "{name}: redo following text");
            assert_eq!(prompt.cursor(), merged_cursor + 1, "{name}: redo cursor");
        }
    }

    #[test]
    fn deletion_and_undo_preserve_complete_unicode_graphemes() {
        let mut prompt = PromptBuffer::new("e\u{301}👨‍👩‍👧漢");

        assert!(prompt.backspace());
        assert_eq!(prompt.text(), "e\u{301}👨‍👩‍👧");
        assert!(prompt.backspace());
        assert_eq!(prompt.text(), "e\u{301}");
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "e\u{301}👨‍👩‍👧");
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "e\u{301}👨‍👩‍👧漢");
        prompt.set_cursor(0);
        assert!(prompt.delete());
        assert_eq!(prompt.text(), "👨‍👩‍👧漢");
    }

    #[test]
    fn modified_enter_and_modified_linefeed_submit_in_insert_mode() {
        for event in [
            key(KeyCode::Enter, KeyModifiers::CONTROL),
            key(KeyCode::Char('\n'), KeyModifiers::CONTROL),
            key(KeyCode::Enter, KeyModifiers::ALT),
            key(KeyCode::Char('\n'), KeyModifiers::ALT),
        ] {
            let mut prompt = PromptBuffer::new("send this");

            assert_eq!(prompt.handle_event(&event, 40), PromptInput::Submit);
            assert_eq!(prompt.text(), "send this");
            assert_eq!(prompt.mode(), Mode::Insert);
        }
    }

    #[test]
    fn ordinary_enter_and_control_j_insert_real_newlines() {
        let mut prompt = PromptBuffer::new("first");

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert!(prompt.insert("second"));
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('j'), KeyModifiers::CONTROL), 40),
            PromptInput::Changed
        );

        assert_eq!(prompt.text(), "first\nsecond\n");
        assert_eq!(prompt.buffer().undo_history.node_count(), 3);
    }

    #[test]
    fn escape_enters_normal_mode_and_enter_submits_without_mutating_the_draft() {
        let mut prompt = PromptBuffer::new("draft");

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.mode(), Mode::Normal);
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Submit
        );
        assert_eq!(prompt.text(), "draft");
    }

    #[test]
    fn normal_mode_line_boundary_escape_selects_a_current_line_grapheme() {
        let cases = [
            ("single line", "abc", 3, 2),
            ("first line end", "abc\ndef", 3, 2),
            ("second line start", "abc\ndef", 4, 4),
            ("second line interior", "abc\ndef", 6, 5),
            ("empty middle line", "abc\n\ndef", 4, 4),
            ("empty draft", "", 0, 0),
            ("Unicode graphemes", "e\u{301}👨‍👩‍👧", 2, 1),
        ];

        for (name, text, insertion_cursor, normal_cursor) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(insertion_cursor);

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: enter normal mode"
            );
            assert_eq!(prompt.mode(), Mode::Normal, "{name}: set normal mode");
            assert_eq!(
                prompt.cursor(),
                normal_cursor,
                "{name}: remain on the current line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve draft");
        }
    }

    #[test]
    fn normal_mode_line_boundary_horizontal_motions_stay_on_current_line() {
        let cases = [
            (
                "h at second line start",
                "one\ntwo",
                4,
                'h',
                4,
                Some("one\nwo"),
            ),
            (
                "l at first line end",
                "one\ntwo",
                2,
                'l',
                2,
                Some("on\ntwo"),
            ),
            (
                "h within second line",
                "one\ntwo",
                5,
                'h',
                4,
                Some("one\nwo"),
            ),
            (
                "l within second line",
                "one\ntwo",
                4,
                'l',
                5,
                Some("one\nto"),
            ),
            ("h on empty line", "one\n\ntwo", 4, 'h', 4, None),
            ("l on empty line", "one\n\ntwo", 4, 'l', 4, None),
            (
                "l after Unicode grapheme",
                "e\u{301}👨‍👩‍👧\nlast",
                1,
                'l',
                1,
                Some("e\u{301}\nlast"),
            ),
        ];

        for (name, text, cursor, motion, expected_cursor, expected_delete) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char(motion), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: apply horizontal motion"
            );
            assert_eq!(
                prompt.cursor(),
                expected_cursor,
                "{name}: stay on current line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve line breaks");

            if let Some(expected) = expected_delete {
                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: delete selected grapheme"
                );
                assert_eq!(prompt.text(), expected, "{name}: preserve adjacent line");
            }
        }
    }

    #[test]
    fn normal_mode_line_boundary_arrow_motions_stay_on_current_line() {
        let cases = [
            (
                "Left at second line start",
                "one\ntwo",
                4,
                KeyCode::Left,
                4,
                Some("one\nwo"),
            ),
            (
                "Right at first line end",
                "one\ntwo",
                2,
                KeyCode::Right,
                2,
                Some("on\ntwo"),
            ),
            (
                "Left within second line",
                "one\ntwo",
                5,
                KeyCode::Left,
                4,
                Some("one\nwo"),
            ),
            (
                "Right within second line",
                "one\ntwo",
                4,
                KeyCode::Right,
                5,
                Some("one\nto"),
            ),
            (
                "Left on empty line",
                "one\n\ntwo",
                4,
                KeyCode::Left,
                4,
                None,
            ),
            (
                "Right on empty line",
                "one\n\ntwo",
                4,
                KeyCode::Right,
                4,
                None,
            ),
            (
                "Right after Unicode grapheme",
                "e\u{301}👨‍👩‍👧\nlast",
                1,
                KeyCode::Right,
                1,
                Some("e\u{301}\nlast"),
            ),
        ];

        for (name, text, cursor, arrow, expected_cursor, expected_delete) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);

            assert_eq!(
                prompt.handle_event(&key(arrow, KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: move with arrow key"
            );
            assert_eq!(prompt.mode(), Mode::Normal, "{name}: remain in normal mode");
            assert_eq!(
                prompt.cursor(),
                expected_cursor,
                "{name}: stay on current line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve line breaks");

            if let Some(expected) = expected_delete {
                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: delete selected grapheme"
                );
                assert_eq!(prompt.text(), expected, "{name}: preserve adjacent line");
            }
        }
    }

    #[test]
    fn insert_mode_arrow_motions_cross_prompt_line_boundaries() {
        let cases = [
            (
                "Left from second line",
                "one\ntwo",
                4,
                KeyCode::Left,
                3,
                "oneZ\ntwo",
            ),
            (
                "Right across newline",
                "one\ntwo",
                3,
                KeyCode::Right,
                4,
                "one\nZtwo",
            ),
            (
                "Right across empty line",
                "one\n\ntwo",
                4,
                KeyCode::Right,
                5,
                "one\n\nZtwo",
            ),
            (
                "Left after Unicode graphemes",
                "e\u{301}👨‍👩‍👧\nlast",
                3,
                KeyCode::Left,
                2,
                "e\u{301}👨‍👩‍👧Z\nlast",
            ),
        ];

        for (name, text, cursor, arrow, expected_cursor, expected_text) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);

            assert_eq!(
                prompt.handle_event(&key(arrow, KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: move with arrow key"
            );
            assert_eq!(prompt.mode(), Mode::Insert, "{name}: remain in insert mode");
            assert_eq!(
                prompt.cursor(),
                expected_cursor,
                "{name}: cross line boundary"
            );

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('Z'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: insert at destination"
            );
            assert_eq!(
                prompt.text(),
                expected_text,
                "{name}: preserve cursor semantics"
            );
        }
    }

    #[test]
    fn normal_mode_line_boundary_delete_preserves_empty_line_separators() {
        let cases = [
            ("empty first line", "\ntwo", 0),
            ("empty middle line", "one\n\ntwo", 4),
            ("empty final line", "one\n", 4),
            ("only a newline", "\n", 0),
            ("empty draft", "", 0),
        ];

        for (name, text, cursor) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);
            let original_history = prompt.buffer().undo_history.node_count();

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('$'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: select empty line"
            );
            assert_eq!(prompt.cursor(), cursor, "{name}: remain on empty line");

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: ignore deletion on empty line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve newline separator");
            assert_eq!(prompt.cursor(), cursor, "{name}: preserve cursor");
            assert_eq!(
                prompt.buffer().undo_history.node_count(),
                original_history,
                "{name}: avoid creating an undo transaction"
            );
        }
    }

    #[test]
    fn normal_mode_line_boundary_append_stays_on_empty_line() {
        let mut prompt = PromptBuffer::new("one\n\ntwo");
        prompt.set_cursor(4);
        prompt.set_mode(Mode::Normal);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('a'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.mode(), Mode::Insert);
        assert_eq!(prompt.cursor(), 4);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('Z'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "one\nZ\ntwo");
        assert_eq!(prompt.cursor(), 5);
    }

    #[test]
    fn normal_mode_supports_vim_word_deletion_and_real_undo() {
        let mut prompt = PromptBuffer::new("first second");
        prompt.set_cursor(0);
        prompt.set_mode(Mode::Normal);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('w'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "second");
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('u'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first second");
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('r'), KeyModifiers::CONTROL), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "second");
    }

    #[test]
    fn normal_mode_line_end_selects_the_current_lines_final_grapheme() {
        let cases = [
            ("single line", "abc", 0, 2, Some("ab"), "abZc"),
            ("first line", "abc\ndef", 0, 2, Some("ab\ndef"), "abZc\ndef"),
            (
                "middle line",
                "one\ntwo\nthree",
                5,
                6,
                Some("one\ntw\nthree"),
                "one\ntwZo\nthree",
            ),
            ("last line", "one\ntwo", 5, 6, Some("one\ntw"), "one\ntwZo"),
            (
                "Unicode graphemes",
                "e\u{301}👨‍👩‍👧\nlast",
                0,
                1,
                Some("e\u{301}\nlast"),
                "e\u{301}Z👨‍👩‍👧\nlast",
            ),
            ("empty line", "first\n\nlast", 6, 6, None, "first\nZ\nlast"),
            ("empty draft", "", 0, 0, None, "Z"),
        ];

        for (name, text, cursor, line_end, expected_delete, expected_insert) in cases {
            let at_line_end = || {
                let mut prompt = PromptBuffer::new(text);
                prompt.set_cursor(cursor);
                prompt.set_mode(Mode::Normal);

                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('$'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: move to current line end"
                );
                assert_eq!(prompt.mode(), Mode::Normal, "{name}: remain in normal mode");
                assert_eq!(prompt.cursor(), line_end, "{name}: select final grapheme");
                prompt
            };

            if let Some(expected) = expected_delete {
                let mut prompt = at_line_end();
                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: delete final grapheme"
                );
                assert_eq!(prompt.text(), expected, "{name}: preserve other lines");
            }

            let mut prompt = at_line_end();
            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('i'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: enter insert mode"
            );
            assert_eq!(prompt.mode(), Mode::Insert, "{name}: enter insert mode");
            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('Z'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: insert before final grapheme"
            );
            assert_eq!(
                prompt.text(),
                expected_insert,
                "{name}: insert on current line"
            );
        }
    }

    #[test]
    fn normal_mode_append_stays_on_the_current_prompt_line() {
        let cases = [
            ("first line", "first\nsecond", 2, 5, "first!\nsecond"),
            ("middle line", "one\ntwo\nthree", 5, 7, "one\ntwo!\nthree"),
            ("last line", "first\nlast", 8, 10, "first\nlast!"),
            ("empty line", "first\n\nlast", 6, 6, "first\n!\nlast"),
            (
                "Unicode graphemes",
                "e\u{301}👨‍👩‍👧\nlast",
                0,
                2,
                "e\u{301}👨‍👩‍👧!\nlast",
            ),
        ];

        for (name, text, cursor, line_end, expected) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('A'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: enter append mode"
            );
            assert_eq!(prompt.mode(), Mode::Insert, "{name}: enter insert mode");
            assert_eq!(prompt.cursor(), line_end, "{name}: stay on current line");

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('!'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: append to current line"
            );
            assert_eq!(prompt.text(), expected, "{name}: preserve remaining lines");
            assert_eq!(prompt.cursor(), line_end + 1, "{name}: advance cursor");
        }
    }

    #[test]
    fn normal_mode_open_line_enters_insert_without_sending() {
        let mut prompt = PromptBuffer::new("first\nlast");
        prompt.set_cursor(2);
        prompt.set_mode(Mode::Normal);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('o'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first\n\nlast");
        assert_eq!(prompt.mode(), Mode::Insert);
    }

    #[test]
    fn history_navigation_restores_the_exact_unsent_draft() {
        let mut prompt = PromptBuffer::with_history(
            "unsent draft",
            vec!["newer\r\nentry".to_string(), "older".to_string()],
        );

        assert!(prompt.history_previous());
        assert_eq!(prompt.text(), "newer\nentry");
        assert!(prompt.history_previous());
        assert_eq!(prompt.text(), "older");
        assert!(prompt.history_next());
        assert_eq!(prompt.text(), "newer\nentry");
        assert!(prompt.history_next());
        assert_eq!(prompt.text(), "unsent draft");
    }

    #[test]
    fn history_navigation_detaches_after_successful_prompt_deletions() {
        type PromptEdit = fn(&mut PromptBuffer) -> bool;

        let scenarios: [(&str, PromptEdit, &str); 4] = [
            ("backspace", PromptBuffer::backspace, "recalled entr"),
            (
                "delete",
                |prompt| {
                    prompt.set_cursor(0);
                    prompt.delete()
                },
                "ecalled entry",
            ),
            (
                "delete previous word",
                PromptBuffer::delete_previous_word,
                "recalled ",
            ),
            (
                "Vim dw",
                |prompt| {
                    prompt.set_cursor(0);
                    prompt.set_mode(Mode::Normal);
                    assert_eq!(
                        prompt.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE), 40),
                        PromptInput::Changed
                    );
                    prompt.handle_event(&key(KeyCode::Char('w'), KeyModifiers::NONE), 40)
                        == PromptInput::Changed
                },
                "entry",
            ),
        ];

        for (name, edit, expected) in scenarios {
            let mut prompt =
                PromptBuffer::with_history("unsent draft", vec!["recalled entry".to_string()]);

            assert!(prompt.history_previous(), "{name}: recall prompt");
            assert!(edit(&mut prompt), "{name}: apply deletion");
            assert_eq!(prompt.text(), expected, "{name}: preserve edited text");
            assert!(
                !prompt.history_next(),
                "{name}: deletion must detach history navigation"
            );
            assert_eq!(prompt.text(), expected, "{name}: keep edited draft");

            assert!(prompt.history_previous(), "{name}: restart navigation");
            assert_eq!(prompt.text(), "recalled entry");
            assert!(prompt.history_next(), "{name}: restore edited draft");
            assert_eq!(prompt.text(), expected);
        }
    }

    #[test]
    fn unsuccessful_prompt_deletion_preserves_history_navigation() {
        let mut prompt =
            PromptBuffer::with_history("unsent draft", vec!["recalled entry".to_string()]);

        assert!(prompt.history_previous());
        prompt.set_cursor(0);
        assert!(!prompt.backspace());
        assert!(prompt.history_next());
        assert_eq!(prompt.text(), "unsent draft");
    }

    #[test]
    fn submission_preserves_exact_text_and_deduplicates_bounded_history() {
        let mut prompt = PromptBuffer::with_history(
            "  first\nsecond  ",
            vec!["  first\nsecond  ".to_string(), "older".to_string()],
        );

        assert_eq!(
            prompt.take_submission().as_deref(),
            Some("  first\nsecond  ")
        );
        assert_eq!(prompt.text(), "");
        assert_eq!(prompt.cursor(), 0);
        assert_eq!(prompt.mode(), Mode::Insert);
        assert_eq!(prompt.history(), ["  first\nsecond  ", "older"]);
        assert!(prompt.buffer().file.is_none());
    }

    #[test]
    fn blank_submission_never_discards_the_existing_draft() {
        let mut prompt = PromptBuffer::new(" \n\t");

        assert!(prompt.take_submission().is_none());
        assert_eq!(prompt.text(), " \n\t");
    }

    #[test]
    fn oversized_edits_never_replace_the_existing_buffer_or_history() {
        let mut prompt = PromptBuffer::new("safe draft");
        let oversized = "x".repeat(PROMPT_MAX_BYTES);

        assert!(!prompt.insert(&oversized));
        assert_eq!(prompt.text(), "safe draft");
        assert_eq!(prompt.buffer().undo_history.node_count(), 0);
        assert!(!prompt.set_text(&"x".repeat(PROMPT_MAX_BYTES + 1)));
        assert_eq!(prompt.text(), "safe draft");
    }

    #[test]
    fn control_s_is_not_a_hidden_submission_shortcut() {
        let mut prompt = PromptBuffer::new("keep editing");

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('s'), KeyModifiers::CONTROL), 40),
            PromptInput::Unhandled
        );
        assert_eq!(prompt.text(), "keep editing");
    }
}
