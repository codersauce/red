//! Compact, reusable Accept/Cancel confirmation with complete wrapped impact details.

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
    dialog::{BorderStyle, Dialog, SurfaceRole},
    wrap_text, Component, PickerItem,
};

const ACCEPT_LABEL: &str = "[ Accept ]";
const CANCEL_LABEL: &str = "[ Cancel ]";
const BUTTON_GAP: usize = 2;

/// A compact, wrapped confirmation surface that defaults to the safe Cancel action.
pub struct Confirmation {
    dialog: Dialog,
    message: String,
    message_rows: Vec<String>,
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
        let width = confirmation_width(editor.vwidth(), &message);
        let message_rows = confirmation_message_rows(&message, width);
        let height = confirmation_height(editor.vheight(), message_rows.len());
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
            message_rows,
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
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.theme = theme.clone();
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.dialog.width = confirmation_width(viewport_width, &self.message);
        self.message_rows = confirmation_message_rows(&self.message, self.dialog.width);
        self.dialog.height = confirmation_height(viewport_height, self.message_rows.len());
        self.dialog.x = viewport_width.saturating_sub(self.dialog.width + 2) / 2;
        self.dialog.y = viewport_height.saturating_sub(self.dialog.height + 2) / 2;
        true
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let visible_message_rows = self.dialog.height.saturating_sub(1);
        for (index, row) in self
            .message_rows
            .iter()
            .take(visible_message_rows)
            .enumerate()
        {
            let message = if index + 1 == visible_message_rows
                && self.message_rows.len() > visible_message_rows
            {
                let mut clipped = truncate_display_width(row, self.dialog.width.saturating_sub(1));
                if self.dialog.width > 0 {
                    clipped.push('…');
                }
                clipped
            } else {
                truncate_display_width(row, self.dialog.width)
            };
            buffer.set_text(
                self.dialog.x + 1,
                self.dialog.y + 1 + index,
                &message,
                &self.style,
            );
        }

        let buttons_width = display_width(ACCEPT_LABEL) + BUTTON_GAP + display_width(CANCEL_LABEL);
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
            ACCEPT_LABEL,
            if self.accept_selected {
                &selected
            } else {
                &self.style
            },
        );
        buffer.set_text(
            button_x + display_width(ACCEPT_LABEL) + BUTTON_GAP,
            button_y,
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
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Right | KeyCode::Tab, _) => {
                self.accept_selected = false;
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Char('y' | 'Y'), _) => Some(self.terminal_action(true)),
            (KeyCode::Char('n' | 'N'), _) => Some(self.terminal_action(false)),
            (KeyCode::Enter, _) => Some(self.terminal_action(self.accept_selected)),
            _ => None,
        }
    }
}

fn confirmation_message_rows(message: &str, width: usize) -> Vec<String> {
    let mut rows = wrap_text(message, width.max(1)).rows;
    if !message.ends_with('\n') && rows.len() > 1 && rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

fn confirmation_height(viewport_height: usize, message_rows: usize) -> usize {
    message_rows
        .saturating_add(1)
        .min(viewport_height.saturating_sub(2).max(2))
        .max(2)
}

fn confirmation_width(viewport_width: usize, message: &str) -> usize {
    let desired = message
        .lines()
        .map(display_width)
        .max()
        .unwrap_or_default()
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

    fn rendered_rows(buffer: &RenderBuffer) -> Vec<String> {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
            .collect()
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
    fn confirmation_stays_compact_for_a_short_single_line_message() {
        let editor = editor();
        let confirmation = Confirmation::new_callback(
            &editor,
            "Delete?",
            "This cannot be undone.",
            PickerHandle::from_raw(1),
        );
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());

        confirmation.draw(&mut buffer).unwrap();

        assert_eq!(confirmation.dialog.height, 2);
        assert!(confirmation.dialog.width <= 60);
        assert!(rendered_rows(&buffer)
            .iter()
            .any(|row| row.contains("This cannot be undone.")));
    }

    #[test]
    fn confirmation_wraps_long_messages_without_silently_hiding_their_impact() {
        let editor = editor();
        let message = "A very long explanation that should stay inside a compact dialog while keeping the complete operation and its consequences visible.";
        let confirmation =
            Confirmation::new_callback(&editor, "Confirm?", message, PickerHandle::from_raw(2));
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());

        confirmation.draw(&mut buffer).unwrap();

        assert!(confirmation.dialog.height > 2);
        assert!(confirmation.dialog.width <= 60);
        assert_eq!(confirmation.message_rows.concat(), message);
        let rendered = rendered_rows(&buffer).join("\n");
        assert!(rendered.contains("[ Accept ]"));
        assert!(rendered.contains("[ Cancel ]"));
    }

    #[test]
    fn confirmation_displays_the_full_replay_branch_worktree_and_safety_boundary() {
        let editor = editor();
        let path = "/Users/felipe.coury/code/red.replay-pr-145-781649e";
        let message = format!(
            "Create local branch replay/pr-145-781649e at the original merge base?\n\n{path}\n\nYour current branch is unchanged. Replay never saves, commits, pushes, or submits a review."
        );
        let confirmation = Confirmation::new_callback(
            &editor,
            "Create Replay scratch worktree?",
            message,
            PickerHandle::from_raw(3),
        );
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());

        confirmation.draw(&mut buffer).unwrap();

        let rendered = rendered_rows(&buffer).join("\n");
        assert!(rendered.contains("replay/pr-145-781649e"));
        assert!(rendered.contains(path));
        assert!(rendered.contains("Your current branch is unchanged."));
        assert!(rendered.contains("[ Accept ]"));
        assert!(rendered.contains("[ Cancel ]"));
    }

    #[test]
    fn confirmation_rewraps_on_resize_and_marks_hidden_impact() {
        let editor = editor();
        let mut confirmation = Confirmation::new_callback(
            &editor,
            "Confirm?",
            "First important impact\nSecond important impact\nThird important impact\nFourth important impact",
            PickerHandle::from_raw(4),
        );

        assert!(confirmation.resize(/*viewport_width*/ 30, /*viewport_height*/ 5));
        let mut buffer = RenderBuffer::new(30, 5, &Style::default());
        confirmation.draw(&mut buffer).unwrap();

        assert!(confirmation.dialog.width <= 28);
        assert!(confirmation.dialog.height <= 3);
        let rendered = rendered_rows(&buffer).join("\n");
        assert!(rendered.contains('…'));
        assert!(rendered.contains("[ Accept ]"));
        assert!(rendered.contains("[ Cancel ]"));
    }
}
