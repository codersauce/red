//! Compact, reusable confirmation dialog: Accept/Cancel, or a short list of choices.

use std::time::{Duration, Instant};

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
/// A choice dialog that opens unprompted ignores keys this soon after it appears, so
/// keystrokes meant for the buffer cannot pick an option.
const CHOICE_ARMING_DELAY: Duration = Duration::from_millis(350);

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

/// One option of a multi-choice confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmationChoice {
    pub label: String,
    /// Picks this option directly. Leave it out for an option that must be chosen
    /// deliberately with the arrow keys and Enter.
    pub hotkey: Option<char>,
    pub action: Action,
}

impl ConfirmationChoice {
    pub fn new(label: impl Into<String>, hotkey: Option<char>, action: Action) -> Self {
        Self {
            label: label.into(),
            hotkey,
            action,
        }
    }
}

enum ConfirmationTarget {
    Callback(PickerHandle),
    Actions {
        accept: Box<Action>,
        cancel: Box<Action>,
    },
    Choices {
        actions: Vec<Action>,
        hotkeys: Vec<Option<char>>,
        /// Option taken by Esc and Ctrl-C.
        cancel: usize,
    },
}

/// A confirmation surface that defaults to the safe option.
pub struct Confirmation {
    dialog: Dialog,
    message: String,
    rows: Vec<Vec<ConfirmationSegment>>,
    selected: usize,
    target: ConfirmationTarget,
    /// Bracketed button labels, in display order.
    labels: Vec<String>,
    multiline: bool,
    scroll: usize,
    style: Style,
    theme: Theme,
    armed_at: Option<Instant>,
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
            vec![accept_label.to_string(), cancel_label.to_string()],
            1,
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
        Self::with_target(
            editor,
            title.into(),
            message.into(),
            vec![accept_label.into(), cancel_label.into()],
            1,
            true,
            Vec::new(),
            ConfirmationTarget::Actions {
                accept: Box::new(accept),
                cancel: Box::new(cancel),
            },
        )
    }

    /// Creates a multiline dialog with several options. `selected` starts focused and
    /// should be the safe one; `cancel` is taken by Esc. The dialog ignores keys for a
    /// moment after it opens, because it may appear while the user is typing.
    pub fn new_choices(
        editor: &Editor,
        title: impl Into<String>,
        message: impl Into<String>,
        choices: Vec<ConfirmationChoice>,
        selected: usize,
        cancel: usize,
    ) -> Self {
        assert!(
            selected < choices.len() && cancel < choices.len(),
            "choice indexes must name an option"
        );
        let labels = choices
            .iter()
            .map(|choice| match choice.hotkey {
                Some(key) => format!("{key} · {}", choice.label),
                None => choice.label.clone(),
            })
            .collect();
        let hotkeys = choices.iter().map(|choice| choice.hotkey).collect();
        let actions = choices.into_iter().map(|choice| choice.action).collect();
        let mut confirmation = Self::with_target(
            editor,
            title.into(),
            message.into(),
            labels,
            selected,
            true,
            Vec::new(),
            ConfirmationTarget::Choices {
                actions,
                hotkeys,
                cancel,
            },
        );
        confirmation.armed_at = Some(Instant::now() + CHOICE_ARMING_DELAY);
        confirmation
    }

    #[allow(clippy::too_many_arguments)]
    fn with_target(
        editor: &Editor,
        title: String,
        message: String,
        labels: Vec<String>,
        selected: usize,
        multiline: bool,
        rows: Vec<Vec<ConfirmationSegment>>,
        target: ConfirmationTarget,
    ) -> Self {
        let style = editor.theme.ui_style.dialog.clone();
        let labels: Vec<String> = labels
            .into_iter()
            .map(|label| format!("[ {label} ]"))
            .collect();
        let (width, height) = confirmation_size(
            editor.vwidth(),
            editor.vheight(),
            &message,
            &labels,
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
            selected,
            target,
            labels,
            multiline,
            scroll: 0,
            style,
            theme: editor.theme.clone(),
            armed_at: None,
        }
    }

    /// Delays arming, for tests that drive keys immediately.
    #[cfg(test)]
    fn arm_now(&mut self) {
        self.armed_at = None;
    }

    fn cancel_index(&self) -> usize {
        match &self.target {
            ConfirmationTarget::Choices { cancel, .. } => *cancel,
            _ => 1,
        }
    }

    fn terminal_action(&self, index: usize) -> KeyAction {
        match &self.target {
            ConfirmationTarget::Callback(handle) => {
                let callback = if index == 0 {
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
                if index == 0 {
                    accept.as_ref().clone()
                } else {
                    cancel.as_ref().clone()
                },
            ]),
            ConfirmationTarget::Choices { actions, .. } => {
                KeyAction::Multiple(vec![Action::CloseDialog, actions[index].clone()])
            }
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
        let mut actions = vec![
            super::UiAction::new("select", "Enter", "Choose selected option"),
            super::UiAction::new("cancel", "Esc", &self.labels[self.cancel_index()]),
        ];
        match &self.target {
            ConfirmationTarget::Choices { hotkeys, .. } => {
                for (index, key) in hotkeys.iter().enumerate() {
                    if let Some(key) = key {
                        actions.push(super::UiAction::new(
                            format!("choice-{index}"),
                            key.to_string(),
                            &self.labels[index],
                        ));
                    }
                }
            }
            _ => {
                actions.push(super::UiAction::new("accept", "y", &self.labels[0]));
                actions.push(super::UiAction::new("reject", "n", &self.labels[1]));
            }
        }
        actions.push(
            super::UiAction::new("options", "← / → / Tab / Shift+Tab", "Choose an option")
                .with_priority(super::ActionPriority::Reference),
        );
        actions.push(
            super::UiAction::new("scroll", "↑ / ↓", "Scroll the message")
                .with_priority(super::ActionPriority::Reference)
                .with_enabled(self.multiline),
        );
        actions
    }

    fn picker_handle(&self) -> Option<PickerHandle> {
        match &self.target {
            ConfirmationTarget::Callback(handle) => Some(*handle),
            ConfirmationTarget::Actions { .. } | ConfirmationTarget::Choices { .. } => None,
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
            &self.labels,
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

        let selected = self.theme.selected_style(
            &self.style,
            &self.theme.ui_style.picker_selected_item,
            crate::theme::SelectionForegroundPriority::Selection,
        );
        let button_y = self.dialog.y + self.dialog.height;
        let mut button_x = self.dialog.x
            + 1
            + self
                .dialog
                .width
                .saturating_sub(buttons_width(&self.labels))
                / 2;
        for (index, label) in self.labels.iter().enumerate() {
            let label = truncate_display_width(
                label,
                (self.dialog.x + 1 + self.dialog.width).saturating_sub(button_x),
            );
            buffer.set_text(
                button_x,
                button_y,
                &label,
                if index == self.selected {
                    &selected
                } else {
                    &self.style
                },
            );
            button_x += display_width(&label) + BUTTON_GAP;
        }
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else {
            return None;
        };
        let last = self.labels.len() - 1;
        let armed = self.armed_at.is_none_or(|at| Instant::now() >= at);
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                Some(self.terminal_action(self.cancel_index()))
            }
            _ if !armed => Some(KeyAction::Single(Action::Refresh)),
            (KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Char('k'), _) => {
                self.selected = self.selected.saturating_sub(1);
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Right | KeyCode::Tab | KeyCode::Char('j') | KeyCode::Char('l'), _) => {
                self.selected = (self.selected + 1).min(last);
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
            (KeyCode::Enter, _) => Some(self.terminal_action(self.selected)),
            (KeyCode::Char(typed), _) => match &self.target {
                ConfirmationTarget::Choices { hotkeys, .. } => hotkeys
                    .iter()
                    .position(|hotkey| *hotkey == Some(typed))
                    .map(|index| self.terminal_action(index)),
                _ => match typed {
                    'y' | 'Y' => Some(self.terminal_action(0)),
                    'n' | 'N' => Some(self.terminal_action(1)),
                    _ => None,
                },
            },
            _ => None,
        }
    }
}

