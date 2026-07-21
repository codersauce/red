//! A compact, multiline prompt composer for agent requests.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde_json::json;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    config::KeyAction,
    editor::{Action, ComposerCallback, Editor, RenderBuffer},
    plugin::ComposerHandle,
    theme::{Style, Theme},
    unicode_utils::{display_width, grapheme_len, grapheme_to_byte, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog},
    Component,
};

const TAB_WIDTH: usize = 4;
const MAX_PROMPT_BYTES: usize = 128 * 1024;
const STATUS: &str = "Enter  ^J newline  Esc  ^P/N";
const EMPTY_STATUS: &str = "Prompt is empty";
const OVERSIZED_STATUS: &str = "Prompt exceeds 128 KiB";

#[derive(Debug)]
pub(crate) struct WrappedText {
    pub(crate) rows: Vec<String>,
    pub(crate) positions: Vec<(usize, usize)>,
}

/// A cursor-aware, multiline composer that submits its complete contents atomically.
pub struct AgentComposer {
    target: ComposerTarget,
    dialog: Dialog,
    query: String,
    cursor: usize,
    history: Vec<String>,
    history_position: Option<usize>,
    history_draft: Option<String>,
    preferred_column: Option<usize>,
    validation_status: Option<&'static str>,
    viewport_width: usize,
    viewport_height: usize,
    style: Style,
    muted_style: Style,
    theme: Theme,
}

#[derive(Debug)]
enum ComposerTarget {
    Legacy { owner: String, id: i32 },
    Callback(ComposerHandle),
}

impl AgentComposer {
    /// Creates a right-aligned composer with the cursor at the end of `query`.
    pub fn new(
        editor: &Editor,
        title: Option<String>,
        id: i32,
        query: String,
        history: Vec<String>,
        owner: String,
    ) -> Self {
        Self::with_target(
            editor,
            title,
            query,
            history,
            ComposerTarget::Legacy { owner, id },
        )
    }

    /// Creates a composer whose result is delivered through a scoped callback.
    pub fn new_callback(
        editor: &Editor,
        title: Option<String>,
        query: String,
        history: Vec<String>,
        handle: ComposerHandle,
    ) -> Self {
        Self::with_target(
            editor,
            title,
            query,
            history,
            ComposerTarget::Callback(handle),
        )
    }

    fn with_target(
        editor: &Editor,
        title: Option<String>,
        query: String,
        history: Vec<String>,
        target: ComposerTarget,
    ) -> Self {
        let theme = editor.theme.clone();
        let style = theme.ui_style.popup.clone();
        let border_style = theme.ui_style.popup_border.clone();
        let title_style = theme.ui_style.popup_title.clone();
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight();
        let (x, y, width, height) = Self::geometry(viewport_width, viewport_height);
        let initial_too_large = query.len() > MAX_PROMPT_BYTES;
        let query = if initial_too_large {
            String::new()
        } else {
            normalize_newlines(&query)
        };
        let history_len = history.len();
        let history = history
            .into_iter()
            .filter(|entry| entry.len() <= MAX_PROMPT_BYTES)
            .collect::<Vec<_>>();
        let history_too_large = history.len() != history_len;
        let cursor = grapheme_len(&query);

        Self {
            target,
            dialog: Dialog::new(
                title,
                x,
                y,
                width,
                height,
                &style,
                BorderStyle::Single,
                &theme,
            )
            .with_border_draw_style(&border_style)
            .with_title_style(&title_style),
            query,
            cursor,
            history,
            history_position: None,
            history_draft: None,
            preferred_column: None,
            validation_status: (initial_too_large || history_too_large).then_some(OVERSIZED_STATUS),
            viewport_width,
            viewport_height,
            style,
            muted_style: theme.ui_style.muted.clone(),
            theme,
        }
    }

