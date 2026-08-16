//! A compact, multiline prompt composer for agent requests.

use crossterm::event::Event;
use serde_json::json;

use crate::{
    config::KeyAction,
    editor::{Action, ComposerCallback, Editor, Mode, RenderBuffer},
    plugin::ComposerHandle,
    text_layout::{LayoutOptions, TextLayout},
    theme::{Style, Theme},
    unicode_utils::truncate_display_width,
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    normalize_prompt_newlines, ActionBar, ActionMode, ActionPriority, Component, PromptBuffer,
    PromptInput, PromptKeyPolicy, UiAction, PROMPT_MAX_BYTES,
};

const MAX_PROMPT_BYTES: usize = PROMPT_MAX_BYTES;
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
    prompt: PromptBuffer,
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
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight();
        let (x, y, width, height) = Self::geometry(viewport_width, viewport_height);
        let initial_too_large = query.len() > MAX_PROMPT_BYTES;
        let query = if initial_too_large {
            String::new()
        } else {
            normalize_prompt_newlines(&query)
        };
        let history_len = history.len();
        let history = history
            .into_iter()
            .filter(|entry| entry.len() <= MAX_PROMPT_BYTES)
            .collect::<Vec<_>>();
        let prompt = PromptBuffer::with_history(&query, history)
            .with_key_policy(PromptKeyPolicy::EnterSends);
        let history_too_large = prompt.history().len() != history_len;

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
            .with_surface_theme(&theme, SurfaceRole::Popup),
            prompt,
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
                    json!(self.prompt.text()),
                ),
            ]),
            ComposerTarget::Callback(handle) => KeyAction::Multiple(vec![
                Action::NotifyComposer(
                    *handle,
                    Box::new(ComposerCallback::Submitted(self.prompt.text())),
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

    fn wrapped_text(&self) -> TextLayout {
        self.prompt.layout(LayoutOptions::word(self.dialog.width))
    }

    fn redraw() -> Option<KeyAction> {
        Some(KeyAction::Single(Action::Refresh))
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
        let body_height = self.body_height();
        let content_x = self.dialog.x + 1;
        let content_y = self.dialog.y + 1;
        if self.dialog.width == 0 || body_height == 0 {
            return Ok(());
        }

        if self.prompt.text().is_empty() {
            let placeholder =
                truncate_display_width("What should the agent do?", self.dialog.width);
            buffer.set_text(content_x, content_y, &placeholder, &self.muted_style);
        } else {
            let wrapped = self.wrapped_text();
            let cursor_row = wrapped
                .position(self.prompt.cursor())
                .map_or(0, |position| position.row);
            let scroll = cursor_row.saturating_sub(body_height - 1);
            for (offset, row) in wrapped
                .rows()
                .iter()
                .skip(scroll)
                .take(body_height)
                .enumerate()
            {
                buffer.set_text(content_x, content_y + offset, &row.text, &self.style);
            }
        }

        if self.dialog.height > body_height {
            let status_y = content_y + body_height;
            let prompt_mode = self.prompt.mode();
            let normal_mode = prompt_mode == Mode::Normal;
            let escape_label = if normal_mode { "Cancel" } else { "Normal" };
            let actions = [
                UiAction::new("send", "Enter", "Send")
                    .with_modes([ActionMode::Insert, ActionMode::Normal])
                    .with_compact_key("↵")
                    .with_compact_label("")
                    .with_priority(ActionPriority::Essential),
                UiAction::new("cancel", "Esc", escape_label)
                    .with_compact_label("")
                    .with_priority(ActionPriority::Essential),
                UiAction::new("newline", "Ctrl+J", "New line").with_modes([ActionMode::Insert]),
                UiAction::new("history", "Ctrl+P/N", "History")
                    .with_compact_key("^P/N")
                    .with_priority(ActionPriority::Secondary),
            ];
            let visible_actions = if self.validation_status.is_some() {
                &actions[..2]
            } else {
                &actions[..]
            };
            ActionBar::new(visible_actions)
                .with_mode(match prompt_mode {
                    Mode::Normal => ActionMode::Normal,
                    Mode::Visual | Mode::VisualLine | Mode::VisualBlock => ActionMode::Visual,
                    Mode::Search => ActionMode::Read,
                    Mode::Insert | Mode::Command => ActionMode::Insert,
                })
                .with_status(self.validation_status)
                .render(
                    buffer,
                    content_x,
                    status_y,
                    self.dialog.width,
                    &self.theme,
                    &self.style,
                );
        }
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let previous_bytes = self.prompt.text().len();
        let inserted_bytes = match event {
            Event::Paste(text) => normalize_prompt_newlines(text).len(),
            Event::Key(key) => match key.code {
                crossterm::event::KeyCode::Char(character)
                    if !key.modifiers.intersects(
                        crossterm::event::KeyModifiers::CONTROL
                            | crossterm::event::KeyModifiers::ALT,
                    ) =>
                {
                    character.len_utf8()
                }
                _ => 0,
            },
            _ => 0,
        };

        match self
            .prompt
            .handle_event_with_layout_options(event, LayoutOptions::word(self.dialog.width))
        {
            PromptInput::Changed => {
                self.validation_status = None;
                Self::redraw()
            }
            PromptInput::Submit => {
                let text = self.prompt.text();
                if text.len() > MAX_PROMPT_BYTES {
                    self.validation_status = Some(OVERSIZED_STATUS);
                    Self::redraw()
                } else if text.trim().is_empty() {
                    self.validation_status = Some(EMPTY_STATUS);
                    Self::redraw()
                } else {
                    Some(self.submit_action())
                }
            }
            PromptInput::Cancel => Some(self.cancel_action()),
            PromptInput::Unhandled if inserted_bytes > MAX_PROMPT_BYTES - previous_bytes => {
                self.validation_status = Some(OVERSIZED_STATUS);
                Self::redraw()
            }
            PromptInput::Unhandled => None,
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
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.popup.clone();
        self.muted_style = theme.ui_style.muted.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Popup);
        self.theme = theme.clone();
    }

    fn is_sensitive_input(&self) -> bool {
        true
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let wrapped = self.wrapped_text();
        let position = wrapped.position(self.prompt.cursor()).unwrap_or_default();
        let row = position.row;
        let column = position.column;
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

    fn cursor_mode(&self) -> Option<Mode> {
        Some(self.prompt.mode())
    }
}

pub(crate) fn wrap_text(text: &str, width: usize) -> WrappedText {
    let (rows, positions) = TextLayout::new(text, LayoutOptions::grapheme(width)).into_parts();
    WrappedText {
        rows: rows.into_iter().map(|row| row.text).collect(),
        positions: positions
            .into_iter()
            .map(|position| (position.row, position.column))
            .collect(),
    }
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
        composer.handle_event(&key(KeyCode::Enter, KeyModifiers::CONTROL))
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
    fn word_wrapping_moves_and_renders_the_cursor_without_rewriting_the_draft() {
        let editor = editor(40, 18);
        let original = "one two three";
        let mut composer = new_composer(&editor, None, 17, original.to_string(), vec![]);
        composer.dialog.width = 7;
        composer.prompt.set_cursor(0);
        let revision = composer.prompt.buffer().revision();
        let mut buffer = RenderBuffer::new(40, editor.vheight(), &Style::default());
        composer.draw(&mut buffer).unwrap();
        assert!(rendered_row(&buffer, composer.dialog.y + 1).contains("one two"));
        assert!(rendered_row(&buffer, composer.dialog.y + 2).contains("three"));

        composer.handle_event(&key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(composer.prompt.cursor(), 8);
        assert_eq!(
            composer.cursor_position(),
            Some((composer.dialog.x + 1, composer.dialog.y + 2))
        );
        composer.resize(30, 12);
        assert_eq!(composer.prompt.cursor(), 8);
        assert_eq!(composer.prompt.text(), original);
        assert_eq!(composer.prompt.buffer().revision(), revision);
        assert_eq!(
            submit(&mut composer),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    "agent".to_string(),
                    "composer:submitted:17".to_string(),
                    json!(original)
                ),
            ]))
        );

        // Confirmations and inline assist still use the legacy projection.
        assert_eq!(wrap_text(original, 7).rows, ["one two", " three"]);
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

        assert_eq!(composer.prompt.text(), "first\tline\n  second\nthird\n");
        let wrapped = composer.wrapped_text();
        assert_eq!(wrapped.rows()[0].text, "first   line");
        assert_eq!(wrapped.rows()[1].text, "  second");
        assert_eq!(wrapped.rows()[2].text, "third");
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
        assert_eq!(composer.prompt.text(), "one tXo");

        composer.handle_event(&key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(composer.prompt.text(), "one o");
        composer.handle_event(&key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        composer.handle_event(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(composer.prompt.text(), "ne o");
        composer.handle_event(&key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.prompt.text(), "ne ");
    }

    #[test]
    fn normal_mode_arrow_motions_keep_agent_composer_lines_intact() {
        let editor = editor(60, 18);
        let cases = [
            (
                "Left at second line start",
                "one\ntwo",
                4,
                KeyCode::Left,
                4,
                "one\nwo",
            ),
            (
                "Right at first line end",
                "one\ntwo",
                2,
                KeyCode::Right,
                2,
                "on\ntwo",
            ),
        ];

        for (name, text, cursor, arrow, expected_cursor, expected_text) in cases {
            let mut composer = new_composer(&editor, None, 7, text.to_string(), vec![]);
            composer.prompt.set_cursor(cursor);
            composer.prompt.set_mode(Mode::Normal);

            assert_eq!(
                composer.handle_event(&key(arrow, KeyModifiers::NONE)),
                Some(KeyAction::Single(Action::Refresh)),
                "{name}: move with arrow key"
            );
            assert_eq!(
                composer.prompt.cursor(),
                expected_cursor,
                "{name}: stay on line"
            );

            assert_eq!(
                composer.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
                Some(KeyAction::Single(Action::Refresh)),
                "{name}: delete selected grapheme"
            );
            assert_eq!(
                composer.prompt.text(),
                expected_text,
                "{name}: preserve lines"
            );
        }
    }

    #[test]
    fn combining_mark_insertion_keeps_agent_composer_cursor_after_merged_grapheme() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 7, "aX".to_string(), vec![]);

        composer.handle_event(&key(KeyCode::Left, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('\u{301}'), KeyModifiers::NONE));

        assert_eq!(composer.prompt.text(), "a\u{301}X");
        assert_eq!(composer.prompt.cursor(), 1);

        composer.handle_event(&key(KeyCode::Char('Z'), KeyModifiers::NONE));

        assert_eq!(composer.prompt.text(), "a\u{301}ZX");
        assert_eq!(composer.prompt.cursor(), 2);
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

        assert!(composer.prompt.text().ends_with("\nx\ny"));
    }

    #[test]
    fn backspace_and_delete_remove_complete_unicode_graphemes() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 9, "e\u{301}👨‍👩‍👧漢".to_string(), vec![]);

        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.prompt.text(), "e\u{301}👨‍👩‍👧");
        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.prompt.text(), "e\u{301}");
        composer.handle_event(&key(KeyCode::Home, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert!(composer.prompt.text().is_empty());
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
        assert_eq!(composer.prompt.text(), "newer\nprompt");
        composer.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.prompt.text(), "older");
        composer.handle_event(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.prompt.text(), "newer\nprompt");
        composer.handle_event(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.prompt.text(), "current draft");
        assert!(composer.is_sensitive_input());
        assert_eq!(composer.picker_id(), None);
    }

    #[test]
    fn empty_submit_stays_open_and_cancel_notifies_plugins() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, " \n\t".to_string(), vec![]);

        assert_eq!(
            submit(&mut composer),
            Some(KeyAction::Single(Action::Refresh))
        );
        let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());
        composer.draw(&mut buffer).unwrap();
        let status_y = composer.dialog.y + 1 + composer.body_height();
        assert!(rendered_row(&buffer, status_y).contains(EMPTY_STATUS));
        assert_eq!(
            composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(composer.prompt.mode(), Mode::Normal);
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
            Some(KeyAction::Single(Action::Refresh))
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
    fn modified_enter_edits_the_real_prompt_buffer_in_insert_mode() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "first".to_string(), vec![]);

        assert_eq!(
            composer.handle_event(&key(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(composer.prompt.text(), "first\n");
        assert_eq!(composer.prompt.buffer().contents(), "first\n");
        assert!(composer.prompt.buffer().file.is_none());
        assert_eq!(composer.prompt.buffer().undo_history.node_count(), 1);
    }

    #[test]
    fn composer_action_hints_follow_prompt_mode() {
        let editor = editor(160, 24);
        let mut composer = new_composer(&editor, None, 802, "send this".to_string(), vec![]);
        let mut buffer = RenderBuffer::new(160, editor.vheight(), &Style::default());
        let status_y = composer.dialog.y + 1 + composer.body_height();

        composer.draw(&mut buffer).unwrap();
        let insert_status = rendered_row(&buffer, status_y);
        assert!(insert_status.contains("INSERT"), "{insert_status:?}");
        assert!(insert_status.contains("Enter Send"), "{insert_status:?}");
        assert!(
            insert_status.contains("Ctrl+J New line"),
            "{insert_status:?}"
        );
        assert!(insert_status.contains("Esc Normal"), "{insert_status:?}");

        assert_eq!(
            composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::Refresh))
        );
        composer.draw(&mut buffer).unwrap();
        let normal_status = rendered_row(&buffer, status_y);
        assert!(normal_status.contains("NORMAL"), "{normal_status:?}");
        assert!(normal_status.contains("Enter Send"), "{normal_status:?}");
        assert!(normal_status.contains("Esc Cancel"), "{normal_status:?}");
        assert!(!normal_status.contains("Ctrl+Enter"), "{normal_status:?}");
        assert!(!normal_status.contains("New line"), "{normal_status:?}");

        assert_eq!(
            composer.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    "agent".to_string(),
                    "composer:submitted:802".to_string(),
                    json!("send this"),
                ),
            ]))
        );
    }

    #[test]
    fn enter_and_control_enter_submit_scoped_callbacks() {
        let editor = editor(60, 18);
        let handle = ComposerHandle::from_raw(42);

        for event in [
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Enter, KeyModifiers::CONTROL),
        ] {
            let mut composer = AgentComposer::new_callback(
                &editor,
                Some("Prompt".to_string()),
                "send this exactly".to_string(),
                vec![],
                handle,
            );

            assert_eq!(
                composer.handle_event(&event),
                Some(KeyAction::Multiple(vec![
                    Action::NotifyComposer(
                        handle,
                        Box::new(ComposerCallback::Submitted("send this exactly".to_string()))
                    ),
                    Action::CloseDialog,
                ]))
            );
        }
    }

    #[test]
    fn escape_enters_real_normal_mode_and_vim_undo_restores_word_deletion() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "first second".to_string(), vec![]);

        composer.handle_event(&key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(
            composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(composer.prompt.mode(), Mode::Normal);
        composer.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(composer.prompt.text(), "second");
        composer.handle_event(&key(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(composer.prompt.text(), "first second");
        assert_eq!(
            composer.handle_event(&key(KeyCode::Char('i'), KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(composer.prompt.mode(), Mode::Insert);
    }

    #[test]
    fn control_s_neither_submits_nor_mutates_the_floating_prompt() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "keep editing".to_string(), vec![]);

        assert_eq!(
            composer.handle_event(&key(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(composer.prompt.text(), "keep editing");
        assert_eq!(composer.prompt.mode(), Mode::Insert);
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
    fn compact_status_preserves_complete_submission_and_cancel_actions() {
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

        let status = rendered_row(&buffer, status_y);
        assert!(status.contains("Send"), "{status:?}");
        assert!(status.contains("Esc"), "{status:?}");
    }

    #[test]
    fn compact_normal_mode_status_preserves_submission_and_cancel_actions() {
        let editor = editor(36, 14);
        let mut composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            "send this".to_string(),
            vec![],
        );
        let mut buffer = RenderBuffer::new(36, editor.vheight(), &Style::default());

        assert_eq!(
            composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::Refresh))
        );
        composer.draw(&mut buffer).unwrap();
        let status_y = composer.dialog.y + 1 + composer.body_height();
        let status = rendered_row(&buffer, status_y);

        assert!(status.contains("Send"), "{status:?}");
        assert!(status.contains("Esc"), "{status:?}");
        assert!(!status.contains("New line"), "{status:?}");
        assert!(!status.contains("^↵"), "{status:?}");
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
        assert_eq!(composer.prompt.text(), "recent");
        composer.handle_event(&key(
            KeyCode::Char('N'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(composer.prompt.text(), "draft");
        composer.handle_event(&key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(composer.prompt.text(), "draft\n");
    }

    #[test]
    fn oversized_ascii_paste_and_insert_leave_the_existing_draft_unchanged() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "draft".to_string(), vec![]);
        let oversized = "x".repeat(MAX_PROMPT_BYTES);

        composer.handle_event(&Event::Paste(oversized));
        assert_eq!(composer.prompt.text(), "draft");
        assert_eq!(composer.validation_status, Some(OVERSIZED_STATUS));
        let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());
        composer.draw(&mut buffer).unwrap();
        let status_y = composer.dialog.y + 1 + composer.body_height();
        assert!(rendered_row(&buffer, status_y).contains(OVERSIZED_STATUS));

        assert!(composer.prompt.set_text(&"x".repeat(MAX_PROMPT_BYTES)));
        composer.handle_event(&key(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(composer.prompt.text().len(), MAX_PROMPT_BYTES);
        assert_eq!(composer.validation_status, Some(OVERSIZED_STATUS));
    }

    #[test]
    fn maximum_escaping_heavy_prompt_fits_the_app_server_frame_and_submits_exactly() {
        let editor = editor(60, 18);
        let accepted = "\u{0}".repeat(MAX_PROMPT_BYTES);
        let mut composer = new_composer(&editor, None, 802, accepted.clone(), vec![]);
        let encoded = serde_json::to_vec(&accepted).unwrap();

        assert!(encoded.len() < 1024 * 1024);
        assert_eq!(composer.prompt.text(), accepted);
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

        assert!(composer.prompt.text().is_empty());
        assert_eq!(composer.prompt.cursor(), 0);
        assert_eq!(composer.prompt.history(), ["safe history".to_string()]);
        assert_eq!(composer.validation_status, Some(OVERSIZED_STATUS));
        let wrapped = composer.wrapped_text();
        assert_eq!(wrapped.rows().len(), 1);
        assert!(wrapped.rows()[0].text.is_empty());
        composer.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.prompt.text(), "safe history");
        assert_eq!(composer.validation_status, None);
    }
}
