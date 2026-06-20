use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog},
    Component,
};

const DEFAULT_WIDTH: usize = 64;
const MIN_WIDTH: usize = 24;
const CONTENT_HEIGHT: usize = 2;

/// Configuration for a cursor-anchored text prompt opened by a plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PromptConfig {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub initial_text: String,
}

/// A small text prompt positioned near the active editor cursor.
pub struct Prompt {
    id: String,
    anchor: (usize, usize),
    x: usize,
    y: usize,
    width: usize,
    input: String,
    cursor: usize,
    placeholder: Option<String>,
    context: Option<String>,
    dialog: Dialog,
    theme: Theme,
}

impl Prompt {
    pub fn new(id: String, config: PromptConfig, editor: &Editor) -> Self {
        let anchor = editor.cursor_position();
        let (x, y, width) = prompt_geometry(anchor, editor.vwidth(), editor.vheight());
        let style = editor.theme.ui_style.dialog.clone();
        let border_style = editor.theme.ui_style.dialog_border.clone();
        let title_style = editor.theme.ui_style.dialog_title.clone();
        let cursor = config.initial_text.chars().count();
        let dialog = Dialog::new(
            config.title.or_else(|| Some("Prompt".to_string())),
            x,
            y,
            width,
            CONTENT_HEIGHT,
            &style,
            BorderStyle::Single,
            &editor.theme,
        )
        .with_border_draw_style(&border_style)
        .with_title_style(&title_style);

        Self {
            id,
            anchor,
            x,
            y,
            width,
            input: config.initial_text,
            cursor,
            placeholder: config.placeholder,
            context: config.context,
            dialog,
            theme: editor.theme.clone(),
        }
    }

    fn resize_to_viewport(&mut self, viewport_width: usize, viewport_height: usize) {
        let (x, y, width) = prompt_geometry(self.anchor, viewport_width, viewport_height);
        self.x = x;
        self.y = y;
        self.width = width;
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
    }

    fn insert_text(&mut self, text: &str) {
        let byte_index = char_to_byte(&self.input, self.cursor);
        self.input.insert_str(byte_index, text);
        self.cursor += text.chars().count();
    }

    fn delete_previous_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = char_to_byte(&self.input, self.cursor - 1);
        let end = char_to_byte(&self.input, self.cursor);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete_next_char(&mut self) {
        let count = self.input.chars().count();
        if self.cursor >= count {
            return;
        }
        let start = char_to_byte(&self.input, self.cursor);
        let end = char_to_byte(&self.input, self.cursor + 1);
        self.input.replace_range(start..end, "");
    }

    fn submitted_action(&self) -> KeyAction {
        KeyAction::Multiple(vec![
            Action::NotifyPlugins(
                format!("prompt:submitted:{}", self.id),
                json!({ "text": self.input }),
            ),
            Action::CloseDialog,
        ])
    }

    fn cancelled_action(&self) -> KeyAction {
        KeyAction::Multiple(vec![
            Action::NotifyPlugins(format!("prompt:cancelled:{}", self.id), json!(null)),
            Action::CloseDialog,
        ])
    }

    fn input_window(&self, max_width: usize) -> (String, usize) {
        if max_width == 0 {
            return (String::new(), 0);
        }

        let cursor_byte = char_to_byte(&self.input, self.cursor);
        let mut start = 0;
        while display_width(&self.input[start..cursor_byte]) >= max_width {
            let Some((offset, _)) = self.input[start..cursor_byte].char_indices().nth(1) else {
                break;
            };
            start += offset;
        }

        let visible = truncate_display_width(&self.input[start..], max_width);
        let cursor_x = display_width(&self.input[start..cursor_byte]).min(max_width);
        (visible, cursor_x)
    }
}

