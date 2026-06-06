use crate::{
    editor::RenderBuffer,
    theme::Style,
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
};

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
        if self.items.is_empty() {
            return;
        }

        self.selected_item = (self.selected_item + 1).min(self.items.len() - 1);
        if self.height > 0 && self.selected_item >= self.top_index + self.height {
            self.top_index = self.selected_item.saturating_sub(self.height - 1);
        }
    }

    pub(crate) fn move_up(&mut self) {
        self.selected_item = self.selected_item.saturating_sub(1);
        if self.selected_item < self.top_index {
            self.top_index = self.selected_item;
        }
    }

    pub fn page_down(&mut self) {
        self.move_by(self.height as isize);
    }

    pub fn page_up(&mut self) {
        self.move_by(-(self.height as isize));
    }

    fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() || self.height == 0 {
            return;
        }

        let new_selected = if delta.is_negative() {
            self.selected_item.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected_item.saturating_add(delta as usize)
        };

        self.selected_item = new_selected.min(self.items.len() - 1);
        if self.selected_item < self.top_index {
            self.top_index = self.selected_item;
        } else if self.selected_item >= self.top_index + self.height {
            self.top_index = self.selected_item.saturating_sub(self.height - 1);
        }
    }

    pub fn selected_item(&self) -> String {
        self.items
            .get(self.selected_item)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_items(&mut self, new_items: Vec<String>) {
        self.selected_item = 0;
        self.top_index = 0;
        self.items = new_items.iter().map(|s| truncate(s, self.width)).collect();
    }

    pub fn set_selected_item(&mut self, item: &str) {
        let item = truncate(item, self.width);
        let Some(index) = self.items.iter().position(|candidate| candidate == &item) else {
            return;
        };
        self.selected_item = index;
        if self.height > 0 && self.selected_item >= self.top_index + self.height {
            self.top_index = self.selected_item.saturating_sub(self.height - 1);
        } else if self.selected_item < self.top_index {
            self.top_index = self.selected_item;
        }
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
                let line = fit_display_width(&format!(" {item}"), self.width);
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
            crossterm::event::Event::Mouse(ev) => {
                let crossterm::event::MouseEvent { kind, .. } = ev;
                match kind {
                    crossterm::event::MouseEventKind::Down(_) => Some(
                        crate::config::KeyAction::Single(crate::editor::Action::CloseDialog),
                    ),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        None
    }
}

fn truncate(s: &str, max_width: usize) -> String {
    let s = s.trim_start_matches("/");
    if display_width(s) <= max_width {
        return s.to_string();
    }

    if max_width == 0 {
        return String::new();
    }

    let mut result = truncate_display_width(s, max_width - 1);
    result.push('…');
    result
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 5), "hell…");
    }

    #[test]
    fn test_truncate_uses_display_width() {
        assert_eq!(truncate("ab👋cd", 5), "ab👋…");
        assert_eq!(display_width(&truncate("ab👋cd", 5)), 5);
    }

    #[test]
    fn test_empty_list_navigation_is_safe() {
        let style = Style::default();
        let mut list = List::new(0, 0, 10, 3, vec![], &style, &style);

        list.move_down();
        list.move_up();

        assert_eq!(list.selected_item(), "");
    }

    #[test]
    fn test_set_items_truncates_by_display_width() {
        let style = Style::default();
        let mut list = List::new(0, 0, 5, 3, vec![], &style, &style);

        list.set_items(vec!["ab👋cd".to_string()]);

        assert_eq!(display_width(&list.selected_item()), 5);
    }
}
