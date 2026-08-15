//! Cursor-anchored prompt and result controls for bounded inline code edits.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, Mode, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::{display_width, grapheme_len, grapheme_to_byte, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    first_prompt_line,
    geometry::anchored_popup_geometry,
    spinner_frame, Component, PromptBuffer, SPINNER_FRAME_INTERVAL_MS,
};

const MAX_WIDTH: usize = 72;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineAssistPopupState {
    Prompt { initial: String, refining: bool },
    Working,
    Applied,
    Failed(String),
}

pub struct InlineAssistPopup {
    state: InlineAssistPopupState,
    prompt: PromptBuffer,
    anchor: (usize, usize),
    viewport_y_offset: usize,
    dialog: Dialog,
    style: Style,
    theme: Theme,
    spinner_tick: usize,
}

impl InlineAssistPopup {
    pub fn new(editor: &Editor, scope: impl Into<String>, state: InlineAssistPopupState) -> Self {
        let scope = scope.into();
        let initial = match &state {
            InlineAssistPopupState::Prompt { initial, .. } => initial.clone(),
            _ => String::new(),
        };
        let local_anchor = editor.cursor_position();
        let anchor = editor.render_cursor_position().unwrap_or(local_anchor);
        let viewport_y_offset = anchor.1.saturating_sub(local_anchor.1);
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight().saturating_add(viewport_y_offset);
        let width = viewport_width.saturating_sub(2).clamp(1, MAX_WIDTH);
        let (x, y, height) = anchored_popup_geometry(
            anchor,
            viewport_width,
            viewport_height,
            width,
            Self::content_height(&state),
        );
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
            prompt: PromptBuffer::new(&initial),
            anchor,
            viewport_y_offset,
            dialog,
            style,
            theme: editor.theme.clone(),
            spinner_tick: 0,
        }
    }

    fn content_height(state: &InlineAssistPopupState) -> usize {
        match state {
            InlineAssistPopupState::Prompt { .. } => 2,
            InlineAssistPopupState::Working => 2,
            InlineAssistPopupState::Applied => 2,
            InlineAssistPopupState::Failed(_) => 3,
        }
    }

    fn insert(&mut self, text: &str) {
        self.prompt.insert(&first_prompt_line(text));
    }

    fn refresh_action() -> Option<KeyAction> {
        Some(KeyAction::Single(Action::Refresh))
    }

    fn reflow(&mut self, viewport_width: usize, viewport_height: usize) {
        let viewport_height = viewport_height.saturating_add(self.viewport_y_offset);
        let width = viewport_width.saturating_sub(2).clamp(1, MAX_WIDTH);
        let (x, y, height) = anchored_popup_geometry(
            self.anchor,
            viewport_width,
            viewport_height,
            width,
            Self::content_height(&self.state),
        );
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
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
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let x = self.dialog.x.saturating_add(1);
        let y = self.dialog.y.saturating_add(1);
        let width = self.dialog.width;
        match &self.state {
            InlineAssistPopupState::Prompt { .. } => {
                let value = self.prompt.text();
                let visible = truncate_display_width(&format!("> {value}"), width);
                buffer.set_text(x, y, &visible, &self.style);
                buffer.set_text(
                    x,
                    y.saturating_add(1),
                    &truncate_display_width("Enter apply · Esc cancel", width),
                    &self.theme.ui_style.muted,
                );
            }
            InlineAssistPopupState::Working => {
                let message = format!(
                    "{} Generating bounded replacement…",
                    spinner_frame(self.spinner_tick as u64 * SPINNER_FRAME_INTERVAL_MS)
                );
                buffer.set_text(x, y, &truncate_display_width(&message, width), &self.style);
                buffer.set_text(
                    x,
                    y.saturating_add(1),
                    &truncate_display_width("Esc cancel", width),
                    &self.theme.ui_style.muted,
                );
            }
            InlineAssistPopupState::Applied => {
                buffer.set_text(x, y, "Applied to buffer (unsaved)", &self.style);
                buffer.set_text(
                    x,
                    y.saturating_add(1),
                    &truncate_display_width("Enter keep · u undo · r refine · A agent", width),
                    &self.theme.ui_style.muted,
                );
            }
            InlineAssistPopupState::Failed(message) => {
                buffer.set_text(
                    x,
                    y,
                    &truncate_display_width("Inline assist failed", width),
                    &self.style,
                );
                buffer.set_text(
                    x,
                    y.saturating_add(1),
                    &truncate_display_width(message, width),
                    &self.style,
                );
                buffer.set_text(
                    x,
                    y.saturating_add(2),
                    &truncate_display_width("r retry/refine · Esc close", width),
                    &self.theme.ui_style.muted,
                );
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
        self.reflow(viewport_width, viewport_height);
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
                    InlineAssistPopupState::Applied => Action::KeepInlineAssist,
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
                    Self::refresh_action()
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
                        Self::refresh_action()
                    }
                    (KeyCode::Delete, _) => {
                        self.prompt.delete();
                        Self::refresh_action()
                    }
                    (KeyCode::Char(character), modifiers)
                        if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.insert(&character.to_string());
                        Self::refresh_action()
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
            InlineAssistPopupState::Applied => match event {
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
        let value = self.prompt.text();
        let prefix = &value[..grapheme_to_byte(&value, self.prompt.cursor())];
        let offset = display_width(prefix).min(self.dialog.width.saturating_sub(3));
        Some((
            self.dialog.x.saturating_add(3 + offset),
            self.dialog.y.saturating_add(1),
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
        let mut popup =
            InlineAssistPopup::new(&editor, "selection", InlineAssistPopupState::Applied);
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
}
