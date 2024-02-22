use crate::{editor::RenderBuffer, theme::Style};

use super::Component;

pub struct Dialog {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub style: Style,
}

impl Dialog {
    pub fn new(x: usize, y: usize, width: usize, height: usize, style: &Style) -> Self {
        Self {
            x,
            y,
            width,
            height,
            style: style.clone(),
        }
    }
}

impl Component for Dialog {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Draw the dialog box
        for y in self.y..self.y + self.height {
            for x in self.x..self.x + self.width {
                buffer.set_char(x, y, ' ', &self.style);
            }
        }

        Ok(())
    }
}
