use std::cmp::min;

use crossterm::event::{Event, KeyCode, KeyEvent};

use crate::{
    color::Color,
    config::KeyAction,
    editor::{Action, RenderBuffer},
    lsp::types::{CompletionItemKind, CompletionResponseItem, Documentation},
};

use super::Component;

const SELECTION_COLOR: Color = Color::Rgb {
    r: 100,
    g: 100,
    b: 100,
};

const COMMENT_COLOR: Color = Color::Rgb {
    r: 128,
    g: 128,
    b: 128,
};

#[derive(Default, Clone)]
pub struct CompletionUI {
    items: Vec<CompletionResponseItem>,
    selected: usize,
    scroll_offset: usize,
    visible: bool,
    x: usize,
    y: usize,
    max_height: usize,
}

impl CompletionUI {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn show(&mut self, items: Vec<CompletionResponseItem>, x: usize, y: usize) {
        self.items = items;
        self.selected = 0;
        self.scroll_offset = 0;
        self.visible = true;
        self.x = x;
        self.y = y;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn selected_item(&self) -> Option<&CompletionResponseItem> {
        self.items.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }

        let new_selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize)
        };

        self.selected = min(new_selected, self.items.len() - 1);

        // Adjust scroll if selection is out of view
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.max_height {
            self.scroll_offset = self.selected - self.max_height + 1;
        }
    }

    fn render_completion(&self) -> Vec<(usize, usize, String, Option<Color>)> {
        if !self.visible || self.items.is_empty() {
            return Vec::new();
        }

        let mut output = Vec::new();
        let visible_items = self
            .items
            .iter()
            .skip(self.scroll_offset)
            .take(self.max_height);
        let mut y_offset = 0;

        // Render completion items
        for (idx, item) in visible_items.enumerate() {
            let is_selected = idx + self.scroll_offset == self.selected;
            let prefix = if is_selected { "> " } else { "  " };

            // Format item with kind if available
            let display = if let Some(kind) = &item.kind {
                format!("{}{} ({})", prefix, item.label, Self::kind_to_string(kind))
            } else {
                format!("{}{}", prefix, item.label)
            };

            output.push((
                self.x,
                self.y + y_offset,
                display,
                if is_selected {
                    Some(SELECTION_COLOR)
                } else {
                    None
                },
            ));
            y_offset += 1;

            // Show detail on the next line if selected
            if is_selected {
                if let Some(detail) = &item.detail {
                    output.push((
                        self.x + 2,
                        self.y + y_offset,
                        detail.clone(),
                        Some(COMMENT_COLOR),
                    ));
                    y_offset += 1;
                }

                // Show documentation if available
                if let Some(doc) = &item.documentation {
                    let doc_text = match doc {
                        Documentation::String(s) => s.clone(),
                        Documentation::MarkupContent(content) => content.value.clone(),
                    };

                    // Split documentation into wrapped lines
                    for line in textwrap::wrap(&doc_text, 60) {
                        output.push((
                            self.x + 2,
                            self.y + y_offset,
                            line.into_owned(),
                            Some(COMMENT_COLOR),
                        ));
                        y_offset += 1;
                    }
                }
            }
        }

        // Show scroll indicators if needed
        if self.scroll_offset > 0 {
            output.push((self.x + 2, self.y, "↑".to_string(), Some(COMMENT_COLOR)));
        }
        if self.scroll_offset + self.max_height < self.items.len() {
            output.push((
                self.x + 2,
                self.y + min(self.max_height, self.items.len()) - 1,
                "↓".to_string(),
                Some(COMMENT_COLOR),
            ));
        }

        output
    }

    fn kind_to_string(kind: &CompletionItemKind) -> &'static str {
        match kind {
            CompletionItemKind::Text => "text",
            CompletionItemKind::Method => "method",
            CompletionItemKind::Function => "fn",
            CompletionItemKind::Constructor => "constructor",
            CompletionItemKind::Field => "field",
            CompletionItemKind::Variable => "var",
            CompletionItemKind::Class => "class",
            CompletionItemKind::Interface => "interface",
            CompletionItemKind::Module => "mod",
            CompletionItemKind::Property => "prop",
            CompletionItemKind::Unit => "unit",
            CompletionItemKind::Value => "value",
            CompletionItemKind::Enum => "enum",
            CompletionItemKind::Keyword => "keyword",
            CompletionItemKind::Snippet => "snippet",
            CompletionItemKind::Color => "color",
            CompletionItemKind::File => "file",
            CompletionItemKind::Reference => "ref",
            CompletionItemKind::Folder => "dir",
            CompletionItemKind::EnumMember => "variant",
            CompletionItemKind::Constant => "const",
            CompletionItemKind::Struct => "struct",
            CompletionItemKind::Event => "event",
            CompletionItemKind::Operator => "op",
            CompletionItemKind::TypeParameter => "type",
        }
    }
}

impl Component for CompletionUI {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        for (x, y, text, color) in self.render_completion() {
            buffer.write_string(x, y, &text, color)?;
        }
        Ok(())
    }

    fn handle_event(&mut self, ev: &Event) -> Option<KeyAction> {
        match ev {
            Event::Key(KeyEvent {
                code: KeyCode::Up, ..
            }) => {
                self.move_selection(-1);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                ..
            }) => {
                self.move_selection(1);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            }) => {
                if let Some(item) = self.selected_item() {
                    let text = if let Some(text_edit) = &item.text_edit {
                        text_edit.new_text.clone()
                    } else if let Some(insert_text) = &item.insert_text {
                        insert_text.clone()
                    } else {
                        item.label.clone()
                    };
                    Some(KeyAction::Multiple(vec![
                        Action::InsertString(text),
                        Action::CloseDialog,
                    ]))
                } else {
                    Some(KeyAction::Single(Action::CloseDialog))
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => Some(KeyAction::Single(Action::CloseDialog)),
            _ => None,
        }
    }
}