fn buttons_width(labels: &[String]) -> usize {
    labels
        .iter()
        .map(|label| display_width(label))
        .sum::<usize>()
        + BUTTON_GAP * labels.len().saturating_sub(1)
}

fn confirmation_size(
    viewport_width: usize,
    viewport_height: usize,
    message: &str,
    labels: &[String],
    multiline: bool,
    rows: &[Vec<ConfirmationSegment>],
) -> (usize, usize) {
    let buttons_width = buttons_width(labels);
    let message_width = message.lines().map(display_width).max().unwrap_or_default();
    let rows_width = rows
        .iter()
        .map(|row| row.iter().map(|segment| display_width(&segment.text)).sum())
        .max()
        .unwrap_or_default();
    let desired_width = message_width.max(rows_width).max(buttons_width);
    let max_width = if multiline { 76 } else { 60 };
    let width = desired_width
        .min(max_width.max(buttons_width))
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
            confirmation.selected = 1;
            assert_eq!(
                confirmation.handle_event(&key(key_code)),
                Some(KeyAction::Single(Action::Refresh))
            );
            assert_eq!(confirmation.selected, 0);
        }

        for key_code in [KeyCode::Char('j'), KeyCode::Char('l')] {
            confirmation.selected = 0;
            assert_eq!(
                confirmation.handle_event(&key(key_code)),
                Some(KeyAction::Single(Action::Refresh))
            );
            assert_eq!(confirmation.selected, 1);
        }
    }

    #[test]
    fn choice_dialogs_ignore_early_keys_and_need_a_deliberate_pick_for_unhotkeyed_options() {
        let editor = editor();
        let choice = |label: &str, hotkey, print: &str| {
            ConfirmationChoice::new(label, hotkey, Action::Print(print.to_string()))
        };
        let mut confirmation = Confirmation::new_choices(
            &editor,
            "Unsaved changes",
            "The agent wants to edit a file you have not saved.",
            vec![
                choice("Apply, keep unsaved", Some('u'), "keep"),
                choice("Apply and save", None, "save"),
                choice("Don't apply", Some('n'), "skip"),
            ],
            0,
            2,
        );
        let chose = |print: &str| {
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print(print.to_string()),
            ]))
        };

        assert_eq!(
            confirmation.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Single(Action::Refresh)),
            "a key typed as the dialog appears is swallowed"
        );
        assert_eq!(confirmation.handle_event(&key(KeyCode::Esc)), chose("skip"));

        confirmation.arm_now();
        assert_eq!(confirmation.handle_event(&key(KeyCode::Char('s'))), None);
        assert_eq!(
            confirmation.handle_event(&key(KeyCode::Enter)),
            chose("keep")
        );
        assert_eq!(
            confirmation.handle_event(&key(KeyCode::Char('n'))),
            chose("skip")
        );
        confirmation.handle_event(&key(KeyCode::Tab));
        assert_eq!(
            confirmation.handle_event(&key(KeyCode::Enter)),
            chose("save")
        );
        confirmation.handle_event(&key(KeyCode::Tab));
        confirmation.handle_event(&key(KeyCode::Tab));
        assert_eq!(
            confirmation.selected, 2,
            "navigation stops at the last option"
        );

        let mut buffer = RenderBuffer::new(80, 20, &Style::default());
        confirmation.draw(&mut buffer).unwrap();
        let screen: String = buffer.cells.iter().map(|cell| cell.c).collect();
        for label in [
            "[ u · Apply, keep unsaved ]",
            "[ Apply and save ]",
            "[ n · Don't apply ]",
        ] {
            assert!(screen.contains(label), "{label} missing");
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

        assert_eq!(confirmation.labels[0], "[ Push ]");
        assert_eq!(confirmation.labels[1], "[ Back ]");
        assert!(confirmation.dialog.height > 2);
        assert!(buffer
            .cells
            .iter()
            .any(|cell| cell.c == 'm' && cell.style.fg == Some(accent)));
    }
}
