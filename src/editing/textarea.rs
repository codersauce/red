//! Host-independent, rope-backed Vim editing for embedded text surfaces.

mod vim;

use std::{borrow::Cow, cell::OnceCell};

use crossterm::event::Event;
use vimlet::{Capabilities, EditingState, Lifecycle, Session};

use super::apply_transactional_replacement;
use crate::{
    buffer::Buffer,
    editor::Mode,
    text_layout::LayoutOptions,
    undo::{CursorSnapshot, TextPosition, TextRange},
    unicode_utils::{char_to_grapheme, grapheme_len, grapheme_to_byte, previous_word_start},
};

const DEFAULT_MAX_BYTES: usize = 128 * 1024;

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
    pending: bool,
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            mode: Mode::Insert,
            cursor: 0,
            preferred_column: None,
            selection_anchor: None,
            pending: false,
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
        self.pending
    }
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
    register: OnceCell<RegisterContent>,
    engine: EditingState,
    session: Session,
    selection: Option<vimlet::Selection>,
    search: Option<vim::Search>,
    last_search: Option<(String, bool)>,
    cursor_byte: vimlet::ByteOffset,
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
            normalized.as_ref()
        } else {
            ""
        };
        let mut area = Self {
            buffer: unnamed_buffer(text),
            state: EditState {
                cursor: grapheme_len(text),
                ..EditState::default()
            },
            max_bytes,
            register: OnceCell::new(),
            engine: EditingState::new(vimlet::Mode::Insert, capabilities()),
            session: Session::with_limits(vimlet::Limits {
                recorded_keys: 1_000,
                ..Default::default()
            }),
            selection: None,
            search: None,
            last_search: None,
            cursor_byte: vimlet::ByteOffset(text.len()),
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
        self.register.get_or_init(|| {
            self.session
                .registers()
                .get('"')
                .map(|r| RegisterContent {
                    text: r.text().to_owned(),
                    linewise: r.shape() == vimlet::Shape::Line,
                })
                .unwrap_or_default()
        })
    }

    /// Replaces the local register, allowing a host to share its global clipboard.
    pub fn set_register(&mut self, register: RegisterContent) {
        let shape = if register.linewise {
            vimlet::Shape::Line
        } else {
            vimlet::Shape::Character
        };
        self.session
            .registers_mut()
            .set('"', vimlet::Register::from_text(register.text, shape));
        self.register.take();
    }

    /// Updates the surface-local mode without changing any surrounding editor.
    pub fn set_mode(&mut self, mode: Mode) {
        let shape = match mode {
            Mode::Visual => Some(vimlet::Shape::Character),
            Mode::VisualLine => Some(vimlet::Shape::Line),
            Mode::VisualBlock => Some(vimlet::Shape::Block { width: 1 }),
            Mode::Normal | Mode::Insert => None,
            _ => return,
        };
        self.close_transaction();
        self.session
            .lifecycle(&mut self.engine, Lifecycle::LeaveMode);
        self.engine = EditingState::new(
            match mode {
                Mode::Visual => vimlet::Mode::Visual,
                Mode::VisualLine => vimlet::Mode::VisualLine,
                Mode::VisualBlock => vimlet::Mode::VisualBlock,
                Mode::Insert => vimlet::Mode::Insert,
                _ => vimlet::Mode::Normal,
            },
            capabilities(),
        );
        self.search = None;
        self.selection = shape.map(|shape| vimlet::Selection {
            anchor: self.cursor_byte,
            cursor: self.cursor_byte,
            shape,
        });
        self.state.selection_anchor = shape.map(|_| self.state.cursor);
        self.state.mode = mode;
        self.state.pending = false;
    }

    /// Moves the cursor to a bounded, absolute extended-grapheme position.
    pub fn set_cursor(&mut self, cursor: usize) {
        self.state.cursor = cursor.min(self.document_grapheme_len());
        self.state.preferred_column = None;
        self.sync_buffer_cursor();
    }

    /// Replaces the complete text as one undoable transaction.
    pub fn set_text(&mut self, text: &str) -> bool {
        let text = normalize_newlines(text);
        if text.len() > self.max_bytes {
            return false;
        }
        let end = self.document_grapheme_len();
        self.replace_graphemes(0, end, &text, grapheme_len(&text), "replace text area")
    }

    /// Inserts normalized text as one undoable transaction.
    pub fn insert(&mut self, text: &str) -> bool {
        let text = normalize_newlines(text);
        if text.is_empty() {
            return false;
        }
        let cursor = self.state.cursor;
        if self.buffer.is_ascii() && text.is_ascii() {
            return self.replace_graphemes(
                cursor,
                cursor,
                &text,
                cursor.saturating_add(text.len()),
                "insert text",
            );
        }
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
        if self.state.cursor >= self.document_grapheme_len() {
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
        if self.buffer.is_ascii() {
            let mut start = self.state.cursor;
            let mut seen_word = false;
            for character in self
                .buffer
                .contents_snapshot()
                .chars_at(self.state.cursor)
                .reversed()
            {
                let whitespace = character.is_whitespace();
                if seen_word && whitespace {
                    break;
                }
                seen_word |= !whitespace;
                start -= 1;
            }
            return self.replace_graphemes(
                start,
                self.state.cursor,
                "",
                start,
                "delete previous word",
            );
        }
        let text = self.text();
        let end = grapheme_to_byte(&text, self.state.cursor);
        let start = grapheme_len(&text[..previous_word_start(&text[..end])]);
        self.replace_graphemes(start, self.state.cursor, "", start, "delete previous word")
    }

    /// Removes everything between the start of the cursor's line and the cursor.
    pub fn delete_to_line_start(&mut self) -> bool {
        let text = self.text();
        let end = grapheme_to_byte(&text, self.state.cursor);
        let start = grapheme_len(&text[..text[..end].rfind('\n').map_or(0, |index| index + 1)]);
        if start == self.state.cursor {
            return false;
        }
        self.replace_graphemes(start, self.state.cursor, "", start, "delete to line start")
    }

    /// Undoes one transaction and restores its exact grapheme cursor snapshot.
    pub fn undo(&mut self) -> bool {
        self.close_transaction();
        let mut history = std::mem::take(&mut self.buffer.undo_history);
        let restored = history.undo(&mut self.buffer);
        self.buffer.undo_history = history;
        let Some((cursor, _)) = restored else {
            return false;
        };
        self.restore_cursor(cursor);
        self.buffer.refresh_dirty();
        true
    }

    /// Redoes one transaction from the selected undo-tree branch.
    pub fn redo(&mut self) -> bool {
        self.close_transaction();
        let mut history = std::mem::take(&mut self.buffer.undo_history);
        let restored = history.redo(&mut self.buffer);
        self.buffer.undo_history = history;
        let Some((cursor, _)) = restored else {
            return false;
        };
        self.restore_cursor(cursor);
        self.buffer.refresh_dirty();
        true
    }

    /// Clears text and pending commands while preserving the configured byte limit.
    pub fn clear(&mut self) {
        self.close_transaction();
        self.buffer = unnamed_buffer("");
        self.cursor_byte = vimlet::ByteOffset(0);
        self.state = EditState::default();
        self.search = None;
        self.selection = None;
        self.session
            .lifecycle(&mut self.engine, Lifecycle::NewDraft);
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
        self.dispatch_event(event, layout)
    }

    fn document_grapheme_len(&self) -> usize {
        if self.buffer.is_ascii() {
            self.buffer.byte_len()
        } else {
            grapheme_len(&self.text())
        }
    }

    fn cursor_position(&self) -> TextPosition {
        self.position_for_grapheme(self.state.cursor)
    }

    fn position_for_grapheme(&self, index: usize) -> TextPosition {
        if self.buffer.is_ascii() {
            return self.buffer.char_idx_to_position(index);
        }
        let text = self.text();
        let byte = grapheme_to_byte(&text, index);
        self.buffer
            .char_idx_to_position(text[..byte].chars().count())
    }

    fn replace_graphemes(
        &mut self,
        start: usize,
        end: usize,
        replacement: &str,
        cursor: usize,
        label: &str,
    ) -> bool {
        let ascii = self.buffer.is_ascii() && replacement.is_ascii();
        let range = if ascii {
            let range = TextRange::new(
                self.buffer.char_idx_to_position(start),
                self.buffer.char_idx_to_position(end),
            );
            let previous = self.buffer.text_in_range(range);
            if previous == replacement
                || self
                    .buffer
                    .byte_len()
                    .saturating_sub(previous.len())
                    .saturating_add(replacement.len())
                    > self.max_bytes
            {
                return false;
            }
            range
        } else {
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
            TextRange::new(
                self.buffer
                    .char_idx_to_position(contents[..start_byte].chars().count()),
                self.buffer
                    .char_idx_to_position(contents[..end_byte].chars().count()),
            )
        };
        // Programmatic prompt edits each form their own undo group.
        self.close_transaction();
        let before = self.cursor_snapshot();
        self.buffer.undo_history.begin_transaction(label, before);
        apply_transactional_replacement(&mut self.buffer, range, replacement);
        self.state.cursor = if ascii {
            cursor.min(self.buffer.byte_len())
        } else {
            cursor.min(self.document_grapheme_len())
        };
        self.state.preferred_column = None;
        self.sync_buffer_cursor();
        self.close_transaction();
        true
    }

    fn close_transaction(&mut self) {
        if self.buffer.undo_history.is_transaction_active() {
            let (x, y) = self.buffer.pos;
            self.buffer
                .undo_history
                .commit_transaction(CursorSnapshot::new(x, y, 0));
            self.buffer.refresh_dirty();
        }
    }

    fn cursor_snapshot(&self) -> CursorSnapshot {
        let position = self.cursor_position();
        if self.buffer.is_ascii() {
            return CursorSnapshot::new(position.character, position.line, 0);
        }
        let line = self.buffer.get(position.line).unwrap_or_default();
        CursorSnapshot::new(
            char_to_grapheme(&line, position.character),
            position.line,
            0,
        )
    }

    fn sync_buffer_cursor(&mut self) {
        let position = self.cursor_position();
        self.cursor_byte = vimlet::ByteOffset(self.buffer.position_to_byte_idx(position));
        self.buffer.pos = if self.buffer.is_ascii() {
            (position.character, position.line)
        } else {
            let line = self.buffer.get(position.line).unwrap_or_default();
            (char_to_grapheme(&line, position.character), position.line)
        };
        if let Some(selection) = &mut self.selection {
            selection.cursor = self.cursor_byte;
        }
    }

    fn restore_cursor(&mut self, snapshot: CursorSnapshot) {
        self.state.cursor = if self.buffer.is_ascii() {
            let start = self
                .buffer
                .position_to_char_idx(TextPosition::new(snapshot.y, 0));
            let end = self
                .buffer
                .position_to_char_idx(TextPosition::new(snapshot.y.saturating_add(1), 0));
            start
                .saturating_add(snapshot.x.min(end.saturating_sub(start)))
                .min(self.buffer.char_len())
        } else {
            let prefix = self.buffer.line_range_contents(0, snapshot.y);
            let line = self.buffer.get(snapshot.y).unwrap_or_default();
            grapheme_len(&prefix)
                .saturating_add(snapshot.x.min(grapheme_len(&line)))
                .min(self.document_grapheme_len())
        };
        self.state.preferred_column = None;
        self.sync_buffer_cursor();
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        replace: false,
        ..Default::default()
    }
}

