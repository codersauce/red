//! Compact, reusable Accept/Cancel confirmation dialog.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde_json::Value;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, PickerCallback, RenderBuffer},
    plugin::PickerHandle,
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    agent_composer::wrap_text,
    dialog::{BorderStyle, Dialog, SurfaceRole},
    Component, PickerItem,
};

const BUTTON_GAP: usize = 2;

enum ConfirmationTarget {
    Callback(PickerHandle),
    Actions {
        accept: Box<Action>,
        cancel: Box<Action>,
    },
}

/// A confirmation surface that defaults to the safe Cancel action.
pub struct Confirmation {
    dialog: Dialog,
    message: String,
    accept_selected: bool,
    target: ConfirmationTarget,
    accept_label: String,
    cancel_label: String,
    multiline: bool,
    scroll: usize,
    style: Style,
    theme: Theme,
}

impl Confirmation {
    pub fn new_callback(
        editor: &Editor,
        title: impl Into<String>,
        message: impl Into<String>,
        callback_handle: PickerHandle,
    ) -> Self {
        Self::with_target(
            editor,
            title.into(),
            message.into(),
            "Accept",
            "Cancel",
            false,
            ConfirmationTarget::Callback(callback_handle),
        )
    }

    /// Creates an editor-owned, multiline confirmation with explicit terminal actions.
    pub fn new_actions(
        editor: &Editor,
        title: impl Into<String>,
        message: impl Into<String>,
        accept_label: impl Into<String>,
        cancel_label: impl Into<String>,
        accept: Action,
        cancel: Action,
    ) -> Self {
        let accept_label = accept_label.into();
        let cancel_label = cancel_label.into();
        Self::with_target(
            editor,
            title.into(),
            message.into(),
            &accept_label,
            &cancel_label,
            true,
            ConfirmationTarget::Actions {
                accept: Box::new(accept),
                cancel: Box::new(cancel),
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_target(
        editor: &Editor,
        title: String,
        message: String,
        accept_label: &str,
        cancel_label: &str,
        multiline: bool,
        target: ConfirmationTarget,
    ) -> Self {
        let style = editor.theme.ui_style.dialog.clone();
        let accept_label = format!("[ {accept_label} ]");
        let cancel_label = format!("[ {cancel_label} ]");
        let (width, height) = confirmation_size(
            editor.vwidth(),
            editor.vheight(),
            &message,
            &accept_label,
            &cancel_label,
            multiline,
        );
        let x = editor.vwidth().saturating_sub(width + 2) / 2;
        let y = editor.vheight().saturating_sub(height + 2) / 2;
        Self {
            dialog: Dialog::new(
                Some(title),
                x,
                y,
                width,
                height,
                &style,
                BorderStyle::Single,
                &editor.theme,
            )
            .with_surface_theme(&editor.theme, SurfaceRole::Dialog),
            message,
            accept_selected: false,
            target,
            accept_label,
            cancel_label,
            multiline,
            scroll: 0,
            style,
            theme: editor.theme.clone(),
        }
    }

    fn terminal_action(&self, accepted: bool) -> KeyAction {
        match &self.target {
            ConfirmationTarget::Callback(handle) => {
                let callback = if accepted {
                    PickerCallback::Selected(PickerItem {
                        id: "accept".to_string(),
                        icon: None,
                        label: "Accept".to_string(),
                        kind: Some("Proceed".to_string()),
                        annotation: None,
                        detail: None,
                        data: Value::Null,
                        matches: Vec::new(),
                        detail_matches: Vec::new(),
                        preview: None,
                    })
                } else {
                    PickerCallback::Cancelled
                };
                KeyAction::Multiple(vec![
                    Action::NotifyPicker(*handle, Box::new(callback)),
                    Action::CloseDialog,
                ])
            }
            ConfirmationTarget::Actions { accept, cancel } => KeyAction::Multiple(vec![
                Action::CloseDialog,
                if accepted {
                    accept.as_ref().clone()
                } else {
                    cancel.as_ref().clone()
                },
            ]),
        }
    }

    fn body_rows(&self) -> Vec<String> {
        if self.multiline {
            wrap_text(&self.message, self.dialog.width).rows
        } else {
            vec![truncate_display_width(&self.message, self.dialog.width)]
        }
    }

    fn body_height(&self) -> usize {
        self.dialog.height.saturating_sub(1)
    }

    fn max_scroll(&self) -> usize {
        self.body_rows().len().saturating_sub(self.body_height())
    }
}

impl Component for Confirmation {
    fn picker_handle(&self) -> Option<PickerHandle> {
        match &self.target {
            ConfirmationTarget::Callback(handle) => Some(*handle),
            ConfirmationTarget::Actions { .. } => None,
        }
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.dialog.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.theme = theme.clone();
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        (self.dialog.width, self.dialog.height) = confirmation_size(
            viewport_width,
            viewport_height,
            &self.message,
            &self.accept_label,
            &self.cancel_label,
            self.multiline,
        );
        self.dialog.x = viewport_width.saturating_sub(self.dialog.width + 2) / 2;
        self.dialog.y = viewport_height.saturating_sub(self.dialog.height + 2) / 2;
        self.scroll = self.scroll.min(self.max_scroll());
        true
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let rows = self.body_rows();
        for (offset, row) in rows
            .iter()
            .skip(self.scroll)
            .take(self.body_height())
            .enumerate()
        {
            buffer.set_text(
                self.dialog.x + 1,
                self.dialog.y + 1 + offset,
                row,
                &self.style,
            );
        }

        let buttons_width =
            display_width(&self.accept_label) + BUTTON_GAP + display_width(&self.cancel_label);
        let button_x = self.dialog.x + 1 + self.dialog.width.saturating_sub(buttons_width) / 2;
        let button_y = self.dialog.y + self.dialog.height;
        let selected = self.theme.selected_style(
            &self.style,
            &self.theme.ui_style.picker_selected_item,
            crate::theme::SelectionForegroundPriority::Selection,
        );
        buffer.set_text(
            button_x,
            button_y,
            &self.accept_label,
            if self.accept_selected {
                &selected
            } else {
                &self.style
            },
        );
        buffer.set_text(
            button_x + display_width(&self.accept_label) + BUTTON_GAP,
            button_y,
            &self.cancel_label,
            if self.accept_selected {
                &self.style
            } else {
                &selected
            },
        );
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else {
            return None;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                Some(self.terminal_action(false))
            }
            (KeyCode::Left | KeyCode::BackTab, _) => {
                self.accept_selected = true;
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Right | KeyCode::Tab, _) => {
                self.accept_selected = false;
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Up | KeyCode::Char('k'), _) if self.multiline => {
                self.scroll = self.scroll.saturating_sub(1);
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Down | KeyCode::Char('j'), _) if self.multiline => {
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll());
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Char('y' | 'Y'), _) => Some(self.terminal_action(true)),
            (KeyCode::Char('n' | 'N'), _) => Some(self.terminal_action(false)),
            (KeyCode::Enter, _) => Some(self.terminal_action(self.accept_selected)),
            _ => None,
        }
    }
}

fn confirmation_size(
    viewport_width: usize,
    viewport_height: usize,
    message: &str,
    accept_label: &str,
    cancel_label: &str,
    multiline: bool,
) -> (usize, usize) {
    let buttons_width = display_width(accept_label) + BUTTON_GAP + display_width(cancel_label);
    let desired_width = message
        .lines()
        .map(display_width)
        .max()
        .unwrap_or_default()
        .max(buttons_width);
    let max_width = if multiline { 76 } else { 60 };
    let width = desired_width
        .min(max_width)
        .min(viewport_width.saturating_sub(2))
        .max(1);
    if !multiline {
        return (width, 2.min(viewport_height.saturating_sub(2)));
    }
    let body_rows = wrap_text(message, width).rows.len().max(1);
    let height = body_rows
        .saturating_add(1)
        .min(viewport_height.saturating_sub(2))
        .max(2.min(viewport_height.saturating_sub(2)));
    (width, height)
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
            80,
            20,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap()
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn confirmation_defaults_to_cancel_and_can_accept_from_the_keyboard() {
        let editor = editor();
        let handle = PickerHandle::from_raw(7);
        let mut confirmation =
            Confirmation::new_callback(&editor, "Delete file?", "This cannot be undone.", handle);

        assert_eq!(
            confirmation.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::NotifyPicker(handle, Box::new(PickerCallback::Cancelled)),
                Action::CloseDialog,
            ]))
        );

        confirmation.handle_event(&key(KeyCode::Left));
        assert!(matches!(
            confirmation.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(actions))
                if matches!(
                    actions.first(),
                    Some(Action::NotifyPicker(
                        callback_handle,
                        event,
                    )) if *callback_handle == handle
                        && matches!(event.as_ref(), PickerCallback::Selected(item) if item.id == "accept")
                )
        ));
    }

    #[test]
    fn confirmation_stays_compact_and_clips_long_messages() {
        let editor = editor();
        let confirmation = Confirmation::new_callback(
            &editor,
            "Delete?",
            "A very long explanation that should stay inside a compact dialog instead of becoming a picker.",
            PickerHandle::from_raw(1),
        );
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());

        confirmation.draw(&mut buffer).unwrap();

        assert_eq!(confirmation.dialog.height, 2);
        assert!(confirmation.dialog.width <= 60);
    }

    #[test]
    fn editor_confirmation_wraps_multiline_details_and_defaults_to_the_safe_action() {
        let editor = editor();
        let digest = "a".repeat(64);
        let mut confirmation = Confirmation::new_actions(
            &editor,
            "Approve native grammar",
            format!(
                "Native grammars execute inside Red.\n\ngo: {digest}\n\nApproval is limited to these exact bytes."
            ),
            "Approve and install",
            "Back",
            Action::Print("approved".to_string()),
            Action::Print("back".to_string()),
        );
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());

        confirmation.draw(&mut buffer).unwrap();

        assert!(confirmation.dialog.height > 2);
        assert!(confirmation.dialog.width <= 76);
        assert_eq!(
            confirmation.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print("back".to_string()),
            ]))
        );

        confirmation.handle_event(&key(KeyCode::Left));
        assert_eq!(
            confirmation.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print("approved".to_string()),
            ]))
        );
    }

    #[test]
    fn multiline_confirmation_scrolls_inside_a_small_viewport() {
        let editor = editor();
        let mut confirmation = Confirmation::new_actions(
            &editor,
            "Approve native grammars",
            (0..12)
                .map(|index| format!("language-{index}: {}", "a".repeat(64)))
                .collect::<Vec<_>>()
                .join("\n"),
            "Approve exact bytes",
            "Back",
            Action::Print("approved".to_string()),
            Action::Print("back".to_string()),
        );
        confirmation.resize(44, 10);
        let mut buffer = RenderBuffer::new(44, 10, &Style::default());

        confirmation.draw(&mut buffer).unwrap();
        assert!(confirmation.max_scroll() > 0);
        assert_eq!(confirmation.scroll, 0);

        confirmation.handle_event(&key(KeyCode::Down));
        assert_eq!(confirmation.scroll, 1);
    }
}
