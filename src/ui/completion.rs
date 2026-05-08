use std::cmp::min;

use crossterm::event::{Event, KeyCode, KeyEvent};

use crate::{
    color::Color,
    config::KeyAction,
    editor::{Action, RenderBuffer},
    log,
    lsp::types::{CompletionItemKind, CompletionResponseItem, Documentation, InsertTextFormat},
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
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
const BORDER_COLOR: Color = Color::Rgb {
    r: 80,
    g: 80,
    b: 80,
};
const DEPRECATED_COLOR: Color = Color::Rgb { r: 128, g: 0, b: 0 };
const MAX_WIDTH: usize = 80;
const PAGE_SIZE: usize = 10;

#[derive(Default, Clone)]
pub struct CompletionUI {
    items: Vec<CompletionResponseItem>,
    selected: usize,
    scroll_offset: usize,
    visible: bool,
    x: usize,
    y: usize,
    max_height: usize,
    width: usize,
    commit_chars: Vec<char>,
}

impl CompletionUI {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn show(&mut self, mut items: Vec<CompletionResponseItem>, x: usize, y: usize) {
        // Collect commit characters from all items
        self.commit_chars = items
            .iter()
            .filter_map(|item| item.commit_characters.as_ref())
            .flat_map(|chars| chars.iter())
            .filter_map(|s| s.chars().next())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        // Sort items by preselect and label
        items.sort_by(|a, b| {
            b.preselect
                .unwrap_or(false)
                .cmp(&a.preselect.unwrap_or(false))
                .then(a.label.cmp(&b.label))
        });

        // Find first preselected item or default to 0
        let selected = items
            .iter()
            .position(|item| item.preselect.unwrap_or(false))
            .unwrap_or(0);

        self.items = items;
        self.selected = selected;
        self.scroll_offset = 0;
        self.visible = true;
        self.x = x;
        self.y = y;
        self.width = self.calculate_width();
        self.max_height = min(self.items.len(), PAGE_SIZE);
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

    fn calculate_width(&self) -> usize {
        let max_item_width = self
            .items
            .iter()
            .map(|item| {
                let kind_width = item
                    .kind
                    .as_ref()
                    .map_or(0, |kind| display_width(Self::kind_to_icon(kind)) + 1);
                display_width(&item.label) + kind_width + 4 // +4 for prefix and padding
            })
            .max()
            .unwrap_or(20);

        max_item_width.clamp(40, MAX_WIDTH)
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

    fn move_page(&mut self, up: bool) {
        let delta = if up {
            -(PAGE_SIZE as isize)
        } else {
            PAGE_SIZE as isize
        };
        self.move_selection(delta);
    }

    fn kind_to_icon(kind: &CompletionItemKind) -> &'static str {
        match kind {
            CompletionItemKind::Text => "abc",
            CompletionItemKind::Method => "ƒ",
            CompletionItemKind::Function => "λ",
            CompletionItemKind::Constructor => "⚡",
            CompletionItemKind::Field => "◆",
            CompletionItemKind::Variable => "𝑥",
            CompletionItemKind::Class => "○",
            CompletionItemKind::Interface => "◌",
            CompletionItemKind::Module => "□",
            CompletionItemKind::Property => "◇",
            CompletionItemKind::Unit => "∅",
            CompletionItemKind::Value => "=",
            CompletionItemKind::Enum => "ℰ",
            CompletionItemKind::Keyword => "🔑",
            CompletionItemKind::Snippet => "✂",
            CompletionItemKind::Color => "🎨",
            CompletionItemKind::File => "📄",
            CompletionItemKind::Reference => "→",
            CompletionItemKind::Folder => "📁",
            CompletionItemKind::EnumMember => "ℯ",
            CompletionItemKind::Constant => "π",
            CompletionItemKind::Struct => "⚪",
            CompletionItemKind::Event => "⚡",
            CompletionItemKind::Operator => "±",
            CompletionItemKind::TypeParameter => "𝑇",
        }
    }

    fn row(content: &str, width: usize) -> String {
        format!("│{}│", fit_display_width(content, width.saturating_sub(2)))
    }

    fn ellipsize(content: &str, width: usize) -> String {
        if display_width(content) <= width {
            return fit_display_width(content, width);
        }

        if width <= 3 {
            return ".".repeat(width);
        }

        let mut truncated = truncate_display_width(content, width - 3);
        truncated.push_str("...");
        fit_display_width(&truncated, width)
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
        let mut y_offset = 1;

        // Draw top border
        output.push((
            self.x,
            self.y + y_offset,
            format!("╭{}╮", "─".repeat(self.width - 2)),
            Some(BORDER_COLOR),
        ));
        y_offset += 1;

        log!(
            "[ui] CompletionUI::render_completion: width={}, height={}",
            self.width,
            self.max_height
        );

        // Render completion items
        for (idx, item) in visible_items.enumerate() {
            let is_selected = idx + self.scroll_offset == self.selected;
            let prefix = if is_selected { ">" } else { " " };

            // Format item with icon and handle deprecated items
            let is_deprecated = item.deprecated.unwrap_or(false);

            let display = if let Some(kind) = &item.kind {
                format!(
                    "{}{} {} {}",
                    prefix,
                    Self::kind_to_icon(kind),
                    if is_deprecated { "⚠ " } else { "" },
                    item.label
                )
            } else {
                format!(
                    "{}  {} {}",
                    prefix,
                    if is_deprecated { "⚠ " } else { "" },
                    item.label
                )
            };

            let display = Self::ellipsize(&display, self.width.saturating_sub(2));

            output.push((
                self.x,
                self.y + y_offset,
                format!("│{display}│"),
                if is_deprecated {
                    Some(DEPRECATED_COLOR)
                } else if is_selected {
                    Some(SELECTION_COLOR)
                } else {
                    None
                },
            ));
            y_offset += 1;

            // Show detail and documentation for selected item
            if is_selected {
                if let Some(detail) = &item.detail {
                    output.push((
                        self.x,
                        self.y + y_offset,
                        Self::row(&format!("  {}", detail), self.width),
                        Some(COMMENT_COLOR),
                    ));
                    y_offset += 1;
                }

                // Show commit characters if available
                if let Some(chars) = &item.commit_characters {
                    let commit_text = format!(
                        "│  Complete with: {}",
                        chars
                            .iter()
                            .map(|c| format!("'{}'", c))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    output.push((
                        self.x,
                        self.y + y_offset,
                        Self::row(&commit_text, self.width),
                        Some(COMMENT_COLOR),
                    ));
                    y_offset += 1;
                }

                if let Some(doc) = &item.documentation {
                    let doc_text = match doc {
                        Documentation::String(s) => s.clone(),
                        Documentation::MarkupContent(content) => content.value.clone(),
                    };

                    // Add separator line
                    output.push((
                        self.x,
                        self.y + y_offset,
                        format!("│{}│", "─".repeat(self.width - 2)),
                        Some(BORDER_COLOR),
                    ));
                    y_offset += 1;

                    // Split documentation into wrapped lines
                    for line in textwrap::wrap(&doc_text, self.width - 4) {
                        output.push((
                            self.x,
                            self.y + y_offset,
                            Self::row(&format!("  {}", line), self.width),
                            Some(COMMENT_COLOR),
                        ));
                        y_offset += 1;
                    }
                }
            }
        }

        // Draw bottom border
        output.push((
            self.x,
            self.y + y_offset,
            format!("╰{}╯", "─".repeat(self.width - 2)),
            Some(BORDER_COLOR),
        ));

        // Show scroll indicators
        if self.scroll_offset > 0 {
            output.push((self.x + 2, self.y + 1, "↑".to_string(), Some(COMMENT_COLOR)));
        }
        if self.scroll_offset + self.max_height < self.items.len() {
            output.push((
                self.x + 2,
                self.y + y_offset - 1,
                "↓".to_string(),
                Some(COMMENT_COLOR),
            ));
        }

        output
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
                code: KeyCode::PageUp,
                ..
            }) => {
                self.move_page(true);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::PageDown,
                ..
            }) => {
                self.move_page(false);
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
                        match item.insert_text_format {
                            Some(InsertTextFormat::Snippet) => {
                                // TODO: Implement snippet parsing and expansion
                                // For now, just insert the text as-is
                                insert_text.clone()
                            }
                            _ => insert_text.clone(),
                        }
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
            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) if self.commit_chars.contains(c) => {
                if let Some(item) = self.selected_item() {
                    let text = if let Some(text_edit) = &item.text_edit {
                        text_edit.new_text.clone()
                    } else if let Some(insert_text) = &item.insert_text {
                        match item.insert_text_format {
                            Some(InsertTextFormat::Snippet) => insert_text.clone(),
                            _ => insert_text.clone(),
                        }
                    } else {
                        item.label.clone()
                    };

                    Some(KeyAction::Multiple(vec![
                        Action::InsertString(text),
                        Action::InsertString(c.to_string()),
                        Action::CloseDialog,
                    ]))
                } else {
                    Some(KeyAction::Single(Action::CloseDialog))
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode_utils::display_width;

    fn item(label: &str, kind: Option<CompletionItemKind>) -> CompletionResponseItem {
        CompletionResponseItem {
            label: label.to_string(),
            kind,
            detail: None,
            documentation: None,
            deprecated: None,
            preselect: None,
            sort_text: None,
            filter_text: None,
            insert_text: None,
            insert_text_format: None,
            text_edit: None,
            additional_text_edits: None,
            command: None,
            data: None,
            commit_characters: None,
        }
    }

    #[test]
    fn completion_rows_fit_display_width_with_wide_labels() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![item(
                "function_with_emoji_👋_and_cjk_世界_that_must_truncate",
                Some(CompletionItemKind::Function),
            )],
            0,
            0,
        );

        let rows = ui.render_completion();

        for (_, _, row, _) in rows {
            assert_eq!(display_width(&row), ui.width);
            assert!(row.is_char_boundary(row.len()));
        }
    }

    #[test]
    fn completion_rows_pad_detail_by_display_width() {
        let mut completion = item("hello", Some(CompletionItemKind::Text));
        completion.detail = Some("returns 👋 世界".to_string());

        let mut ui = CompletionUI::new();
        ui.show(vec![completion], 0, 0);

        let rows = ui.render_completion();

        assert!(rows
            .iter()
            .any(|(_, _, row, _)| row.contains("returns 👋 世界")));
        for (_, _, row, _) in rows {
            assert_eq!(display_width(&row), ui.width);
        }
    }
}
