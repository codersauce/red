//! Compact, reusable Accept/Cancel confirmation dialog.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde::Deserialize;
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfirmationOptions {
    pub accept_label: Option<String>,
    pub cancel_label: Option<String>,
    pub rows: Vec<Vec<ConfirmationSegment>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationSegment {
    pub text: String,
    #[serde(default)]
    pub style: Style,
}

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
    rows: Vec<Vec<ConfirmationSegment>>,
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
        Self::new_callback_with_options(
            editor,
            title,
            message,
            callback_handle,
            ConfirmationOptions::default(),
        )
    }

    pub fn new_callback_with_options(
        editor: &Editor,
        title: impl Into<String>,
        message: impl Into<String>,
        callback_handle: PickerHandle,
        options: ConfirmationOptions,
    ) -> Self {
        let accept_label = options.accept_label.as_deref().unwrap_or("Accept");
        let cancel_label = options.cancel_label.as_deref().unwrap_or("Cancel");
        let multiline = !options.rows.is_empty();
        Self::with_target(
            editor,
            title.into(),
            message.into(),
            accept_label,
            cancel_label,
            multiline,
            options.rows,
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
            Vec::new(),
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
        rows: Vec<Vec<ConfirmationSegment>>,
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
            &rows,
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
            rows,
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

    fn body_rows(&self) -> Vec<Vec<ConfirmationSegment>> {
        let mut rows = if self.multiline {
            wrap_text(&self.message, self.dialog.width)
                .rows
                .into_iter()
                .map(|text| {
                    vec![ConfirmationSegment {
                        text,
                        style: self.style.clone(),
                    }]
                })
                .collect()
        } else {
            vec![vec![ConfirmationSegment {
                text: truncate_display_width(&self.message, self.dialog.width),
                style: self.style.clone(),
            }]]
        };
        rows.extend(self.rows.clone());
        rows
    }

    fn body_height(&self) -> usize {
        self.dialog.height.saturating_sub(1)
    }

    fn max_scroll(&self) -> usize {
        self.body_rows().len().saturating_sub(self.body_height())
    }
}

impl Component for Confirmation {
    fn shortcut_context(&self) -> &str {
        self.dialog.title().unwrap_or("Confirmation")
    }
    fn surface_actions(&self) -> Vec<super::UiAction> {
        vec![
            super::UiAction::new("select", "Enter", "Choose selected option"),
            super::UiAction::new("cancel", "Esc", "Cancel"),
            super::UiAction::new("accept", "y", &self.accept_label),
            super::UiAction::new("reject", "n", &self.cancel_label),
            super::UiAction::new("options", "← / → / Tab / Shift+Tab", "Choose an option")
                .with_priority(super::ActionPriority::Reference),
            super::UiAction::new("scroll", "↑ / ↓", "Scroll the message")
                .with_priority(super::ActionPriority::Reference)
                .with_enabled(self.multiline),
        ]
    }

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
            &self.rows,
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
            let mut x = self.dialog.x + 1;
            let mut remaining = self.dialog.width;
            for segment in row {
                if remaining == 0 {
                    break;
                }
                let text = truncate_display_width(&segment.text, remaining);
                let width = display_width(&text);
                let mut style = segment.style.clone();
                style.fg = style.fg.or(self.style.fg);
                style.bg = self.style.bg;
                buffer.set_text(x, self.dialog.y + 1 + offset, &text, &style);
                x = x.saturating_add(width);
                remaining = remaining.saturating_sub(width);
            }
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
            (KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Char('k'), _) => {
                self.accept_selected = true;
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Right | KeyCode::Tab | KeyCode::Char('j') | KeyCode::Char('l'), _) => {
                self.accept_selected = false;
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Up, _) if self.multiline => {
                self.scroll = self.scroll.saturating_sub(1);
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Down, _) if self.multiline => {
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
    rows: &[Vec<ConfirmationSegment>],
) -> (usize, usize) {
    let buttons_width = display_width(accept_label) + BUTTON_GAP + display_width(cancel_label);
    let message_width = message.lines().map(display_width).max().unwrap_or_default();
    let rows_width = rows
        .iter()
        .map(|row| row.iter().map(|segment| display_width(&segment.text)).sum())
        .max()
        .unwrap_or_default();
    let desired_width = message_width.max(rows_width).max(buttons_width);
    let max_width = if multiline { 76 } else { 60 };
    let width = desired_width
        .min(max_width)
        .min(viewport_width.saturating_sub(2))
        .max(1);
    if !multiline {
        return (width, 2.min(viewport_height.saturating_sub(2)));
    }
    let body_rows = wrap_text(message, width)
        .rows
        .len()
        .max(1)
        .saturating_add(rows.len());
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
    use crate::{buffer::Buffer, color::Color, config::Config, lsp::LspManager};

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
    fn confirmation_buttons_support_vim_navigation() {
        let editor = editor();
        let mut confirmation = Confirmation::new_callback(
            &editor,
            "Delete file?",
            "This cannot be undone.",
            PickerHandle::from_raw(7),
        );

        for key_code in [KeyCode::Char('h'), KeyCode::Char('k')] {
            confirmation.accept_selected = false;
            assert_eq!(
                confirmation.handle_event(&key(key_code)),
                Some(KeyAction::Single(Action::Refresh))
            );
            assert!(confirmation.accept_selected);
        }

        for key_code in [KeyCode::Char('j'), KeyCode::Char('l')] {
            confirmation.accept_selected = true;
            assert_eq!(
                confirmation.handle_event(&key(key_code)),
                Some(KeyAction::Single(Action::Refresh))
            );
            assert!(!confirmation.accept_selected);
        }
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

    #[test]
    fn callback_confirmation_renders_structured_rows_and_custom_labels() {
        let editor = editor();
        let accent = Color::Rgb {
            r: 24,
            g: 180,
            b: 90,
        };
        let confirmation = Confirmation::new_callback_with_options(
            &editor,
            "Push changes",
            "Review outgoing commits.",
            PickerHandle::from_raw(3),
            ConfirmationOptions {
                accept_label: Some("Push".to_string()),
                cancel_label: Some("Back".to_string()),
                rows: vec![vec![
                    ConfirmationSegment {
                        text: "main".to_string(),
                        style: Style {
                            fg: Some(accent),
                            ..Style::default()
                        },
                    },
                    ConfirmationSegment {
                        text: " → origin/main".to_string(),
                        style: Style::default(),
                    },
                ]],
            },
        );
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());

        confirmation.draw(&mut buffer).unwrap();

        assert_eq!(confirmation.accept_label, "[ Push ]");
        assert_eq!(confirmation.cancel_label, "[ Back ]");
        assert!(confirmation.dialog.height > 2);
        assert!(buffer
            .cells
            .iter()
            .any(|cell| cell.c == 'm' && cell.style.fg == Some(accent)));
    }
}