fn normalize_newlines(text: &str) -> Cow<'_, str> {
    if !text.as_bytes().contains(&b'\r') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
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
    buffer.mark_saved();
    buffer
}

#[cfg(test)]
mod tests {

    #[test]
    fn ascii_edits_preserve_multiline_cursor_snapshots_and_undo_redo() {
        let mut area = TextArea::new("alpha\nbravo\ncharlie");
        area.set_cursor("alpha\nbr".len());

        assert!(area.insert("XY"));
        assert_eq!(area.text(), "alpha\nbrXYavo\ncharlie");
        assert_eq!(area.cursor(), "alpha\nbrXY".len());
        assert_eq!(area.buffer.pos, (4, 1));

        assert!(area.undo());
        assert_eq!(area.text(), "alpha\nbravo\ncharlie");
        assert_eq!(area.cursor(), "alpha\nbr".len());
        assert_eq!(area.buffer.pos, (2, 1));

        assert!(area.redo());
        assert_eq!(area.text(), "alpha\nbrXYavo\ncharlie");
        assert_eq!(area.cursor(), "alpha\nbrXY".len());
        assert_eq!(area.buffer.pos, (4, 1));
    }

    #[test]
    fn ascii_fast_path_preserves_newline_normalization_and_byte_capacity() {
        let mut area = TextArea::with_max_bytes("ab", 7);
        area.set_cursor(1);

        assert!(area.insert("x\r\ny\rz"));
        assert_eq!(area.text(), "ax\ny\nzb");
        assert_eq!(area.cursor(), 6);
        assert!(!area.insert("q"));
        assert_eq!(area.text(), "ax\ny\nzb");
        assert!(area.undo());
        assert_eq!(area.text(), "ab");
        assert_eq!(area.cursor(), 1);
    }