    fn cancel_action(&self) -> KeyAction {
        match &self.target {
            ComposerTarget::Legacy { owner, id } => KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    owner.clone(),
                    format!("composer:cancelled:{id}"),
                    json!(null),
                ),
            ]),
            ComposerTarget::Callback(handle) => KeyAction::Multiple(vec![
                Action::NotifyComposer(*handle, Box::new(ComposerCallback::Cancelled)),
                Action::CloseDialog,
            ]),
        }
    }

    fn submit_action(&self) -> KeyAction {
        match &self.target {
            ComposerTarget::Legacy { owner, id } => KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    owner.clone(),
                    format!("composer:submitted:{id}"),
                    json!(self.query),
                ),
            ]),
            ComposerTarget::Callback(handle) => KeyAction::Multiple(vec![
                Action::NotifyComposer(
                    *handle,
                    Box::new(ComposerCallback::Submitted(self.query.clone())),
                ),
                Action::CloseDialog,
            ]),
        }
    }

    fn geometry(viewport_width: usize, viewport_height: usize) -> (usize, usize, usize, usize) {
        let outer_width = (viewport_width * 60 / 100)
            .clamp(36, 80)
            .min(viewport_width);
        let outer_height = (viewport_height * 65 / 100)
            .clamp(8, 18)
            .min(viewport_height);
        let x = viewport_width.saturating_sub(outer_width);
        let y = viewport_height.saturating_sub(outer_height) / 2;
        (
            x,
            y,
            outer_width.saturating_sub(2),
            outer_height.saturating_sub(2),
        )
    }

    fn body_height(&self) -> usize {
        if self.dialog.height > 1 {
            self.dialog.height - 1
        } else {
            self.dialog.height
        }
    }

    fn wrapped_text(&self) -> WrappedText {
        wrap_text(&self.query, self.dialog.width)
    }

    fn insert(&mut self, text: &str) {
        if text.len() > MAX_PROMPT_BYTES.saturating_sub(self.query.len()) {
            self.validation_status = Some(OVERSIZED_STATUS);
            return;
        }
        let text = normalize_newlines(text);
        if text.is_empty() {
            return;
        }

        let offset = grapheme_to_byte(&self.query, self.cursor);
        self.query.insert_str(offset, &text);
        self.cursor = self.query[..offset + text.len()].graphemes(true).count();
        self.preferred_column = None;
        self.validation_status = None;
        self.history_position = None;
        self.history_draft = None;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = grapheme_to_byte(&self.query, self.cursor - 1);
        let end = grapheme_to_byte(&self.query, self.cursor);
        self.query.replace_range(start..end, "");
        self.cursor -= 1;
        self.preferred_column = None;
        self.validation_status = None;
        self.history_position = None;
        self.history_draft = None;
    }

    fn delete(&mut self) {
        if self.cursor >= grapheme_len(&self.query) {
            return;
        }
        let start = grapheme_to_byte(&self.query, self.cursor);
        let end = grapheme_to_byte(&self.query, self.cursor + 1);
        self.query.replace_range(start..end, "");
        self.preferred_column = None;
        self.validation_status = None;
        self.history_position = None;
        self.history_draft = None;
    }

    fn delete_previous_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = grapheme_to_byte(&self.query, self.cursor);
        let before = &self.query[..end];
        let mut start = self.cursor;
        let mut seen_word = false;

        for grapheme in before.graphemes(true).rev() {
            let whitespace = grapheme.chars().all(char::is_whitespace);
            if seen_word && whitespace {
                break;
            }
            seen_word |= !whitespace;
            start -= 1;
        }

        let start_byte = grapheme_to_byte(&self.query, start);
        self.query.replace_range(start_byte..end, "");
        self.cursor = start;
        self.preferred_column = None;
        self.validation_status = None;
        self.history_position = None;
        self.history_draft = None;
    }

    fn move_vertically(&mut self, direction: isize) {
        let wrapped = self.wrapped_text();
        let Some(&(row, column)) = wrapped.positions.get(self.cursor) else {
            return;
        };
        let target_row = row.saturating_add_signed(direction);
        if target_row >= wrapped.rows.len() || target_row == row {
            return;
        }
        let goal = *self.preferred_column.get_or_insert(column);
        let mut target = None;
        let mut distance = usize::MAX;

        for (index, &(candidate_row, candidate_column)) in wrapped.positions.iter().enumerate() {
            if candidate_row != target_row {
                continue;
            }
            let candidate_distance = candidate_column.abs_diff(goal);
            if candidate_distance < distance {
                target = Some(index);
                distance = candidate_distance;
            }
        }

        if let Some(target) = target {
            self.cursor = target;
        }
    }

    fn history_back(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let position = match self.history_position {
            Some(position) => (position + 1).min(self.history.len() - 1),
            None => {
                self.history_draft = Some(self.query.clone());
                0
            }
        };
        self.history_position = Some(position);
        self.query.clone_from(&self.history[position]);
        self.query = normalize_newlines(&self.query);
        self.cursor = grapheme_len(&self.query);
        self.preferred_column = None;
        self.validation_status = None;
    }

    fn history_forward(&mut self) {
        let Some(position) = self.history_position else {
            return;
        };
        if position > 0 {
            let next = position - 1;
            self.history_position = Some(next);
            self.query.clone_from(&self.history[next]);
            self.query = normalize_newlines(&self.query);
        } else {
            self.history_position = None;
            self.query = self.history_draft.take().unwrap_or_default();
        }
        self.cursor = grapheme_len(&self.query);
        self.preferred_column = None;
        self.validation_status = None;
    }

    fn redraw() -> Option<KeyAction> {
        Some(KeyAction::Single(Action::ShowDialog))
    }
}