impl Component for Prompt {
    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.resize_to_viewport(viewport_width, viewport_height);
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.dialog.style = theme.ui_style.dialog.clone();
        self.dialog.border_draw_style = theme.ui_style.dialog_border.clone();
        self.dialog.title_style = theme.ui_style.dialog_title.clone();
        self.dialog.theme = theme.clone();
        self.theme = theme.clone();
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        match event {
            Event::Paste(text) => {
                let pasted = text
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\n', " ");
                self.insert_text(&pasted);
                Some(KeyAction::Single(Action::Refresh))
            }
            Event::Key(event) => match event.code {
                KeyCode::Esc => Some(self.cancelled_action()),
                KeyCode::Enter => {
                    if self.input.trim().is_empty() {
                        None
                    } else {
                        Some(self.submitted_action())
                    }
                }
                KeyCode::Backspace => {
                    self.delete_previous_char();
                    Some(KeyAction::Single(Action::Refresh))
                }
                KeyCode::Delete => {
                    self.delete_next_char();
                    Some(KeyAction::Single(Action::Refresh))
                }
                KeyCode::Left => {
                    self.cursor = self.cursor.saturating_sub(1);
                    Some(KeyAction::Single(Action::Refresh))
                }
                KeyCode::Right => {
                    self.cursor = (self.cursor + 1).min(self.input.chars().count());
                    Some(KeyAction::Single(Action::Refresh))
                }
                KeyCode::Home => {
                    self.cursor = 0;
                    Some(KeyAction::Single(Action::Refresh))
                }
                KeyCode::End => {
                    self.cursor = self.input.chars().count();
                    Some(KeyAction::Single(Action::Refresh))
                }
                KeyCode::Char(c)
                    if event.modifiers == KeyModifiers::NONE
                        || event.modifiers == KeyModifiers::SHIFT =>
                {
                    self.insert_text(&c.to_string());
                    Some(KeyAction::Single(Action::Refresh))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let context = self.context.as_deref().unwrap_or_default();
        buffer.set_text(
            self.x + 1,
            self.y + 1,
            &fit_display_width(context, self.width),
            &self.theme.ui_style.muted,
        );

        let input_width = self.width.saturating_sub(2);
        let (visible, _) = self.input_window(input_width);
        let input = if visible.is_empty() {
            self.placeholder.as_deref().unwrap_or_default()
        } else {
            &visible
        };
        let style: &Style = if visible.is_empty() {
            &self.theme.ui_style.muted
        } else {
            &self.theme.ui_style.dialog
        };
        buffer.set_text(
            self.x + 2,
            self.y + 2,
            &fit_display_width(input, input_width),
            style,
        );
        buffer.set_text(
            self.x + 1,
            self.y + 2,
            ">",
            &self.theme.ui_style.picker_prompt,
        );
        Ok(())
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let input_width = self.width.saturating_sub(2);
        let (_, cursor_x) = self.input_window(input_width);
        Some((self.x + 2 + cursor_x, self.y + 2))
    }
}

fn prompt_geometry(
    anchor: (usize, usize),
    viewport_width: usize,
    viewport_height: usize,
) -> (usize, usize, usize) {
    let width = DEFAULT_WIDTH
        .min(viewport_width.saturating_sub(2))
        .max(MIN_WIDTH.min(viewport_width));
    let outer_height = CONTENT_HEIGHT + 2;
    let x = anchor
        .0
        .min(viewport_width.saturating_sub(width.saturating_add(2)));
    let below = anchor.1.saturating_add(1);
    let y = if below.saturating_add(outer_height) <= viewport_height.saturating_sub(2) {
        below
    } else {
        anchor.1.saturating_sub(outer_height)
    };
    (x, y, width)
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};

    fn test_editor() -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, String::new());
        Editor::with_size(lsp, 80, 24, config, Theme::default(), vec![buffer])
            .expect("test editor should initialize")
    }

    #[test]
    fn prompt_submits_text() {
        let editor = test_editor();
        let mut prompt = Prompt::new("ask".to_string(), PromptConfig::default(), &editor);
        prompt.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));

        let action = prompt.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        assert!(matches!(action, Some(KeyAction::Multiple(_))));
    }

    #[test]
    fn prompt_paste_keeps_single_line() {
        let editor = test_editor();
        let mut prompt = Prompt::new("ask".to_string(), PromptConfig::default(), &editor);

        prompt.handle_event(&Event::Paste("one\r\ntwo".to_string()));

        assert_eq!(prompt.input, "one two");
    }
}
