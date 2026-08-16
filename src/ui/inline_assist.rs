//! Cursor-anchored prompt and result controls for bounded inline code edits.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, Mode, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::{grapheme_len, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    first_prompt_line,
    geometry::{anchored_popup_geometry, anchored_popup_geometry_avoiding_rows},
    spinner_frame, wrap_text, ActionBar, ActionPriority, Component, OverlayLayout, PromptBuffer,
    ScreenRect, UiAction, SPINNER_FRAME_INTERVAL_MS,
};

const MAX_WIDTH: usize = 72;
const MAX_PROMPT_ROWS: usize = 6;
const MAX_ERROR_ROWS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineAssistPopupState {
    Prompt { initial: String, refining: bool },
    Working,
    Applied { edited: bool, comments: usize },
    Failed(String),
}

pub struct InlineAssistPopup {
    state: InlineAssistPopupState,
    prompt: PromptBuffer,
    layout: OverlayLayout,
    dialog: Dialog,
    style: Style,
    theme: Theme,
    spinner_tick: usize,
}

impl InlineAssistPopup {
    fn draw_actions(&self, buffer: &mut RenderBuffer, x: usize, y: usize, width: usize) {
        ActionBar::new(&self.surface_actions()).render(
            buffer,
            x,
            y,
            width,
            &self.theme,
            &self.style,
        );
    }
    pub fn new(editor: &Editor, scope: impl Into<String>, state: InlineAssistPopupState) -> Self {
        let local_anchor = editor.cursor_position();
        let anchor = editor.render_cursor_position().unwrap_or(local_anchor);
        let viewport_y_offset = anchor.1.saturating_sub(local_anchor.1);
        Self::new_in_layout(
            editor,
            scope,
            state,
            OverlayLayout {
                viewport: ScreenRect {
                    x: 0,
                    y: 0,
                    width: editor.vwidth(),
                    height: editor.vheight().saturating_add(viewport_y_offset),
                },
                anchor,
                avoid_rows: None,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn new_avoiding_rows(
        editor: &Editor,
        scope: impl Into<String>,
        state: InlineAssistPopupState,
        avoid_rows: Option<(usize, usize)>,
    ) -> Self {
        let local_anchor = editor.cursor_position();
        let anchor = editor.render_cursor_position().unwrap_or(local_anchor);
        let viewport_y_offset = anchor.1.saturating_sub(local_anchor.1);
        Self::new_in_layout(
            editor,
            scope,
            state,
            OverlayLayout {
                viewport: ScreenRect {
                    x: 0,
                    y: 0,
                    width: editor.vwidth(),
                    height: editor.vheight().saturating_add(viewport_y_offset),
                },
                anchor,
                avoid_rows,
            },
        )
    }

    pub(crate) fn new_in_layout(
        editor: &Editor,
        scope: impl Into<String>,
        state: InlineAssistPopupState,
        layout: OverlayLayout,
    ) -> Self {
        let scope = scope.into();
        let initial = match &state {
            InlineAssistPopupState::Prompt { initial, .. } => initial.clone(),
            _ => String::new(),
        };
        let prompt = PromptBuffer::new(&initial);
        let width = Self::content_width(layout.viewport.width);
        let desired_height = Self::content_height(&state, &prompt, width);
        let (x, y, height) = Self::geometry(layout, width, desired_height);
        let style = editor.theme.ui_style.dialog.clone();
        let dialog = Dialog::new(
            Some(format!("Inline assist · {scope}")),
            x,
            y,
            width,
            height,
            &style,
            BorderStyle::Rounded,
            &editor.theme,
        )
        .with_surface_theme(&editor.theme, SurfaceRole::Dialog);
        Self {
            state,
            prompt,
            layout,
            dialog,
            style,
            theme: editor.theme.clone(),
            spinner_tick: 0,
        }
    }

    fn content_width(viewport_width: usize) -> usize {
        viewport_width.saturating_sub(2).min(MAX_WIDTH)
    }

    fn prompt_width(width: usize) -> usize {
        width.saturating_sub(2).max(1)
    }

    fn content_height(
        state: &InlineAssistPopupState,
        prompt: &PromptBuffer,
        width: usize,
    ) -> usize {
        match state {
            InlineAssistPopupState::Prompt { .. } => {
                wrap_text(&prompt.text(), Self::prompt_width(width))
                    .rows
                    .len()
                    .clamp(1, MAX_PROMPT_ROWS)
                    .saturating_add(1)
            }
            InlineAssistPopupState::Working => 2,
            InlineAssistPopupState::Applied { .. } => 2,
            InlineAssistPopupState::Failed(message) => wrap_text(message, width.max(1))
                .rows
                .len()
                .clamp(1, MAX_ERROR_ROWS)
                .saturating_add(2),
        }
    }

    fn geometry(layout: OverlayLayout, width: usize, height: usize) -> (usize, usize, usize) {
        let viewport = layout.viewport;
        let anchor = (
            layout
                .anchor
                .0
                .saturating_sub(viewport.x)
                .min(viewport.width.saturating_sub(1)),
            layout
                .anchor
                .1
                .saturating_sub(viewport.y)
                .min(viewport.height.saturating_sub(1)),
        );
        let avoid_rows = layout.avoid_rows.and_then(|(start, end)| {
            let viewport_end = viewport.y.saturating_add(viewport.height.saturating_sub(1));
            let start = start.max(viewport.y);
            let end = end.min(viewport_end);
            (start <= end).then_some((
                start.saturating_sub(viewport.y),
                end.saturating_sub(viewport.y),
            ))
        });
        let (x, y, height) = avoid_rows.map_or_else(
            || anchored_popup_geometry(anchor, viewport.width, viewport.height, width, height),
            |avoid_rows| {
                anchored_popup_geometry_avoiding_rows(
                    anchor,
                    avoid_rows,
                    viewport.width,
                    viewport.height,
                    width,
                    height,
                )
            },
        );
        (
            viewport.x.saturating_add(x),
            viewport.y.saturating_add(y),
            height,
        )
    }

    fn insert(&mut self, text: &str) {
        self.prompt.insert(&first_prompt_line(text));
    }

    fn refresh_action() -> Option<KeyAction> {
        Some(KeyAction::Single(Action::Refresh))
    }

    fn reflow(&mut self) {
        let width = Self::content_width(self.layout.viewport.width);
        let desired_height = Self::content_height(&self.state, &self.prompt, width);
        let (x, y, height) = Self::geometry(self.layout, width, desired_height);
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
    }

    fn prompt_changed(&mut self) -> Option<KeyAction> {
        self.reflow();
        Self::refresh_action()
    }

    fn wrapped_prompt(&self) -> super::agent_composer::WrappedText {
        wrap_text(&self.prompt.text(), Self::prompt_width(self.dialog.width))
    }

    fn inside(&self, column: usize, row: usize) -> bool {
        (self.dialog.x
            ..self
                .dialog
                .x
                .saturating_add(self.dialog.width)
                .saturating_add(2))
            .contains(&column)
            && (self.dialog.y
                ..self
                    .dialog
                    .y
                    .saturating_add(self.dialog.height)
                    .saturating_add(2))
                .contains(&row)
    }
}

impl Component for InlineAssistPopup {
    fn surface_actions(&self) -> Vec<UiAction> {
        let essential =
            |id, key, label| UiAction::new(id, key, label).with_priority(ActionPriority::Essential);
        match &self.state {
            InlineAssistPopupState::Prompt { .. } => vec![
                essential("apply", "Enter", "apply"),
                essential("cancel", "Esc", "cancel"),
            ],
            InlineAssistPopupState::Working => vec![essential("cancel", "Esc", "cancel")],
            InlineAssistPopupState::Applied { edited, .. } => vec![
                essential("keep", "Enter", "keep"),
                UiAction::new("undo", "u", if *edited { "undo" } else { "dismiss" }),
                UiAction::new("refine", "r", "refine"),
                UiAction::new("agent", "A", "agent"),
            ],
            InlineAssistPopupState::Failed(_) => vec![
                essential("retry", "r", "retry/refine"),
                essential("close", "Esc", "close"),
            ],
        }
    }
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let x = self.dialog.x.saturating_add(1);
        let y = self.dialog.y.saturating_add(1);
        let width = self.dialog.width;
        match &self.state {
            InlineAssistPopupState::Prompt { .. } => {
                let show_help = self.dialog.height > 1;
                let body_height = self.dialog.height.saturating_sub(usize::from(show_help));
                if body_height > 0 {
                    let wrapped = self.wrapped_prompt();
                    let cursor_row = wrapped
                        .positions
                        .get(self.prompt.cursor())
                        .map_or(0, |position| position.0);
                    let scroll = cursor_row.saturating_sub(body_height.saturating_sub(1));
                    for (offset, row) in wrapped
                        .rows
                        .iter()
                        .skip(scroll)
                        .take(body_height)
                        .enumerate()
                    {
                        let marker = if scroll.saturating_add(offset) == 0 {
                            ">"
                        } else {
                            "│"
                        };
                        buffer.set_text(x, y.saturating_add(offset), marker, &self.style);
                        buffer.set_text(
                            x.saturating_add(2),
                            y.saturating_add(offset),
                            row,
                            &self.style,
                        );
                    }
                }
                if show_help {
                    self.draw_actions(
                        buffer,
                        x,
                        y.saturating_add(self.dialog.height.saturating_sub(1)),
                        width,
                    );
                }
            }
            InlineAssistPopupState::Working => {
                let message = format!(
                    "{} Preparing inline result…",
                    spinner_frame(self.spinner_tick as u64 * SPINNER_FRAME_INTERVAL_MS)
                );
                if self.dialog.height > 0 {
                    buffer.set_text(x, y, &truncate_display_width(&message, width), &self.style);
                }
                if self.dialog.height > 1 {
                    self.draw_actions(buffer, x, y.saturating_add(1), width);
                }
            }
            InlineAssistPopupState::Applied { edited, comments } => {
                let message = match (*edited, *comments) {
                    (true, 0) => "Applied to buffer (unsaved)".to_string(),
                    (true, count) => format!("Applied unsaved edit · {count} comment(s)"),
                    (false, 0) => "No changes or comments needed".to_string(),
                    (false, count) => format!("Added {count} inline comment(s) · code unchanged"),
                };
                if self.dialog.height > 0 {
                    buffer.set_text(x, y, &truncate_display_width(&message, width), &self.style);
                }
                if self.dialog.height > 1 {
                    self.draw_actions(buffer, x, y.saturating_add(1), width);
                }
            }
            InlineAssistPopupState::Failed(message) => {
                if self.dialog.height > 0 {
                    buffer.set_text(
                        x,
                        y,
                        &truncate_display_width("Inline assist failed", width),
                        &self.style,
                    );
                }
                let message_height = self.dialog.height.saturating_sub(2);
                for (offset, row) in wrap_text(message, width.max(1))
                    .rows
                    .iter()
                    .take(message_height)
                    .enumerate()
                {
                    buffer.set_text(x, y.saturating_add(1 + offset), row, &self.style);
                }
                if self.dialog.height > 1 {
                    self.draw_actions(
                        buffer,
                        x,
                        y.saturating_add(self.dialog.height.saturating_sub(1)),
                        width,
                    );
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> anyhow::Result<bool> {
        if matches!(self.state, InlineAssistPopupState::Working) {
            self.spinner_tick = self.spinner_tick.saturating_add(1);
            return Ok(self
                .spinner_tick
                .is_multiple_of(SPINNER_FRAME_INTERVAL_MS as usize / 10));
        }
        Ok(false)
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.layout.viewport.width = viewport_width;
        self.layout.viewport.height = viewport_height;
        self.reflow();
        true
    }

    fn update_overlay_layout(&mut self, layout: OverlayLayout) -> bool {
        self.layout = layout;
        self.reflow();
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.dialog.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.theme = theme.clone();
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        if let Event::Mouse(mouse) = event {
            if matches!(mouse.kind, MouseEventKind::Down(_))
                && !self.inside(mouse.column as usize, mouse.row as usize)
            {
                let action = match &self.state {
                    InlineAssistPopupState::Applied { .. } => Action::KeepInlineAssist,
                    InlineAssistPopupState::Prompt { refining: true, .. } => {
                        Action::CancelInlineAssistRefine
                    }
                    _ => Action::CancelInlineAssist,
                };
                return Some(KeyAction::Single(action));
            }
            return None;
        }
        match &self.state {
            InlineAssistPopupState::Prompt { refining, .. } => match event {
                Event::Paste(text) => {
                    self.insert(text);
                    self.prompt_changed()
                }
                Event::Key(key) => match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        Some(KeyAction::Single(if *refining {
                            Action::CancelInlineAssistRefine
                        } else {
                            Action::CancelInlineAssist
                        }))
                    }
                    (KeyCode::Enter, _) => {
                        let prompt = self.prompt.text().trim().to_string();
                        (!prompt.is_empty())
                            .then_some(KeyAction::Single(Action::SubmitInlineAssist(prompt)))
                    }
                    (KeyCode::Left, _) => {
                        self.prompt
                            .set_cursor(self.prompt.cursor().saturating_sub(1));
                        Self::refresh_action()
                    }
                    (KeyCode::Right, _) => {
                        self.prompt
                            .set_cursor(self.prompt.cursor().saturating_add(1));
                        Self::refresh_action()
                    }
                    (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                        self.prompt.set_cursor(0);
                        Self::refresh_action()
                    }
                    (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                        self.prompt.set_cursor(grapheme_len(&self.prompt.text()));
                        Self::refresh_action()
                    }
                    (KeyCode::Backspace, _) => {
                        self.prompt.backspace();
                        self.prompt_changed()
                    }
                    (KeyCode::Delete, _) => {
                        self.prompt.delete();
                        self.prompt_changed()
                    }
                    (KeyCode::Char(character), modifiers)
                        if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.insert(&character.to_string());
                        self.prompt_changed()
                    }
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::Working => match event {
                Event::Key(key) if matches!(key.code, KeyCode::Esc) => {
                    Some(KeyAction::Single(Action::CancelInlineAssist))
                }
                _ => None,
            },
            InlineAssistPopupState::Applied { .. } => match event {
                Event::Key(key) => match key.code {
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('k') => {
                        Some(KeyAction::Single(Action::KeepInlineAssist))
                    }
                    KeyCode::Char('u') => Some(KeyAction::Single(Action::UndoInlineAssist)),
                    KeyCode::Char('r') => Some(KeyAction::Single(Action::RefineInlineAssist)),
                    KeyCode::Char('A') => Some(KeyAction::Single(Action::EscalateInlineAssist)),
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::Failed(_) => match event {
                Event::Key(key) => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        Some(KeyAction::Single(Action::CancelInlineAssist))
                    }
                    KeyCode::Enter | KeyCode::Char('r') => {
                        Some(KeyAction::Single(Action::RefineInlineAssist))
                    }
                    _ => None,
                },
                _ => None,
            },
        }
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        if !matches!(self.state, InlineAssistPopupState::Prompt { .. }) {
            return None;
        }
        let body_height = self
            .dialog
            .height
            .saturating_sub(usize::from(self.dialog.height > 1));
        if body_height == 0 {
            return None;
        }
        let wrapped = self.wrapped_prompt();
        let (row, column) = wrapped
            .positions
            .get(self.prompt.cursor())
            .copied()
            .unwrap_or_default();
        let scroll = row.saturating_sub(body_height.saturating_sub(1));
        Some((
            self.dialog.x.saturating_add(3).saturating_add(column).min(
                self.layout
                    .viewport
                    .x
                    .saturating_add(self.layout.viewport.width.saturating_sub(1)),
            ),
            self.dialog
                .y
                .saturating_add(1)
                .saturating_add(row.saturating_sub(scroll))
                .min(
                    self.layout
                        .viewport
                        .y
                        .saturating_add(self.layout.viewport.height.saturating_sub(1)),
                ),
        ))
    }

    fn cursor_mode(&self) -> Option<Mode> {
        matches!(self.state, InlineAssistPopupState::Prompt { .. }).then_some(Mode::Insert)
    }

    fn is_sensitive_input(&self) -> bool {
        matches!(self.state, InlineAssistPopupState::Prompt { .. })
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};

    fn editor() -> Editor {
        let config = Config::default();
        Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            60,
            14,
            config,
            Theme::default(),
            vec![Buffer::new(None, "fn main() {}\n".to_string())],
        )
        .unwrap()
    }

    #[test]
    fn prompt_submits_bounded_action_and_cancel_is_explicit() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new(
            &editor,
            "line 1",
            InlineAssistPopupState::Prompt {
                initial: String::new(),
                refining: false,
            },
        );
        assert!(popup.is_sensitive_input());
        popup.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        assert_eq!(
            popup.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Some(KeyAction::Single(Action::SubmitInlineAssist(
                "x".to_string()
            )))
        );
        assert_eq!(
            popup.handle_event(&Event::Key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)
            )),
            Some(KeyAction::Single(Action::CancelInlineAssist))
        );
    }

    #[test]
    fn applied_state_exposes_keep_undo_refine_and_escalate() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new(
            &editor,
            "selection",
            InlineAssistPopupState::Applied {
                edited: true,
                comments: 0,
            },
        );
        for (key, action) in [
            (KeyCode::Enter, Action::KeepInlineAssist),
            (KeyCode::Char('u'), Action::UndoInlineAssist),
            (KeyCode::Char('r'), Action::RefineInlineAssist),
            (KeyCode::Char('A'), Action::EscalateInlineAssist),
        ] {
            assert_eq!(
                popup.handle_event(&Event::Key(KeyEvent::new(key, KeyModifiers::NONE))),
                Some(KeyAction::Single(action))
            );
        }
    }

