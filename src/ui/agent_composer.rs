//! A compact, multiline prompt composer for agent requests.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde_json::json;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    config::KeyAction,
    editor::{Action, ComposerCallback, Editor, Mode, RenderBuffer},
    plugin::ComposerHandle,
    theme::{Style, Theme},
    unicode_utils::{display_width, grapheme_len, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog},
    Component, ModalComposer, ModalComposerMode, ModalComposerOutcome,
};

const TAB_WIDTH: usize = 4;
const INSERT_HINTS: &str = " Ctrl+Enter send; Esc normal; Enter line";
const NORMAL_HINTS: &str = " Ctrl+Enter send; i edit; Enter send";
const VISUAL_HINTS: &str = " Ctrl+Enter send; Esc normal; hjkl select";
#[cfg(test)]
const STATUS: &str = "INSERT Ctrl+Enter send; Esc normal";
#[cfg(test)]
const EMPTY_STATUS: &str = "Prompt is empty";
#[cfg(test)]
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
    title: Option<String>,
    composer: ModalComposer,
    ascii_borders: bool,
    viewport_width: usize,
    viewport_height: usize,
    style: Style,
    muted_style: Style,
    footer_style: Style,
    theme: Theme,
}

#[derive(Debug)]
enum ComposerTarget {
    Legacy { owner: String, id: i32 },
    Callback(ComposerHandle),
}

