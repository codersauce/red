use crate::{editor::RenderBuffer, theme::Style};

pub struct List {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    items: Vec<String>,
    item_style: Style,
    selected_item_style: Style,
    selected_item: usize,
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
        }
    }

    pub fn draw(&self, buffer: &mut RenderBuffer) {
        for (i, y) in (self.y..self.y + self.height).enumerate() {
            if let Some(item) = self.items.get(y - self.y) {
                let style = if self.selected_item == i {
                    &self.selected_item_style
                } else {
                    &self.item_style
                };
                let line = format!(" {:<width$}", item, width = self.width - 1);
                buffer.set_text(self.x, y, &line, style);
            }
        }
    }
}