    #[test]
    fn popup_avoids_the_rendered_target_rows() {
        let editor = editor();
        let avoid_rows = (4, 7);
        let popup = InlineAssistPopup::new_avoiding_rows(
            &editor,
            "lines 5–8 selection",
            InlineAssistPopupState::Applied {
                edited: true,
                comments: 0,
            },
            Some(avoid_rows),
        );
        let popup_last_row = popup
            .dialog
            .y
            .saturating_add(popup.dialog.height)
            .saturating_add(1);

        assert!(popup_last_row < avoid_rows.0 || popup.dialog.y > avoid_rows.1);
    }

    #[test]
    fn prompt_soft_wraps_grows_and_stays_inside_its_window() {
        let editor = editor();
        let viewport = ScreenRect {
            x: 30,
            y: 2,
            width: 30,
            height: 12,
        };
        let avoid_rows = (7, 7);
        let mut popup = InlineAssistPopup::new_in_layout(
            &editor,
            "line 8",
            InlineAssistPopupState::Prompt {
                initial: String::new(),
                refining: false,
            },
            OverlayLayout {
                viewport,
                anchor: (40, 7),
                avoid_rows: Some(avoid_rows),
            },
        );
        let initial_height = popup.dialog.height;

        popup.handle_event(&Event::Paste(format!(
            "{}TAIL",
            "expand this request ".repeat(12)
        )));

        let popup_last_column = popup
            .dialog
            .x
            .saturating_add(popup.dialog.width)
            .saturating_add(1);
        let popup_last_row = popup
            .dialog
            .y
            .saturating_add(popup.dialog.height)
            .saturating_add(1);
        assert!(popup.dialog.height > initial_height);
        assert!(popup.dialog.height <= MAX_PROMPT_ROWS + 1);
        assert!(popup.dialog.x >= viewport.x);
        assert!(popup_last_column < viewport.x + viewport.width);
        assert!(popup.dialog.y >= viewport.y);
        assert!(popup_last_row < viewport.y + viewport.height);
        assert!(popup_last_row < avoid_rows.0 || popup.dialog.y > avoid_rows.1);
        let cursor = popup.cursor_position().unwrap();
        assert!((viewport.x..viewport.x + viewport.width).contains(&cursor.0));
        assert!((viewport.y..viewport.y + viewport.height).contains(&cursor.1));
        let mut buffer = RenderBuffer::new(60, 14, &Style::default());
        popup.draw(&mut buffer).unwrap();
        let rendered = buffer.cells.iter().map(|cell| cell.c).collect::<String>();
        assert!(rendered.contains("TAIL"));
    }