impl Component for AgentComposer {
    fn composer_handle(&self) -> Option<ComposerHandle> {
        match &self.target {
            ComposerTarget::Legacy { .. } => None,
            ComposerTarget::Callback(handle) => Some(*handle),
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let right = self.dialog.x + self.dialog.width + 1;
        let bottom = self.dialog.y + self.dialog.height + 1;
        buffer.set_char(
            self.dialog.x,
            self.dialog.y,
            '┌',
            &self.dialog.border_draw_style,
            &self.theme,
        );
        buffer.set_char(
            right,
            self.dialog.y,
            '┐',
            &self.dialog.border_draw_style,
            &self.theme,
        );
        buffer.set_char(
            self.dialog.x,
            bottom,
            '└',
            &self.dialog.border_draw_style,
            &self.theme,
        );
        buffer.set_char(
            right,
            bottom,
            '┘',
            &self.dialog.border_draw_style,
            &self.theme,
        );
        let body_height = self.body_height();
        let content_x = self.dialog.x + 1;
        let content_y = self.dialog.y + 1;
        if self.dialog.width == 0 || body_height == 0 {
            return Ok(());
        }

        if self.query.is_empty() {
            let placeholder =
                truncate_display_width("What should the agent do?", self.dialog.width);
            buffer.set_text(content_x, content_y, &placeholder, &self.muted_style);
        } else {
            let wrapped = self.wrapped_text();
            let cursor_row = wrapped
                .positions
                .get(self.cursor)
                .map_or(0, |position| position.0);
            let scroll = cursor_row.saturating_sub(body_height - 1);
            for (offset, row) in wrapped
                .rows
                .iter()
                .skip(scroll)
                .take(body_height)
                .enumerate()
            {
                buffer.set_text(content_x, content_y + offset, row, &self.style);
            }
        }

        if self.dialog.height > body_height {
            let status_y = content_y + body_height;
            let status = self.validation_status.unwrap_or(STATUS);
            let status = truncate_display_width(status, self.dialog.width);
            buffer.set_text(content_x, status_y, &status, &self.muted_style);
        }
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        match event {
            Event::Paste(text) => {
                self.insert(text);
                Self::redraw()
            }
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => Some(self.cancel_action()),
                (KeyCode::Char('c' | 'C'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    Some(self.cancel_action())
                }
                (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                    self.insert("\n");
                    Self::redraw()
                }
                (KeyCode::Enter, _) => {
                    if self.query.len() > MAX_PROMPT_BYTES {
                        self.validation_status = Some(OVERSIZED_STATUS);
                        return Self::redraw();
                    }
                    if self.query.trim().is_empty() {
                        self.validation_status = Some(EMPTY_STATUS);
                        return Self::redraw();
                    }
                    Some(self.submit_action())
                }
                (KeyCode::Char('j' | 'J'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.insert("\n");
                    Self::redraw()
                }
                (KeyCode::Char('p' | 'P'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.history_back();
                    Self::redraw()
                }
                (KeyCode::Char('n' | 'N'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.history_forward();
                    Self::redraw()
                }
                (KeyCode::Char('w' | 'W'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.delete_previous_word();
                    Self::redraw()
                }
                (KeyCode::Home, _) => {
                    self.cursor = 0;
                    self.preferred_column = None;
                    Self::redraw()
                }
                (KeyCode::Char('a' | 'A'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.cursor = 0;
                    self.preferred_column = None;
                    Self::redraw()
                }
                (KeyCode::End, _) => {
                    self.cursor = grapheme_len(&self.query);
                    self.preferred_column = None;
                    Self::redraw()
                }
                (KeyCode::Char('e' | 'E'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.cursor = grapheme_len(&self.query);
                    self.preferred_column = None;
                    Self::redraw()
                }
                (KeyCode::Left, _) => {
                    self.cursor = self.cursor.saturating_sub(1);
                    self.preferred_column = None;
                    Self::redraw()
                }
                (KeyCode::Right, _) => {
                    self.cursor = (self.cursor + 1).min(grapheme_len(&self.query));
                    self.preferred_column = None;
                    Self::redraw()
                }
                (KeyCode::Up, _) => {
                    self.move_vertically(-1);
                    Self::redraw()
                }
                (KeyCode::Down, _) => {
                    self.move_vertically(1);
                    Self::redraw()
                }
                (KeyCode::Backspace, _) => {
                    self.backspace();
                    Self::redraw()
                }
                (KeyCode::Delete, _) => {
                    self.delete();
                    Self::redraw()
                }
                (KeyCode::Tab, _) => {
                    self.insert("\t");
                    Self::redraw()
                }
                (KeyCode::Char(character), modifiers)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.insert(&character.to_string());
                    Self::redraw()
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        let (x, y, width, height) = Self::geometry(viewport_width, viewport_height);
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
        self.viewport_width = viewport_width;
        self.viewport_height = viewport_height;
        self.preferred_column = None;
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.popup.clone();
        self.muted_style = theme.ui_style.muted.clone();
        self.dialog.style = theme.ui_style.popup.clone();
        self.dialog.border_draw_style = theme.ui_style.popup_border.clone();
        self.dialog.title_style = theme.ui_style.popup_title.clone();
        self.dialog.theme = theme.clone();
        self.theme = theme.clone();
    }

    fn is_sensitive_input(&self) -> bool {
        true
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let wrapped = self.wrapped_text();
        let (row, column) = wrapped
            .positions
            .get(self.cursor)
            .copied()
            .unwrap_or_default();
        let body_height = self.body_height();
        let scroll = row.saturating_sub(body_height.saturating_sub(1));
        let x = self
            .dialog
            .x
            .saturating_add(1)
            .saturating_add(column)
            .min(self.viewport_width.saturating_sub(1));
        let y = self
            .dialog
            .y
            .saturating_add(1)
            .saturating_add(row.saturating_sub(scroll))
            .min(self.viewport_height.saturating_sub(1));
        Some((x, y))
    }
}

pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub(crate) fn wrap_text(text: &str, width: usize) -> WrappedText {
    let grapheme_count = grapheme_len(text);
    if width == 0 {
        return WrappedText {
            rows: Vec::new(),
            positions: vec![(0, 0); grapheme_count + 1],
        };
    }

    let mut rows = vec![String::new()];
    let mut positions = Vec::with_capacity(grapheme_count + 1);
    let mut row = 0;
    let mut column = 0;
    positions.push((row, column));

    for grapheme in text.graphemes(true) {
        if grapheme == "\n" {
            row += 1;
            column = 0;
            if rows.len() <= row {
                rows.push(String::new());
            }
            positions.push((row, column));
            continue;
        }

        if column == width {
            row += 1;
            column = 0;
            if rows.len() <= row {
                rows.push(String::new());
            }
        }

        let mut grapheme_width = if grapheme == "\t" {
            TAB_WIDTH - (column % TAB_WIDTH)
        } else {
            display_width(grapheme)
        };
        if grapheme_width > width.saturating_sub(column) && column > 0 {
            row += 1;
            column = 0;
            rows.push(String::new());
            grapheme_width = if grapheme == "\t" {
                TAB_WIDTH
            } else {
                display_width(grapheme)
            };
        }

        if grapheme_width > width {
            rows[row].push('?');
            column += 1;
        } else if grapheme == "\t" {
            rows[row].push_str(&" ".repeat(grapheme_width));
            column += grapheme_width;
        } else {
            rows[row].push_str(grapheme);
            column += grapheme_width;
        }

        if column == width {
            positions.push((row + 1, 0));
        } else {
            positions.push((row, column));
        }
    }

    if positions
        .last()
        .is_some_and(|position| position.0 >= rows.len())
    {
        rows.push(String::new());
    }

    WrappedText { rows, positions }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};

    fn editor(width: usize, height: usize) -> Editor {
        let config = Config::default();
        Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap()
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn submit(composer: &mut AgentComposer) -> Option<KeyAction> {
        composer.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE))
    }

    fn new_composer(
        editor: &Editor,
        title: Option<String>,
        id: i32,
        query: String,
        history: Vec<String>,
    ) -> AgentComposer {
        AgentComposer::new(editor, title, id, query, history, "agent".to_string())
    }

    fn rendered_row(buffer: &RenderBuffer, y: usize) -> String {
        buffer.cells[y * buffer.width..(y + 1) * buffer.width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[test]
    fn overflowing_prompt_wraps_and_keeps_cursor_inside_the_dialog() {
        let editor = editor(80, 24);
        let query = format!("prefix-{}-TAIL", "x".repeat(160));
        let composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            query,
            vec![],
        );
        let mut buffer = RenderBuffer::new(80, editor.vheight(), &Style::default());

        composer.draw(&mut buffer).unwrap();
        let rendered = (0..buffer.height)
            .map(|row| rendered_row(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        let (cursor_x, cursor_y) = composer.cursor_position().unwrap();

        assert!(rendered.contains("TAIL"));
        assert!(cursor_x < 80);
        assert!(cursor_y < editor.vheight());
        assert!(cursor_x < composer.dialog.x + composer.dialog.width + 1);
    }

    #[test]
    fn paste_preserves_all_lines_normalizes_crlf_and_renders_tabs_as_spaces() {
        let editor = editor(60, 18);
        let mut composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            String::new(),
            vec![],
        );
        composer.handle_event(&Event::Paste(
            "first\tline\r\n  second\rthird\n".to_string(),
        ));

        assert_eq!(composer.query, "first\tline\n  second\nthird\n");
        let wrapped = composer.wrapped_text();
        assert_eq!(wrapped.rows[0], "first   line");
        assert_eq!(wrapped.rows[1], "  second");
        assert_eq!(wrapped.rows[2], "third");
        assert_eq!(
            submit(&mut composer),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    "agent".to_string(),
                    "composer:submitted:802".to_string(),
                    json!("first\tline\n  second\nthird\n")
                )
            ]))
        );
    }

    #[test]
    fn navigation_and_deletion_edit_at_the_cursor_without_inserting_modifiers() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 7, "one two".to_string(), vec![]);

        composer.handle_event(&key(KeyCode::Left, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Left, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('X'), KeyModifiers::SHIFT));
        composer.handle_event(&key(KeyCode::Delete, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('q'), KeyModifiers::CONTROL));
        composer.handle_event(&key(KeyCode::Char('z'), KeyModifiers::ALT));
        assert_eq!(composer.query, "one tXo");

        composer.handle_event(&key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(composer.query, "one o");
        composer.handle_event(&key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        composer.handle_event(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(composer.query, "ne o");
        composer.handle_event(&key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.query, "ne ");
    }

    #[test]
    fn newline_shortcuts_and_vertical_motion_work_on_wrapped_lines() {
        let editor = editor(40, 14);
        let mut composer = new_composer(&editor, None, 1, "a".repeat(40), vec![]);
        let (_, original_row) = composer.cursor_position().unwrap();

        composer.handle_event(&key(KeyCode::Up, KeyModifiers::NONE));
        let (_, moved_row) = composer.cursor_position().unwrap();
        assert!(moved_row < original_row);
        composer.handle_event(&key(KeyCode::Down, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('j'), KeyModifiers::CONTROL));
        composer.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Enter, KeyModifiers::SHIFT));
        composer.handle_event(&key(KeyCode::Char('y'), KeyModifiers::NONE));

        assert!(composer.query.ends_with("\nx\ny"));
    }

    #[test]
    fn backspace_and_delete_remove_complete_unicode_graphemes() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 9, "e\u{301}👨‍👩‍👧漢".to_string(), vec![]);

        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.query, "e\u{301}👨‍👩‍👧");
        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.query, "e\u{301}");
        composer.handle_event(&key(KeyCode::Home, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert!(composer.query.is_empty());
    }

    #[test]
    fn history_navigation_preserves_the_original_draft() {
        let editor = editor(60, 18);
        let mut composer = new_composer(
            &editor,
            None,
            802,
            "current draft".to_string(),
            vec!["newer\r\nprompt".to_string(), "older".to_string()],
        );

        composer.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.query, "newer\nprompt");
        composer.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.query, "older");
        composer.handle_event(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.query, "newer\nprompt");
        composer.handle_event(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.query, "current draft");
        assert!(composer.is_sensitive_input());
        assert_eq!(composer.picker_id(), None);
    }

