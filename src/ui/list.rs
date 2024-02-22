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

    pub fn items(&self) -> &Vec<String> {
        &self.items
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
}