    #[test]
    fn applied_popup_uses_the_owning_split_coordinates() {
        let editor = editor();
        let viewport = ScreenRect {
            x: 42,
            y: 1,
            width: 18,
            height: 10,
        };
        let mut popup = InlineAssistPopup::new_in_layout(
            &editor,
            "line 4",
            InlineAssistPopupState::Applied {
                edited: true,
                comments: 0,
            },
            OverlayLayout {
                viewport,
                anchor: (48, 4),
                avoid_rows: Some((4, 4)),
            },
        );

        assert!(popup.dialog.x >= viewport.x);
        assert!(
            popup
                .dialog
                .x
                .saturating_add(popup.dialog.width)
                .saturating_add(2)
                <= viewport.x + viewport.width
        );

        let resized_viewport = ScreenRect {
            x: 24,
            y: 2,
            width: 14,
            height: 8,
        };
        assert!(popup.update_overlay_layout(OverlayLayout {
            viewport: resized_viewport,
            anchor: (28, 4),
            avoid_rows: Some((4, 4)),
        }));
        assert!(popup.dialog.x >= resized_viewport.x);
        assert!(
            popup
                .dialog
                .x
                .saturating_add(popup.dialog.width)
                .saturating_add(2)
                <= resized_viewport.x + resized_viewport.width
        );
    }
}
