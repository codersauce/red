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
    dialog::{BorderStyle, Dialog},
    Component, PickerItem,
};

const ACCEPT_LABEL: &str = "[ Accept ]";
const CANCEL_LABEL: &str = "[ Cancel ]";
const BUTTON_GAP: usize = 2;

/// A two-line confirmation surface that defaults to the safe Cancel action.
pub struct Confirmation {
    dialog: Dialog,
    message: String,
    accept_selected: bool,
    callback_handle: PickerHandle,
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
        let title = title.into();
        let message = message.into();
        let style = editor.theme.ui_style.dialog.clone();
        let border_style = editor.theme.ui_style.dialog_border.clone();
        let title_style = editor.theme.ui_style.dialog_title.clone();
        let width = confirmation_width(editor.vwidth(), &message);
        let x = editor.vwidth().saturating_sub(width + 2) / 2;
        let y = editor.vheight().saturating_sub(4) / 2;
        Self {
            dialog: Dialog::new(
                Some(title),
                x,
                y,
                width,
                2,
                &style,
                BorderStyle::Single,
                &editor.theme,
            )
            .with_border_draw_style(&border_style)
            .with_title_style(&title_style),
            message,
            accept_selected: false,
            callback_handle,
            style,
            theme: editor.theme.clone(),
        }
    }

    fn terminal_action(&self, accepted: bool) -> KeyAction {
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
            Action::NotifyPicker(self.callback_handle, Box::new(callback)),
            Action::CloseDialog,
        ])
    }
}

impl Component for Confirmation {
    fn picker_handle(&self) -> Option<PickerHandle> {
        Some(self.callback_handle)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.dialog.clone();
        self.dialog.style = theme.ui_style.dialog.clone();
        self.dialog.border_draw_style = theme.ui_style.dialog_border.clone();
        self.dialog.title_style = theme.ui_style.dialog_title.clone();
        self.dialog.theme = theme.clone();
        self.theme = theme.clone();
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.dialog.width = confirmation_width(viewport_width, &self.message);
        self.dialog.x = viewport_width.saturating_sub(self.dialog.width + 2) / 2;
        self.dialog.y = viewport_height.saturating_sub(4) / 2;
        true
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let message = truncate_display_width(&self.message, self.dialog.width);
        buffer.set_text(self.dialog.x + 1, self.dialog.y + 1, &message, &self.style);

        let buttons_width = display_width(ACCEPT_LABEL) + BUTTON_GAP + display_width(CANCEL_LABEL);
        let button_x = self.dialog.x + 1 + self.dialog.width.saturating_sub(buttons_width) / 2;
        let selected = self.theme.selected_style(
            &self.style,
            &self.theme.ui_style.picker_selected_item,
            crate::theme::SelectionForegroundPriority::Selection,
        );
        buffer.set_text(
            button_x,
            self.dialog.y + 2,
            ACCEPT_LABEL,
            if self.accept_selected {
                &selected
            } else {
                &self.style
            },
        );
        buffer.set_text(
            button_x + display_width(ACCEPT_LABEL) + BUTTON_GAP,
            self.dialog.y + 2,
            CANCEL_LABEL,
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
                Some(KeyAction::Single(Action::ShowDialog))
            }
            (KeyCode::Right | KeyCode::Tab, _) => {
                self.accept_selected = false;
                Some(KeyAction::Single(Action::ShowDialog))
            }
            (KeyCode::Char('y' | 'Y'), _) => Some(self.terminal_action(true)),
            (KeyCode::Char('n' | 'N'), _) => Some(self.terminal_action(false)),
            (KeyCode::Enter, _) => Some(self.terminal_action(self.accept_selected)),
            _ => None,
        }
    }
}

fn confirmation_width(viewport_width: usize, message: &str) -> usize {
    let desired = display_width(message)
        .max(display_width(ACCEPT_LABEL) + BUTTON_GAP + display_width(CANCEL_LABEL));
    desired.min(60).min(viewport_width.saturating_sub(2)).max(1)
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
}
