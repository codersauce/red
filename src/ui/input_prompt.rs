//! Single-line prompt component with optional masked rendering.
//!
//! [`InputPrompt`] owns text as Unicode graphemes for user-visible cursor motion and
//! returns submission or cancellation actions to the editor. Sensitive prompts mask
//! display and identify themselves through
//! [`Component::is_sensitive_input`] so
//! performance traces do not include their contents.

use crossterm::event::Event;

use crate::{
    config::KeyAction,
    editor::{Action, ComposerCallback, Editor, RenderBuffer},
    keyboard::is_word_backspace,
    plugin::ComposerHandle,
    theme::{Style, Theme},
    unicode_utils::{display_width, grapheme_len, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    first_prompt_line,
    prompt_buffer::PROMPT_MAX_BYTES,
    Component,
};

type SubmitAction = Box<dyn Fn(String) -> Action + Send>;

/// A reusable single-line input dialog. Its initial value starts selected so typing a
/// replacement is one keystroke, while cursor motion and paste remain Unicode-safe.
pub struct InputPrompt {
    dialog: Dialog,
    prompt: vimlet::CommandLine,
    selected: bool,
    masked: bool,
    submit: SubmitAction,
    callback_handle: Option<ComposerHandle>,
    style: Style,
    theme: Theme,
}

impl InputPrompt {
    pub fn new(
        editor: &Editor,
        title: impl Into<String>,
        initial: impl Into<String>,
        submit: impl Fn(String) -> Action + Send + 'static,
    ) -> Self {
        let title = title.into();
        let mut value = first_prompt_line(&initial.into());
        value.truncate(value.floor_char_boundary(PROMPT_MAX_BYTES.min(value.len())));
        let selected = !value.is_empty();
        let prompt = vimlet::CommandLine::new(vimlet::PromptKind::Field, value)
            .expect("single-line initial value");
        let width = editor.vwidth().saturating_sub(2).clamp(1, 60);
        let x = editor.vwidth().saturating_sub(width + 2) / 2;
        let y = editor.vheight().saturating_sub(3) / 2;
        let style = editor.theme.ui_style.dialog.clone();
        Self {
            dialog: Dialog::new(
                Some(title),
                x,
                y,
                width,
                1,
                &style,
                BorderStyle::Single,
                &editor.theme,
            )
            .with_surface_theme(&editor.theme, SurfaceRole::Dialog),
            prompt,
            selected,
            masked: false,
            submit: Box::new(submit),
            callback_handle: None,
            style,
            theme: editor.theme.clone(),
        }
    }

    /// Builds a single-line prompt that masks its contents while preserving paste and
    /// normal editing behavior. Secret values are delivered only on submission.
    pub fn secret(
        editor: &Editor,
        title: impl Into<String>,
        submit: impl Fn(String) -> Action + Send + 'static,
    ) -> Self {
        let mut prompt = Self::new(editor, title, String::new(), submit);
        prompt.masked = true;
        prompt
    }

    /// Builds a plugin-owned single-line prompt using the same callback lifecycle as
    /// [`crate::ui::AgentComposer`].
    pub fn new_callback(
        editor: &Editor,
        title: impl Into<String>,
        initial: impl Into<String>,
        handle: ComposerHandle,
    ) -> Self {
        let mut prompt = Self::new(editor, title, initial, move |value| {
            Action::NotifyComposer(handle, Box::new(ComposerCallback::Submitted(value)))
        });
        prompt.callback_handle = Some(handle);
        prompt
    }
}

impl Component for InputPrompt {
    fn shortcut_context(&self) -> &str {
        self.dialog.title().unwrap_or("Input")
    }
    fn surface_actions(&self) -> Vec<super::UiAction> {
        let mut actions = vec![
            super::UiAction::new("submit", "Enter", "Submit"),
            super::UiAction::new("cancel", "Esc", "Cancel"),
        ];
        actions.extend(super::reference_actions(&[
            ("Editing", "← / → / Home / End", "Move the text cursor"),
            (
                "Editing",
                "Backspace / Delete",
                "Delete previous / next character",
            ),
            ("Editing", "Ctrl+w / Alt+Backspace", "Delete previous word"),
        ]));
        actions
    }

    fn is_sensitive_input(&self) -> bool {
        self.masked
    }

    fn composer_handle(&self) -> Option<ComposerHandle> {
        self.callback_handle
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.dialog.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.theme = theme.clone();
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.dialog.width = viewport_width.saturating_sub(2).clamp(1, 60);
        self.dialog.x = viewport_width.saturating_sub(self.dialog.width + 2) / 2;
        self.dialog.y = viewport_height.saturating_sub(3) / 2;
        true
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let text = self.prompt.text();
        let visible = if self.masked {
            "*".repeat(grapheme_len(text).min(self.dialog.width))
        } else {
            truncate_display_width(text, self.dialog.width)
        };
        let style = if self.selected {
            self.theme.selected_style(
                &self.style,
                &self.theme.ui_style.picker_selected_item,
                crate::theme::SelectionForegroundPriority::Selection,
            )
        } else {
            self.style.clone()
        };
        buffer.set_text(self.dialog.x + 1, self.dialog.y + 1, &visible, &style);
        Ok(())
    }

    fn handle_event(&mut self, ev: &Event) -> Option<KeyAction> {
        use vimlet::{Key, PromptKind, PromptOutcome};
        let key = match ev {
            Event::Key(key)
                if key.kind != crossterm::event::KeyEventKind::Release
                    && is_word_backspace(*key) =>
            {
                Key::Ctrl('w')
            }
            _ => match crate::keyboard::prompt_key(ev)? {
                Key::Ctrl('a') => Key::Home,
                Key::Text(text) => Key::Text(first_prompt_line(&text)),
                key => key,
            },
        };
        let inserted_len = match &key {
            Key::Text(text) => text.len(),
            Key::Char(ch) if !ch.is_control() => ch.len_utf8(),
            _ => 0,
        };
        if inserted_len
            > PROMPT_MAX_BYTES.saturating_sub(if self.selected {
                0
            } else {
                self.prompt.text().len()
            })
        {
            return Some(KeyAction::Single(Action::Refresh));
        }
        if self.selected
            && (inserted_len > 0 || matches!(key, Key::Backspace | Key::Delete | Key::Ctrl('w')))
        {
            self.selected = false;
            self.prompt = vimlet::CommandLine::empty(PromptKind::Field);
        }
        match self.prompt.handle(key) {
            PromptOutcome::Cancelled => Some(self.cancel_action()),
            PromptOutcome::Accepted(value) => {
                let value = value.trim();
                if value.is_empty() {
                    return Some(self.cancel_action());
                }
                let submit = (self.submit)(value.to_owned());
                let actions = if self.callback_handle.is_some() {
                    vec![submit, Action::CloseDialog]
                } else {
                    vec![Action::CloseDialog, submit]
                };
                Some(KeyAction::Multiple(actions))
            }
            PromptOutcome::Edited | PromptOutcome::Moved | PromptOutcome::Consumed => {
                self.selected = false;
                Some(KeyAction::Single(Action::Refresh))
            }
            _ => None,
        }
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let prefix = &self.prompt.text()[..self.prompt.caret().0];
        let offset = if self.masked {
            grapheme_len(prefix)
        } else {
            display_width(prefix)
        }
        .min(self.dialog.width.saturating_sub(1));
        let x = self.dialog.x + 1 + offset;
        Some((x, self.dialog.y + 1))
    }
}

impl InputPrompt {
    fn cancel_action(&self) -> KeyAction {
        if let Some(handle) = self.callback_handle {
            KeyAction::Multiple(vec![
                Action::NotifyComposer(handle, Box::new(ComposerCallback::Cancelled)),
                Action::CloseDialog,
            ])
        } else {
            KeyAction::Single(Action::CloseDialog)
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager, theme::Theme};

    fn editor() -> Editor {
        let config = Config::default();
        Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            50,
            12,
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
    fn word_backspace_respects_selection_cursor_and_secret_input() {
        let editor = editor();
        for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let shortcut = Event::Key(KeyEvent::new(KeyCode::Backspace, modifiers));
            let mut selected = InputPrompt::new(&editor, "Rename", "old name", Action::Print);
            selected.handle_event(&shortcut);
            assert_eq!(selected.prompt.text(), "");
            assert!(!selected.selected);

            let mut prompt = InputPrompt::secret(&editor, "Secret", Action::Print);
            prompt.handle_event(&Event::Paste("one 👨‍👩‍👧e\u{301} rest".into()));
            for _ in 0..5 {
                prompt.handle_event(&key(KeyCode::Left));
            }
            prompt.handle_event(&Event::Key(KeyEvent::new_with_kind(
                KeyCode::Backspace,
                modifiers,
                KeyEventKind::Release,
            )));
            assert_eq!(prompt.prompt.text(), "one 👨‍👩‍👧e\u{301} rest");
            prompt.handle_event(&shortcut);
            assert_eq!(prompt.prompt.text(), "one 👨‍👩‍👧 rest");
            assert_eq!(prompt.prompt.caret().0, "one 👨‍👩‍👧".len());
            assert!(prompt.is_sensitive_input());
            assert!(!format!("{:?}", prompt.prompt).contains("rest"));
        }
    }

    #[test]
    fn first_typed_character_replaces_the_selected_initial_value() {
        let editor = editor();
        let mut prompt = InputPrompt::new(&editor, "Rename symbol", "old_name", Action::Print);

        prompt.handle_event(&key(KeyCode::Char('n')));
        prompt.handle_event(&key(KeyCode::Char('e')));
        let action = prompt.handle_event(&key(KeyCode::Enter));

        assert_eq!(
            action,
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print("ne".to_string())
            ]))
        );
    }

    #[test]
    fn single_line_input_replaces_the_selected_default() {
        let editor = editor();
        let mut prompt = InputPrompt::new(&editor, "Rename symbol", "old", Action::Print);

        prompt.handle_event(&Event::Paste("👨‍👩‍👧name\r\nignored".to_string()));
        prompt.handle_event(&key(KeyCode::Left));
        prompt.handle_event(&key(KeyCode::Backspace));

        assert_eq!(prompt.prompt.text(), "👨‍👩‍👧nae");
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print("👨‍👩‍👧nae".to_string())
            ]))
        );
    }

    #[test]
    fn combining_mark_paste_keeps_input_prompt_cursor_after_merged_grapheme() {
        let editor = editor();
        let mut prompt = InputPrompt::new(&editor, "Rename symbol", "aX", Action::Print);

        prompt.handle_event(&key(KeyCode::Left));
        prompt.handle_event(&Event::Paste("\u{301}".to_string()));

        assert_eq!(prompt.prompt.text(), "a\u{301}X");
        assert_eq!(prompt.prompt.caret().0, 3);

        prompt.handle_event(&key(KeyCode::Char('Z')));

        assert_eq!(prompt.prompt.text(), "a\u{301}ZX");
        assert_eq!(prompt.prompt.caret().0, 4);
    }

    #[test]
    fn paste_is_single_line_and_backspace_removes_one_grapheme() {
        let editor = editor();
        let mut prompt = InputPrompt::new(&editor, "Rename symbol", "old", Action::Print);

        prompt.handle_event(&Event::Paste("👨‍👩‍👧name\nignored".to_string()));
        prompt.handle_event(&key(KeyCode::Backspace));
        let action = prompt.handle_event(&key(KeyCode::Enter));

        assert_eq!(
            action,
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print("👨‍👩‍👧nam".to_string())
            ]))
        );
    }

    #[test]
    fn secret_prompt_masks_pasted_contents_and_submits_the_original_value() {
        let editor = editor();
        let mut prompt = InputPrompt::secret(&editor, "OpenAI API key", Action::Print);
        let secret = "sk-test-secret-that-must-not-be-rendered";

        prompt.handle_event(&Event::Paste(secret.to_string()));
        let mut buffer = RenderBuffer::new(50, 12, &Style::default());
        prompt.draw(&mut buffer).unwrap();
        let rendered = buffer
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();

        assert!(!rendered.contains(secret));
        assert!(rendered.contains("********"));
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print(secret.to_string())
            ]))
        );
    }

    #[test]
    fn escape_and_empty_submission_cancel_without_executing() {
        let editor = editor();
        let mut prompt = InputPrompt::new(&editor, "Rename symbol", "", Action::Print);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Single(Action::CloseDialog))
        );
        let mut prompt = InputPrompt::new(&editor, "Rename symbol", "old", Action::Print);
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Esc)),
            Some(KeyAction::Single(Action::CloseDialog))
        );
    }

    #[test]
    fn callback_input_uses_composer_lifecycle_and_preserves_a_trailing_slash() {
        let editor = editor();
        let handle = ComposerHandle::from_raw(9);
        let mut prompt = InputPrompt::new_callback(&editor, "New path", "", handle);

        prompt.handle_event(&Event::Paste("nested/".to_string()));

        assert_eq!(prompt.composer_handle(), Some(handle));
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::NotifyComposer(
                    handle,
                    Box::new(ComposerCallback::Submitted("nested/".to_string()))
                ),
                Action::CloseDialog,
            ]))
        );

        let mut cancelled = InputPrompt::new_callback(&editor, "New path", "", handle);
        assert_eq!(
            cancelled.handle_event(&key(KeyCode::Esc)),
            Some(KeyAction::Multiple(vec![
                Action::NotifyComposer(handle, Box::new(ComposerCallback::Cancelled)),
                Action::CloseDialog,
            ]))
        );
    }
}