    #[test]
    fn indexed_ascii_deletion_preserves_word_whitespace_unicode_and_undo() {
        for original in [
            "",
            "word",
            "one two three",
            "one   two   ",
            "\tleading\twords\t",
            "first\n\nsecond word",
            "e\u{301}clair 👨‍👩‍👧 family",
            "漢字\u{3000}かな",
        ] {
            let count = crate::unicode_utils::grapheme_len(original);
            for cursor in 0..=count {
                let mut area = TextArea::new(original);
                area.set_cursor(cursor);
                let byte = super::grapheme_to_byte(original, cursor);
                let start = crate::unicode_utils::previous_word_start(&original[..byte]);
                let expected = format!("{}{}", &original[..start], &original[byte..]);
                assert_eq!(area.delete_previous_word(), cursor != 0);
                assert_eq!(area.text(), expected, "{original:?} at cursor {cursor}");
                assert_eq!(
                    area.cursor(),
                    crate::unicode_utils::grapheme_len(&original[..start]),
                    "{original:?} at cursor {cursor}"
                );
                if cursor != 0 {
                    assert!(area.undo());
                    assert_eq!(area.text(), original);
                    assert_eq!(area.cursor(), cursor);
                }

                let mut area = TextArea::new(original);
                area.set_cursor(cursor);
                if cursor == count {
                    assert!(!area.delete());
                } else {
                    let end = super::grapheme_to_byte(original, cursor + 1);
                    assert!(area.delete());
                    assert_eq!(
                        area.text(),
                        format!("{}{}", &original[..byte], &original[end..]),
                        "{original:?} at cursor {cursor}"
                    );
                    assert_eq!(area.cursor(), cursor);
                    assert!(area.undo());
                    assert_eq!(area.text(), original);
                    assert_eq!(area.cursor(), cursor);
                }
            }
        }
    }

