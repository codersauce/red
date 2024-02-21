use crate::theme::Style;

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

    pub fn draw(&self, buffer: &mut crate::editor::RenderBuffer) -> anyhow::Result<()> {
        for y in self.y..self.y + self.height {
            for x in self.x..self.x + self.width {
                buffer.set_char(x, y, ' ', &self.style);
            }
        }

        Ok(())
    }
}