    #[test]
    fn empty_submit_stays_open_and_cancel_notifies_plugins() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, " \n\t".to_string(), vec![]);

        assert_eq!(
            submit(&mut composer),
            Some(KeyAction::Single(Action::ShowDialog))
        );
        let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());
        composer.draw(&mut buffer).unwrap();
        let status_y = composer.dialog.y + 1 + composer.body_height();
        assert!(rendered_row(&buffer, status_y).contains(EMPTY_STATUS));
        assert_eq!(
            composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    "agent".to_string(),
                    "composer:cancelled:802".to_string(),
                    json!(null)
                )
            ]))
        );
        assert_eq!(
            composer.handle_event(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    "agent".to_string(),
                    "composer:cancelled:802".to_string(),
                    json!(null)
                )
            ]))
        );
    }

    #[test]
    fn callback_composer_delivers_terminal_results_before_closing() {
        let editor = editor(60, 18);
        let handle = ComposerHandle::from_raw(42);
        let mut submitted = AgentComposer::new_callback(
            &editor,
            Some("Prompt".to_string()),
            "exact text".to_string(),
            vec![],
            handle,
        );

        assert_eq!(submitted.composer_handle(), Some(handle));
        assert_eq!(
            submit(&mut submitted),
            Some(KeyAction::Multiple(vec![
                Action::NotifyComposer(
                    handle,
                    Box::new(ComposerCallback::Submitted("exact text".to_string()))
                ),
                Action::CloseDialog,
            ]))
        );

        let mut cancelled = AgentComposer::new_callback(
            &editor,
            Some("Prompt".to_string()),
            String::new(),
            vec![],
            handle,
        );
        assert_eq!(
            cancelled.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::NotifyComposer(handle, Box::new(ComposerCallback::Cancelled)),
                Action::CloseDialog,
            ]))
        );
    }

    #[test]
    fn resize_to_narrow_and_tiny_viewports_keeps_borders_and_cursor_in_bounds() {
        let editor = editor(80, 24);
        let mut composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            format!("{}漢", "x".repeat(120)),
            vec![],
        );

        for (width, height) in [(48, 14), (8, 4), (2, 2), (1, 1)] {
            composer.resize(width, height);
            let mut buffer = RenderBuffer::new(width, height, &Style::default());
            composer.draw(&mut buffer).unwrap();
            let (cursor_x, cursor_y) = composer.cursor_position().unwrap();
            assert!(cursor_x < width);
            assert!(cursor_y < height);
            if width >= 2 && height >= 2 {
                let left = composer.dialog.x;
                let right = composer.dialog.x + composer.dialog.width + 1;
                let top = composer.dialog.y;
                let bottom = composer.dialog.y + composer.dialog.height + 1;
                assert_eq!(buffer.cells[top * width + left].c, '┌');
                assert_eq!(buffer.cells[top * width + right].c, '┐');
                assert_eq!(buffer.cells[bottom * width + left].c, '└');
                assert_eq!(buffer.cells[bottom * width + right].c, '┘');
            }
        }
    }

    #[test]
    fn compact_status_keeps_every_shortcut_visible_at_minimum_width() {
        let editor = editor(36, 14);
        let composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            String::new(),
            vec![],
        );
        let mut buffer = RenderBuffer::new(36, editor.vheight(), &Style::default());

        composer.draw(&mut buffer).unwrap();
        let status_y = composer.dialog.y + 1 + composer.body_height();

        assert!(rendered_row(&buffer, status_y).contains(STATUS));
    }

    #[test]
    fn control_shortcuts_accept_shift_and_alt_modifiers_without_leaking_text() {
        let editor = editor(60, 18);
        let mut composer = new_composer(
            &editor,
            None,
            802,
            "draft".to_string(),
            vec!["recent".to_string()],
        );

        composer.handle_event(&key(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(composer.query, "recent");
        composer.handle_event(&key(
            KeyCode::Char('N'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(composer.query, "draft");
        composer.handle_event(&key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(composer.query, "draft\n");
    }

    #[test]
    fn oversized_ascii_paste_and_insert_leave_the_existing_draft_unchanged() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "draft".to_string(), vec![]);
        let oversized = "x".repeat(MAX_PROMPT_BYTES);

        composer.handle_event(&Event::Paste(oversized));
        assert_eq!(composer.query, "draft");
        assert_eq!(composer.validation_status, Some(OVERSIZED_STATUS));
        let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());
        composer.draw(&mut buffer).unwrap();
        let status_y = composer.dialog.y + 1 + composer.body_height();
        assert!(rendered_row(&buffer, status_y).contains(OVERSIZED_STATUS));

        composer.query = "x".repeat(MAX_PROMPT_BYTES);
        composer.cursor = MAX_PROMPT_BYTES;
        composer.handle_event(&key(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(composer.query.len(), MAX_PROMPT_BYTES);
        assert_eq!(composer.validation_status, Some(OVERSIZED_STATUS));
    }

    #[test]
    fn maximum_escaping_heavy_prompt_fits_the_app_server_frame_and_submits_exactly() {
        let editor = editor(60, 18);
        let accepted = "\u{0}".repeat(MAX_PROMPT_BYTES);
        let mut composer = new_composer(&editor, None, 802, accepted.clone(), vec![]);
        let encoded = serde_json::to_vec(&accepted).unwrap();

        assert!(encoded.len() < 1024 * 1024);
        assert_eq!(composer.query, accepted);
        assert_eq!(
            submit(&mut composer),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    "agent".to_string(),
                    "composer:submitted:802".to_string(),
                    json!(accepted)
                )
            ]))
        );
    }

    #[test]
    fn oversized_initial_and_history_entries_are_rejected_before_navigation_or_wrapping() {
        let editor = editor(60, 18);
        let oversized = "x".repeat(MAX_PROMPT_BYTES + 1);
        let mut composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            oversized.clone(),
            vec![oversized, "safe history".to_string()],
        );

        assert!(composer.query.is_empty());
        assert_eq!(composer.cursor, 0);
        assert_eq!(composer.history, vec!["safe history".to_string()]);
        assert_eq!(composer.validation_status, Some(OVERSIZED_STATUS));
        let wrapped = composer.wrapped_text();
        assert_eq!(wrapped.rows, vec![String::new()]);
        composer.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.query, "safe history");
        assert_eq!(composer.validation_status, None);
    }
}
