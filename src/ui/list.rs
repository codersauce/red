use crate::{editor::RenderBuffer, theme::Style};

use super::Component;

pub struct List {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    items: Vec<String>,
    item_style: Style,
    selected_item_style: Style,
    selected_item: usize,
    top_index: usize,
}

impl List {
    pub fn new(
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        items: Vec<String>,
        item_style: &Style,
        selected_item_style: &Style,
    ) -> Self {
        let items = items.iter().map(|s| truncate(s, width)).collect();
        List {
            x,
            y,
            width,
            height,
            items,
            item_style: item_style.clone(),
            selected_item_style: selected_item_style.clone(),
            selected_item: 0,
            top_index: 0,
        }
    }

    pub fn move_down(&mut self) {
        self.selected_item += 1;
        if self.selected_item > self.items.len() - 1 {
            self.selected_item = self.items.len() - 1;
            return;
        }
        if self.top_index + self.selected_item > self.height - 1 {
            self.top_index += 1;
        }
    }

    pub(crate) fn move_up(&mut self) {
        self.selected_item = self.selected_item.saturating_sub(1);
        if self.selected_item < self.top_index {
            self.top_index = self.selected_item;
        }
    }

    pub fn selected_item(&self) -> String {
        self.items[self.selected_item].clone()
    }

    pub fn set_items(&mut self, new_items: Vec<String>) {
        self.selected_item = 0;
        self.top_index = 0;
        self.items = new_items;
    }

    pub fn items(&self) -> &Vec<String> {
        &self.items
    }
}

impl Component for List {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        for (i, y) in (self.y..self.y + self.height).enumerate() {
            if let Some(item) = self.items.get(y - self.y + self.top_index) {
                let style = if self.selected_item == self.top_index + i {
                    &self.selected_item_style
                } else {
                    &self.item_style
                };
                let line = format!(" {:<width$}", item, width = self.width - 1);
                buffer.set_text(self.x, y, &line, style);
            }
        }

        Ok(())
    }

    fn handle_event(&mut self, ev: &crossterm::event::Event) -> Option<crate::config::KeyAction> {
        match ev {
            crossterm::event::Event::Key(event) => match event.code {
                crossterm::event::KeyCode::Esc => Some(crate::config::KeyAction::Single(
                    crate::editor::Action::CloseDialog,
                )),
                _ => None,
            },
            crossterm::event::Event::Mouse(ev) => match ev {
                crossterm::event::MouseEvent { kind, .. } => match kind {
                    crossterm::event::MouseEventKind::Down(_) => Some(
                        crate::config::KeyAction::Single(crate::editor::Action::CloseDialog),
                    ),
                    _ => None,
                },
            },
            _ => None,
        }
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        None
    }
}

fn truncate(s: &str, max_width: usize) -> String {
    let s = s.trim_start_matches("/");
    if s.len() <= max_width {
        return s.to_string();
    }

    let mut result = String::with_capacity(max_width);
    for (i, c) in s.chars().enumerate() {
        if i == max_width - 1 {
            result.push_str("…");
            break;
        }

        result.push(c);
    }

    result
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 5), "hell…");
    }
}