impl AgentComposer {
    /// Creates a centered, Vim-capable composer with the cursor at the end of `query`.
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
        let style = theme.ui_style.popup.with_bg(theme.style.bg);
        let border_style = theme.ui_style.popup_border.with_bg(theme.style.bg);
        let title_style = theme.ui_style.popup_title.with_bg(theme.style.bg);
        let popup_title = title.clone();
        let ascii_borders = editor.window_borders_ascii();
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight();
        let (x, y, width, height) = Self::geometry(viewport_width, viewport_height);
        let composer = ModalComposer::new(&query, history);

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
            title: popup_title,
            composer,
            ascii_borders,
            viewport_width,
            viewport_height,
            style,
            muted_style: theme.ui_style.muted.with_bg(theme.style.bg),
            footer_style: theme.ui_style.muted.clone(),
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
                    json!(self.composer.contents()),
                ),
            ]),
            ComposerTarget::Callback(handle) => KeyAction::Multiple(vec![
                Action::NotifyComposer(
                    *handle,
                    Box::new(ComposerCallback::Submitted(self.composer.contents())),
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
        let x = viewport_width.saturating_sub(outer_width) / 2;
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
        wrap_text(&self.composer.contents(), self.dialog.width)
    }

    fn draw_border(&self, buffer: &mut RenderBuffer) {
        if buffer.width == 0 || buffer.height == 0 {
            return;
        }

        let x = self.dialog.x;
        let y = self.dialog.y;
        let width = self.dialog.width.saturating_add(2);
        let height = self.dialog.height.saturating_add(2);
        let right = x.saturating_add(width.saturating_sub(1));
        let bottom = y.saturating_add(height.saturating_sub(1));
        let (horizontal, vertical, corners) = if self.ascii_borders {
            ('-', '|', ['+', '+', '+', '+'])
        } else {
            ('─', '│', ['┌', '┐', '└', '┘'])
        };
        let style = &self.dialog.border_draw_style;

        buffer.fill_rect(x, y, width, 1, horizontal, style, &self.theme);
        buffer.fill_rect(x, bottom, width, 1, horizontal, style, &self.theme);
        buffer.fill_rect(x, y, 1, height, vertical, style, &self.theme);
        buffer.fill_rect(right, y, 1, height, vertical, style, &self.theme);
        buffer.set_char(x, y, corners[0], style, &self.theme);
        buffer.set_char(right, y, corners[1], style, &self.theme);
        buffer.set_char(x, bottom, corners[2], style, &self.theme);
        buffer.set_char(right, bottom, corners[3], style, &self.theme);

        if let Some(title) = &self.title {
            let available = width.saturating_sub(2);
            if available > 0 {
                let title = format!(" {title} ");
                let title = truncate_display_width(&title, available);
                let offset = available.saturating_sub(display_width(&title)) / 2;
                let title_x = x.saturating_add(1).saturating_add(offset);
                buffer.set_text(title_x, y, &title, &self.dialog.title_style);
            }
        }
    }

    fn composer_action(&self, outcome: ModalComposerOutcome) -> Option<KeyAction> {
        match outcome {
            ModalComposerOutcome::Submit => Some(self.submit_action()),
            ModalComposerOutcome::Changed | ModalComposerOutcome::Rejected => Self::redraw(),
            ModalComposerOutcome::Unhandled => None,
        }
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
        self.draw_border(buffer);
        let body_height = self.body_height();
        let content_x = self.dialog.x + 1;
        let content_y = self.dialog.y + 1;
        if self.dialog.width == 0 || body_height == 0 {
            return Ok(());
        }

        if self.composer.contents().is_empty() {
            let placeholder =
                truncate_display_width("What should the agent do?", self.dialog.width);
            buffer.set_text(content_x, content_y, &placeholder, &self.muted_style);
        } else {
            let wrapped = self.wrapped_text();
            let cursor_row = wrapped
                .positions
                .get(self.composer.cursor_grapheme_index())
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
            let mode = self.composer.mode();
            let shortcuts = match mode {
                ModalComposerMode::Insert => INSERT_HINTS,
                ModalComposerMode::Normal => NORMAL_HINTS,
                ModalComposerMode::Visual => VISUAL_HINTS,
            };
            let mode_status = format!("{}{shortcuts}", mode.label());
            let status = self
                .composer
                .validation_status()
                .unwrap_or(mode_status.as_str());
            let status = truncate_display_width(status, self.dialog.width);
            buffer.set_text(
                content_x,
                status_y,
                &" ".repeat(self.dialog.width),
                &self.footer_style,
            );
            buffer.set_text(content_x, status_y, &status, &self.footer_style);
        }
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        match event {
            Event::Paste(text) => {
                let outcome = self.composer.handle_paste(text);
                self.composer_action(outcome)
            }
            Event::Key(key)
                if matches!(key.code, KeyCode::Char('c' | 'C'))
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                Some(self.cancel_action())
            }
            Event::Key(key) => {
                let outcome = self.composer.handle_key(*key);
                self.composer_action(outcome)
            }
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
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.popup.with_bg(theme.style.bg);
        self.muted_style = theme.ui_style.muted.with_bg(theme.style.bg);
        self.footer_style = theme.ui_style.muted.clone();
        self.dialog.style = self.style.clone();
        self.dialog.border_draw_style = theme.ui_style.popup_border.with_bg(theme.style.bg);
        self.dialog.title_style = theme.ui_style.popup_title.with_bg(theme.style.bg);
        self.dialog.theme = theme.clone();
        self.theme = theme.clone();
    }

    fn is_sensitive_input(&self) -> bool {
        true
    }

    fn cursor_mode(&self) -> Option<Mode> {
        Some(match self.composer.mode() {
            ModalComposerMode::Insert => Mode::Insert,
            ModalComposerMode::Normal => Mode::Normal,
            ModalComposerMode::Visual => Mode::Visual,
        })
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        if self.viewport_width == 0 || self.viewport_height == 0 {
            return None;
        }
        let wrapped = self.wrapped_text();
        let (row, column) = wrapped
            .positions
            .get(self.composer.cursor_grapheme_index())
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
    use crate::{
        buffer::Buffer, config::Config, lsp::LspManager, theme::parse_vscode_theme,
        ui::modal_composer::MAX_PROMPT_BYTES,
    };

    fn editor(width: usize, height: usize) -> Editor {
        editor_with_config(width, height, Config::default())
    }

    fn editor_with_config(width: usize, height: usize, config: Config) -> Editor {
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
    fn floating_composer_keeps_the_editor_background_and_one_accented_footer() {
        let editor = editor(80, 24);

        for theme_path in ["themes/kanso.json", "themes/github-light.json"] {
            let theme = parse_vscode_theme(theme_path).unwrap();
            assert_ne!(
                theme.ui_style.muted.bg, theme.style.bg,
                "{theme_path} should exercise a visibly accented footer",
            );

            for draft in ["", "Explain the current file"] {
                let mut composer = new_composer(
                    &editor,
                    Some("Agent prompt".to_string()),
                    802,
                    draft.to_string(),
                    vec![],
                );
                composer.set_theme(&theme);

                let mut buffer = RenderBuffer::new(80, editor.vheight(), &theme.style);
                composer.draw(&mut buffer).unwrap();

                let left = composer.dialog.x;
                let right = left + composer.dialog.width + 1;
                let top = composer.dialog.y;
                let bottom = top + composer.dialog.height + 1;
                let footer_y = top + 1 + composer.body_height();

                for y in top..=bottom {
                    for x in left..=right {
                        let footer_content = y == footer_y && x > left && x < right;
                        let expected_background = if footer_content {
                            theme.ui_style.muted.bg
                        } else {
                            theme.style.bg
                        };
                        assert_eq!(
                            buffer.cells[y * buffer.width + x].style.bg,
                            expected_background,
                            "{theme_path}, draft={draft:?}, x={x}, y={y}",
                        );
                    }
                }

                assert_eq!(
                    buffer.cells[top * buffer.width + left].style.fg,
                    theme.ui_style.popup_border.fg,
                    "floating outlines should retain their theme accent",
                );

                let body = &buffer.cells[(top + 1) * buffer.width + left + 1];
                let expected_foreground = if draft.is_empty() {
                    theme.ui_style.muted.fg
                } else {
                    theme.ui_style.popup.fg
                };
                assert_eq!(body.style.fg, expected_foreground);
                assert!(rendered_row(&buffer, footer_y).contains("Ctrl+Enter send"));
            }
        }
    }

    #[test]
    fn floating_composer_reports_its_vim_cursor_mode_and_wrapped_position() {
        let editor = editor(16, 12);
        let mut composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            "wide 漢 👨‍👩‍👧\nsecond e\u{301} line".to_string(),
            vec![],
        );

        let assert_cursor = |composer: &AgentComposer, expected_mode: Mode| {
            assert_eq!(Component::cursor_mode(composer), Some(expected_mode));

            let wrapped = composer.wrapped_text();
            let (row, column) = wrapped.positions[composer.composer.cursor_grapheme_index()];
            let scroll = row.saturating_sub(composer.body_height().saturating_sub(1));

            assert_eq!(
                composer.cursor_position(),
                Some((
                    composer.dialog.x + 1 + column,
                    composer.dialog.y + 1 + row.saturating_sub(scroll),
                )),
            );
        };

        assert_cursor(&composer, Mode::Insert);

        composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert_cursor(&composer, Mode::Normal);

        composer.handle_event(&key(KeyCode::Char('v'), KeyModifiers::NONE));
        assert_cursor(&composer, Mode::Visual);

        composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert_cursor(&composer, Mode::Normal);

        composer.handle_event(&key(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_cursor(&composer, Mode::Insert);
    }

    #[test]
    fn modified_enter_submits_the_complete_floating_prompt_in_every_vim_mode() {
        let editor = editor(60, 18);
        let modifiers = [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        ];

        for modifiers in modifiers {
            for mode in [
                ModalComposerMode::Insert,
                ModalComposerMode::Normal,
                ModalComposerMode::Visual,
            ] {
                let mut composer = new_composer(
                    &editor,
                    Some("Agent prompt".to_string()),
                    802,
                    "first\n漢👨‍👩‍👧".to_string(),
                    vec![],
                );

                if mode != ModalComposerMode::Insert {
                    composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE));
                    if mode == ModalComposerMode::Visual {
                        composer.handle_event(&key(KeyCode::Char('v'), KeyModifiers::NONE));
                    }
                }

                assert_eq!(composer.composer.mode(), mode);
                assert_eq!(
                    composer.handle_event(&key(KeyCode::Enter, modifiers)),
                    Some(KeyAction::Multiple(vec![
                        Action::CloseDialog,
                        Action::NotifyPlugin(
                            "agent".to_string(),
                            "composer:submitted:802".to_string(),
                            json!("first\n漢👨‍👩‍👧"),
                        ),
                    ])),
                    "modified Enter should submit in {mode:?} with {modifiers:?}",
                );
                assert_eq!(composer.composer.contents(), "first\n漢👨‍👩‍👧");
            }
        }
    }

    #[test]
    fn control_s_does_not_submit_or_change_the_floating_prompt() {
        let editor = editor(60, 18);

        for mode in [
            ModalComposerMode::Insert,
            ModalComposerMode::Normal,
            ModalComposerMode::Visual,
        ] {
            let mut composer = new_composer(
                &editor,
                Some("Agent prompt".to_string()),
                802,
                "preserve this draft".to_string(),
                vec![],
            );

            if mode != ModalComposerMode::Insert {
                composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE));
                if mode == ModalComposerMode::Visual {
                    composer.handle_event(&key(KeyCode::Char('v'), KeyModifiers::NONE));
                }
            }

            assert_eq!(composer.composer.mode(), mode);
            assert_eq!(
                composer.handle_event(&key(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                None,
                "Ctrl+S must not submit in {mode:?}",
            );
            assert_eq!(composer.composer.contents(), "preserve this draft");
            assert_eq!(composer.composer.mode(), mode);
        }
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

        assert_eq!(
            composer.composer.contents(),
            "first\tline\n  second\nthird\n"
        );
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
        assert_eq!(composer.composer.contents(), "one tXo");

        composer.handle_event(&key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(composer.composer.contents(), "one o");
        composer.handle_event(&key(KeyCode::Home, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(composer.composer.contents(), "ne o");
        composer.handle_event(&key(KeyCode::End, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.composer.contents(), "ne ");
    }

    #[test]
    fn newline_shortcuts_and_vertical_motion_work_on_multiline_buffers() {
        let editor = editor(40, 14);
        let mut composer = new_composer(
            &editor,
            None,
            1,
            format!("{}\n{}", "a".repeat(20), "b".repeat(20)),
            vec![],
        );
        let (_, original_row) = composer.cursor_position().unwrap();

        composer.handle_event(&key(KeyCode::Up, KeyModifiers::NONE));
        let (_, moved_row) = composer.cursor_position().unwrap();
        assert!(moved_row < original_row);
        composer.handle_event(&key(KeyCode::Down, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('j'), KeyModifiers::CONTROL));
        composer.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Enter, KeyModifiers::SHIFT));
        composer.handle_event(&key(KeyCode::Char('y'), KeyModifiers::NONE));

        assert!(composer.composer.contents().ends_with("\nx\ny"));
    }

    #[test]
    fn backspace_and_delete_remove_complete_unicode_graphemes() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 9, "e\u{301}👨‍👩‍👧漢".to_string(), vec![]);

        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.composer.contents(), "e\u{301}👨‍👩‍👧");
        composer.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.composer.contents(), "e\u{301}");
        composer.handle_event(&key(KeyCode::Home, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert!(composer.composer.contents().is_empty());
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
        assert_eq!(composer.composer.contents(), "newer\nprompt");
        composer.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.composer.contents(), "older");
        composer.handle_event(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.composer.contents(), "newer\nprompt");
        composer.handle_event(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.composer.contents(), "current draft");
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
            Some(KeyAction::Single(Action::ShowDialog))
        );
        assert_eq!(composer.composer.mode(), ModalComposerMode::Normal);
        assert_eq!(composer.composer.contents(), " \n\t");
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
            cancelled.handle_event(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
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
    fn compact_status_keeps_control_enter_visible_at_minimum_width() {
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

        assert!(status.contains(STATUS));
        assert!(!status.contains("^S"));
    }

    #[test]
    fn floating_status_shows_control_enter_in_every_vim_mode() {
        let editor = editor(60, 18);
        let mut composer = new_composer(
            &editor,
            Some("Agent prompt".to_string()),
            802,
            "draft".to_string(),
            vec![],
        );

        for mode in [
            ModalComposerMode::Insert,
            ModalComposerMode::Normal,
            ModalComposerMode::Visual,
        ] {
            if mode == ModalComposerMode::Normal {
                composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE));
            } else if mode == ModalComposerMode::Visual {
                composer.handle_event(&key(KeyCode::Char('v'), KeyModifiers::NONE));
            }

            assert_eq!(composer.composer.mode(), mode);

            let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());
            composer.draw(&mut buffer).unwrap();
            let status_y = composer.dialog.y + 1 + composer.body_height();
            let status = rendered_row(&buffer, status_y);

            assert!(
                status.contains("Ctrl+Enter send"),
                "{mode:?} status should expose Ctrl+Enter: {status}",
            );
            assert!(
                !status.contains("^S"),
                "{mode:?} status must not advertise Ctrl+S: {status}",
            );
        }
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
        assert_eq!(composer.composer.contents(), "recent");
        composer.handle_event(&key(
            KeyCode::Char('N'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(composer.composer.contents(), "draft");
        composer.handle_event(&key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(composer.composer.contents(), "draft\n");
    }

    #[test]
    fn oversized_ascii_paste_and_insert_leave_the_existing_draft_unchanged() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "draft".to_string(), vec![]);
        let oversized = "x".repeat(MAX_PROMPT_BYTES);

        composer.handle_event(&Event::Paste(oversized));
        assert_eq!(composer.composer.contents(), "draft");
        assert_eq!(
            composer.composer.validation_status(),
            Some(OVERSIZED_STATUS)
        );
        let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());
        composer.draw(&mut buffer).unwrap();
        let status_y = composer.dialog.y + 1 + composer.body_height();
        assert!(rendered_row(&buffer, status_y).contains(OVERSIZED_STATUS));

        assert!(composer
            .composer
            .set_contents(&"x".repeat(MAX_PROMPT_BYTES)));
        composer.handle_event(&key(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(composer.composer.contents().len(), MAX_PROMPT_BYTES);
        assert_eq!(
            composer.composer.validation_status(),
            Some(OVERSIZED_STATUS)
        );
    }

    #[test]
    fn maximum_escaping_heavy_prompt_fits_the_app_server_frame_and_submits_exactly() {
        let editor = editor(60, 18);
        let accepted = "\u{0}".repeat(MAX_PROMPT_BYTES);
        let mut composer = new_composer(&editor, None, 802, accepted.clone(), vec![]);
        let encoded = serde_json::to_vec(&accepted).unwrap();

        assert!(encoded.len() < 1024 * 1024);
        assert_eq!(composer.composer.contents(), accepted);
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

        assert!(composer.composer.contents().is_empty());
        assert_eq!(composer.composer.cursor(), (0, 0));
        assert_eq!(
            composer.composer.validation_status(),
            Some(OVERSIZED_STATUS)
        );
        let wrapped = composer.wrapped_text();
        assert_eq!(wrapped.rows, vec![String::new()]);
        composer.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.composer.contents(), "safe history");
        assert_eq!(composer.composer.validation_status(), None);
    }

    #[test]
    fn first_prompt_is_centered_in_the_available_viewport() {
        let editor = editor(100, 30);
        let composer = new_composer(&editor, None, 1, String::new(), vec![]);
        let outer_width = composer.dialog.width + 2;
        let outer_height = composer.dialog.height + 2;

        assert_eq!(composer.dialog.x, (100 - outer_width) / 2);
        assert_eq!(composer.dialog.y, (editor.vheight() - outer_height) / 2);
    }

    #[test]
    fn insert_enter_adds_a_line_and_normal_enter_submits_the_complete_prompt() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "first".to_string(), vec![]);

        assert_eq!(
            composer.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::ShowDialog))
        );
        composer.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(composer.composer.contents(), "first\nx");

        assert_eq!(
            composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::ShowDialog))
        );
        assert_eq!(composer.composer.mode(), ModalComposerMode::Normal);
        assert_eq!(
            composer.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::NotifyPlugin(
                    "agent".to_string(),
                    "composer:submitted:802".to_string(),
                    json!("first\nx")
                )
            ]))
        );
    }

    #[test]
    fn floating_normal_mode_uses_real_buffer_operators_and_undo() {
        let editor = editor(60, 18);
        let mut composer = new_composer(&editor, None, 802, "one two".to_string(), vec![]);

        composer.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('0'), KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE));
        composer.handle_event(&key(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(composer.composer.contents(), "two");

        composer.handle_event(&key(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(composer.composer.contents(), "one two");
    }

    #[test]
    fn narrow_wrapping_uses_ascii_fallback_without_splitting_unicode_graphemes() {
        let wrapped = wrap_text("漢👨‍👩‍👧e\u{301}", 1);

        assert_eq!(wrapped.rows, vec!["?", "?", "e\u{301}", ""]);
        assert_eq!(wrapped.positions, vec![(0, 0), (1, 0), (2, 0), (3, 0)]);
    }

    #[test]
    fn floating_placeholder_uses_clean_plain_text_without_quote_prefixes() {
        let editor = editor(60, 18);
        let composer = new_composer(&editor, None, 802, String::new(), vec![]);
        let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());

        composer.draw(&mut buffer).unwrap();
        let body = rendered_row(&buffer, composer.dialog.y + 1);

        assert!(body.contains("What should the agent do?"));
        assert!(!body.contains("> "));
    }

    #[test]
    fn configured_ascii_border_preserves_the_centered_popup_title() {
        let config = Config {
            window_borders_ascii: true,
            ..Config::default()
        };
        let editor = editor_with_config(60, 18, config);
        let composer = new_composer(
            &editor,
            Some("Ask the agent".to_string()),
            802,
            String::new(),
            vec![],
        );
        let mut buffer = RenderBuffer::new(60, editor.vheight(), &Style::default());

        composer.draw(&mut buffer).unwrap();

        let left = composer.dialog.x;
        let right = left + composer.dialog.width + 1;
        let top = composer.dialog.y;
        let bottom = top + composer.dialog.height + 1;
        let top_row = rendered_row(&buffer, top);

        assert!(composer.ascii_borders);
        assert!(top_row.contains(" Ask the agent "));
        assert_eq!(buffer.cells[top * buffer.width + left].c, '+');
        assert_eq!(buffer.cells[top * buffer.width + right].c, '+');
        assert_eq!(buffer.cells[bottom * buffer.width + left].c, '+');
        assert_eq!(buffer.cells[bottom * buffer.width + right].c, '+');
        assert_eq!(buffer.cells[(top + 1) * buffer.width + left].c, '|');
        assert_eq!(buffer.cells[(top + 1) * buffer.width + right].c, '|');
        assert!(!top_row.contains('─'));
        assert!(!top_row.contains('┌'));
    }

    #[test]
    fn default_border_preserves_unicode_corners_and_clips_long_title_inside_them() {
        let editor = editor(8, 8);
        let composer = new_composer(
            &editor,
            Some("A very long 👨‍👩‍👧 title".to_string()),
            802,
            String::new(),
            vec![],
        );
        let mut buffer = RenderBuffer::new(8, editor.vheight(), &Style::default());

        composer.draw(&mut buffer).unwrap();

        let left = composer.dialog.x;
        let right = left + composer.dialog.width + 1;
        let top = composer.dialog.y;
        let bottom = top + composer.dialog.height + 1;

        assert!(!composer.ascii_borders);
        assert_eq!(buffer.cells[top * buffer.width + left].c, '┌');
        assert_eq!(buffer.cells[top * buffer.width + right].c, '┐');
        assert_eq!(buffer.cells[bottom * buffer.width + left].c, '└');
        assert_eq!(buffer.cells[bottom * buffer.width + right].c, '┘');
    }

    #[test]
    fn resizing_to_a_zero_viewport_hides_the_cursor_and_draws_safely() {
        let editor = editor(60, 18);
        let mut composer = new_composer(
            &editor,
            Some("Ask the agent".to_string()),
            802,
            "👨‍👩‍👧漢".to_string(),
            vec![],
        );

        assert!(composer.resize(0, 0));
        let mut buffer = RenderBuffer::new(0, 0, &Style::default());

        composer.draw(&mut buffer).unwrap();
        assert_eq!(composer.cursor_position(), None);
    }

    #[test]
    fn ascii_border_keeps_all_corners_on_screen_after_tiny_resize() {
        let config = Config {
            window_borders_ascii: true,
            ..Config::default()
        };
        let editor = editor_with_config(60, 18, config);
        let mut composer = new_composer(
            &editor,
            Some("A long title".to_string()),
            802,
            "👨‍👩‍👧漢".to_string(),
            vec![],
        );

        for (width, height) in [(8, 4), (2, 2), (1, 1)] {
            assert!(composer.resize(width, height));
            let mut buffer = RenderBuffer::new(width, height, &Style::default());

            composer.draw(&mut buffer).unwrap();

            let (cursor_x, cursor_y) = composer.cursor_position().unwrap();
            assert!(cursor_x < width);
            assert!(cursor_y < height);
            if width >= 2 && height >= 2 {
                let left = composer.dialog.x;
                let right = left + composer.dialog.width + 1;
                let top = composer.dialog.y;
                let bottom = top + composer.dialog.height + 1;
                assert_eq!(buffer.cells[top * width + left].c, '+');
                assert_eq!(buffer.cells[top * width + right].c, '+');
                assert_eq!(buffer.cells[bottom * width + left].c, '+');
                assert_eq!(buffer.cells[bottom * width + right].c, '+');
            }
        }
    }
}