    #[test]
    fn indexed_document_boundaries_preserve_ascii_and_unicode_home_end_navigation() {
        for original in [
            "",
            "plain ascii draft",
            "first\n\nlast\n",
            "normalized\r\nwindows\rline",
            "e\u{301} 👨‍👩‍👧\n漢字 🇧🇷",
            "\u{301}leading\ntrailing\u{200d}",
        ] {
            let mut area = TextArea::new(original);
            let text = area.text();
            let expected = crate::unicode_utils::grapheme_len(&text);
            assert_eq!(area.document_grapheme_len(), expected);
            for code in [KeyCode::Home, KeyCode::End, KeyCode::Home, KeyCode::End] {
                assert_eq!(
                    area.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 80),
                    TextAreaOutcome::Changed
                );
                assert_eq!(
                    area.cursor(),
                    if code == KeyCode::Home { 0 } else { expected },
                    "{original:?} after {code:?}"
                );
            }
            area.set_cursor(usize::MAX);
            assert_eq!(area.cursor(), expected);
        }
    }

    #[test]
    fn indexed_cursor_restoration_matches_unicode_reference_for_every_snapshot() {
        for original in [
            "",
            "alpha",
            "alpha\nbravo\ncharlie",
            "alpha\n\nbravo\n",
            "\tleading words\t\nnext",
            "e\u{301} 👨‍👩‍👧\n漢字 🇧🇷",
            "\u{301}leading\ntrailing\u{200d}",
        ] {
            let mut area = TextArea::new(original);
            for line_index in 0..=area.buffer.len() + 2 {
                for column in [0, 1, 2, 8, 128, usize::MAX] {
                    let prefix = area.buffer.line_range_contents(0, line_index);
                    let line = area.buffer.get(line_index).unwrap_or_default();
                    let expected = crate::unicode_utils::grapheme_len(&prefix)
                        .saturating_add(column.min(crate::unicode_utils::grapheme_len(&line)))
                        .min(crate::unicode_utils::grapheme_len(original));
                    area.restore_cursor(crate::undo::CursorSnapshot::new(column, line_index, 0));
                    assert_eq!(
                        area.cursor(),
                        expected,
                        "{original:?} at line {line_index}, column {column}"
                    );
                }
            }
        }
    }

    #[test]
    fn borrowed_textarea_loading_preserves_normalized_capacity_and_unicode_cursors() {
        let area = TextArea::with_max_bytes("ab\r\ncd\r", 6);
        assert_eq!(area.text(), "ab\ncd\n");
        assert_eq!(area.cursor(), 6);
        assert_eq!(area.buffer.pos, (0, 2));

        let oversized = TextArea::with_max_bytes("abcdefg", 6);
        assert_eq!(oversized.text(), "");
        assert_eq!(oversized.cursor(), 0);

        let unicode = TextArea::new("e\u{301}\r\n👋");
        assert_eq!(unicode.text(), "e\u{301}\n👋");
        assert_eq!(unicode.cursor(), 3);
        assert_eq!(unicode.buffer.pos, (1, 1));
    }

    #[test]
    fn ascii_to_unicode_transition_preserves_combining_grapheme_cursors() {
        let mut area = TextArea::new("ab");
        area.set_cursor(1);

        assert!(area.insert("👋"));
        assert_eq!(area.text(), "a👋b");
        assert_eq!(area.cursor(), 2);
        assert!(!area.buffer.is_ascii());

        assert!(area.insert("\u{301}"));
        assert_eq!(area.text(), "a👋\u{301}b");
        assert_eq!(area.cursor(), 2);
        assert!(area.undo());
        assert_eq!(area.text(), "a👋b");
        assert_eq!(area.cursor(), 2);
    }

    #[test]
    fn indexed_ascii_cursor_positions_match_unicode_aware_reference_boundaries() {
        let samples = [
            "",
            "\n",
            "\n\n",
            "alpha",
            "alpha\nbravo\ncharlie",
            "alpha\n\nbravo\n",
            "\t leading spaces\t\nnext\tline",
            "normalized\r\nwindows\rnewlines",
            "e\u{301} 👨‍👩‍👧\n漢字 🇧🇷",
            "\u{301}leading\ntrailing\u{200d}",
        ];

        for sample in samples {
            let mut area = TextArea::new(sample);
            let contents = area.text();
            let grapheme_count = crate::unicode_utils::grapheme_len(&contents);
            for cursor in 0..=grapheme_count + 2 {
                area.set_cursor(cursor);
                let bounded = cursor.min(grapheme_count);
                let byte = super::grapheme_to_byte(&contents, bounded);
                let expected_start = contents[..byte].rfind('\n').map_or(0, |index| {
                    crate::unicode_utils::grapheme_len(&contents[..index + 1])
                });
                let expected_end = contents[byte..].find('\n').map_or_else(
                    || crate::unicode_utils::grapheme_len(&contents),
                    |index| crate::unicode_utils::grapheme_len(&contents[..byte + index]),
                );
                use vimlet::TextSource;
                let rope = area.buffer.contents_snapshot();
                let index_at = |target| super::vim::index_from(&rope, (0, 0), target);
                assert_eq!(area.cursor_byte.0, byte);
                assert_eq!(area.cursor(), bounded, "{sample:?} at cursor {cursor}");
                assert_eq!(
                    index_at(rope.line_start(byte)),
                    expected_start,
                    "{sample:?} at cursor {cursor}"
                );
                assert_eq!(
                    index_at(rope.line_end(byte)),
                    expected_end,
                    "{sample:?} at cursor {cursor}"
                );

                for line in 0..=area.buffer.len() + 1 {
                    for character in [0, 1, 2, 32, usize::MAX] {
                        let position = crate::undo::TextPosition::new(line, character);
                        let index = area.buffer.position_to_char_idx(position);
                        let byte = contents
                            .char_indices()
                            .nth(index)
                            .map_or(contents.len(), |(byte, _)| byte);
                        let expected = crate::unicode_utils::grapheme_len(&contents[..byte]);
                        assert_eq!(
                            index_at(area.buffer.position_to_byte_idx(position)),
                            expected,
                            "{sample:?} at line {line}, character {character}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rope_delimiter_matching_preserves_nested_and_unicode_cursor_positions() {
        let samples = [
            "",
            "no delimiters",
            "(alpha)",
            "(one (two) three)",
            "before [one [two] three] after",
            "{one {two} three}",
            "([{}])",
            "(unclosed",
            "closing)",
            "first (\n nested (word)\n final)\n",
            "e\u{301} (👨‍👩‍👧 [漢字]) 🇧🇷",
            "\u{301}leading {👩‍💻 {λ}} trailing",
        ];

        for sample in samples {
            let mut area = TextArea::new(sample);
            area.set_mode(crate::editor::Mode::Normal);
            let text = area.text();
            let chars = text.chars().collect::<Vec<_>>();
            for offset in 0..=crate::unicode_utils::grapheme_len(&text) {
                area.set_cursor(offset);
                let byte = super::grapheme_to_byte(&text, offset);
                let scalar = text[..byte].chars().count();
                let delimiter = (scalar..chars.len())
                    .take_while(|&i| chars[i] != '\n')
                    .find(|&i| "()[]{}".contains(chars[i]));
                let expected = delimiter.and_then(|scalar| {
                    let character = chars[scalar];
                    let (opening, closing, forward) = match character {
                        '(' => ('(', ')', true),
                        '[' => ('[', ']', true),
                        '{' => ('{', '}', true),
                        ')' => ('(', ')', false),
                        ']' => ('[', ']', false),
                        '}' => ('{', '}', false),
                        _ => return None,
                    };
                    let mut depth = 0usize;
                    let indices: Box<dyn Iterator<Item = usize>> = if forward {
                        Box::new(scalar..chars.len())
                    } else {
                        Box::new((0..=scalar).rev())
                    };
                    for index in indices {
                        if chars[index] == if forward { opening } else { closing } {
                            depth += 1;
                        } else if chars[index] == if forward { closing } else { opening } {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                let target = text
                                    .char_indices()
                                    .nth(index)
                                    .map_or(text.len(), |(byte, _)| byte);
                                return Some(crate::unicode_utils::grapheme_len(&text[..target]));
                            }
                        }
                    }
                    None
                });

                keys(&mut area, "%");
                assert_eq!(
                    area.cursor(),
                    expected.unwrap_or(offset),
                    "{sample:?} at grapheme {offset}"
                );
            }
        }
    }

    #[test]
    fn word_backspace_in_search_edits_only_the_search_pattern() {
        for key in [
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        ] {
            let mut area = normal("keep this draft");
            keys(&mut area, "/first second");
            assert_eq!(area.mode(), Mode::Search);
            area.handle_event(&Event::Key(key), 80);
            assert_eq!(area.search.as_ref().unwrap().pattern, "first ");
            assert_eq!(area.text(), "keep this draft");
            assert_eq!(area.mode(), Mode::Search);
        }
    }

    #[test]
    fn empty_textarea_has_an_exact_clean_baseline() {
        let mut buffer = super::unnamed_buffer("");
        assert!(!buffer.is_dirty());
        buffer.insert_str(0, 0, "x");
        assert!(buffer.is_dirty());
        buffer.remove(0, 0);
        assert_eq!(buffer.contents(), "");
        assert!(!buffer.is_dirty());
    }

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
    fn batched_insert_replay_matches_per_key_unicode_and_capacity_behavior() {
        for text in [
            "plain text",
            "λ😀tail",
            "\u{301}tail",
            "a\u{301}b",
            "\u{600}a",
            "👩\u{200d}💻 tail",
            "first\nlast",
        ] {
            for cursor in [0, 1, crate::unicode_utils::grapheme_len(text)] {
                for capacity in [text.len() + 5, 4096] {
                    for recipe in [
                        "iHello world\u{1b}",
                        "iλx\u{301}y\u{1b}",
                        "ihello\nworld\u{1b}",
                    ] {
                        let mut fast = TextArea::with_max_bytes(text, capacity);
                        let mut slow = TextArea::with_max_bytes(text, capacity);
                        for area in [&mut fast, &mut slow] {
                            area.set_cursor(cursor);
                            area.set_mode(Mode::Normal);
                        }
                        fast.session.registers_mut().set(
                            'a',
                            vimlet::Register::from_text(recipe, vimlet::Shape::Character),
                        );
                        keys(&mut fast, "@a");
                        for ch in recipe.chars() {
                            let code = if ch == '\u{1b}' {
                                KeyCode::Esc
                            } else {
                                KeyCode::Char(ch)
                            };
                            slow.handle_event(
                                &Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                                80,
                            );
                        }
                        assert_eq!(
                            (fast.text(), fast.cursor(), fast.mode()),
                            (slow.text(), slow.cursor(), slow.mode()),
                            "text={text:?}, cursor={cursor}, capacity={capacity}"
                        );
                        fast.undo();
                        slow.undo();
                        assert_eq!((fast.text(), fast.cursor()), (slow.text(), slow.cursor()));
                        fast.redo();
                        slow.redo();
                        assert_eq!((fast.text(), fast.cursor()), (slow.text(), slow.cursor()));
                    }
                }
            }
        }
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
