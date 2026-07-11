use crossterm::event::{Event, KeyCode, KeyModifiers};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::{display_width, grapheme_len, grapheme_to_byte, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog},
    Component,
};

type SubmitAction = Box<dyn Fn(String) -> Action + Send>;

/// A reusable single-line input dialog. Its initial value starts selected so typing a
/// replacement is one keystroke, while cursor motion and paste remain Unicode-safe.
pub struct InputPrompt {
    dialog: Dialog,
    value: String,
    cursor: usize,
    selected: bool,
    masked: bool,
    submit: SubmitAction,
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
        let value = initial.into();
        let width = editor.vwidth().saturating_sub(2).clamp(1, 60);
        let x = editor.vwidth().saturating_sub(width + 2) / 2;
        let y = editor.vheight().saturating_sub(3) / 2;
        let style = editor.theme.ui_style.dialog.clone();
        let border_style = editor.theme.ui_style.dialog_border.clone();
        let title_style = editor.theme.ui_style.dialog_title.clone();
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
            .with_border_draw_style(&border_style)
            .with_title_style(&title_style),
            cursor: grapheme_len(&value),
            selected: !value.is_empty(),
            value,
            masked: false,
            submit: Box::new(submit),
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

    fn insert(&mut self, text: &str) {
        let text = text.split(['\r', '\n']).next().unwrap_or_default();
        if self.selected {
            self.value.clear();
            self.cursor = 0;
            self.selected = false;
        }
        let offset = grapheme_to_byte(&self.value, self.cursor);
        self.value.insert_str(offset, text);
        self.cursor += grapheme_len(text);
    }
}

impl Component for InputPrompt {
    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.dialog.clone();
        self.dialog.style = theme.ui_style.dialog.clone();
        self.dialog.border_draw_style = theme.ui_style.dialog_border.clone();
        self.dialog.title_style = theme.ui_style.dialog_title.clone();
        self.dialog.theme = theme.clone();
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
        let visible = if self.masked {
            "*".repeat(grapheme_len(&self.value).min(self.dialog.width))
        } else {
            truncate_display_width(&self.value, self.dialog.width)
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
        match ev {
            Event::Paste(text) => {
                self.insert(text);
                Some(KeyAction::Single(Action::ShowDialog))
            }
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    Some(KeyAction::Single(Action::CloseDialog))
                }
                (KeyCode::Enter, _) => {
                    let value = self.value.trim().to_string();
                    if value.is_empty() {
                        return Some(KeyAction::Single(Action::CloseDialog));
                    }
                    Some(KeyAction::Multiple(vec![
                        Action::CloseDialog,
                        (self.submit)(value),
                    ]))
                }
                (KeyCode::Left, _) => {
                    self.selected = false;
                    self.cursor = self.cursor.saturating_sub(1);
                    Some(KeyAction::Single(Action::ShowDialog))
                }
                (KeyCode::Right, _) => {
                    self.selected = false;
                    self.cursor = (self.cursor + 1).min(grapheme_len(&self.value));
                    Some(KeyAction::Single(Action::ShowDialog))
                }
                (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                    self.selected = false;
                    self.cursor = 0;
                    Some(KeyAction::Single(Action::ShowDialog))
                }
                (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                    self.selected = false;
                    self.cursor = grapheme_len(&self.value);
                    Some(KeyAction::Single(Action::ShowDialog))
                }
                (KeyCode::Backspace, _) => {
                    if self.selected {
                        self.value.clear();
                        self.cursor = 0;
                        self.selected = false;
                    } else if self.cursor > 0 {
                        let start = grapheme_to_byte(&self.value, self.cursor - 1);
                        let end = grapheme_to_byte(&self.value, self.cursor);
                        self.value.replace_range(start..end, "");
                        self.cursor -= 1;
                    }
                    Some(KeyAction::Single(Action::ShowDialog))
                }
                (KeyCode::Delete, _) => {
                    if self.selected {
                        self.value.clear();
                        self.cursor = 0;
                        self.selected = false;
                    } else if self.cursor < grapheme_len(&self.value) {
                        let start = grapheme_to_byte(&self.value, self.cursor);
                        let end = grapheme_to_byte(&self.value, self.cursor + 1);
                        self.value.replace_range(start..end, "");
                    }
                    Some(KeyAction::Single(Action::ShowDialog))
                }
                (KeyCode::Char(character), modifiers)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.insert(&character.to_string());
                    Some(KeyAction::Single(Action::ShowDialog))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let offset = if self.masked {
            self.cursor.min(self.dialog.width.saturating_sub(1))
        } else {
            let prefix = &self.value[..grapheme_to_byte(&self.value, self.cursor)];
            display_width(prefix).min(self.dialog.width.saturating_sub(1))
        };
        let x = self.dialog.x + 1 + offset;
        Some((x, self.dialog.y + 1))
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

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
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Esc)),
            Some(KeyAction::Single(Action::CloseDialog))
        );
    }
}
