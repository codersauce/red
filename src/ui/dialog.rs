use crate::{
    editor::RenderBuffer,
    theme::{Style, Theme},
};

use super::Component;

pub struct Dialog {
    title: Option<String>,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub style: Style,
    pub border_style: BorderStyle,
    pub theme: Theme,
}

#[derive(PartialEq)]
pub enum BorderStyle {
    None,
    Single,
}

impl Dialog {
    pub fn new(
        title: Option<String>,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        style: &Style,
        border_style: BorderStyle,
        theme: &Theme,
    ) -> Self {
        Self {
            title,
            x,
            y,
            width,
            height,
            style: style.clone(),
            border_style,
            theme: theme.clone(),
        }
    }
}

impl Component for Dialog {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let mut height = self.height;
        let mut width = self.width;

        if self.border_style != BorderStyle::None {
            height += 2;
        }
        if self.border_style != BorderStyle::None {
            width += 2;
        }

        // Draw the dialog box
        for y in self.y..self.y + height {
            for x in self.x..self.x + width {
                buffer.set_char(x, y, ' ', &self.style, &self.theme);
            }
        }

        // Draw the border
        if self.border_style != BorderStyle::None {
            let border_style = match self.border_style {
                BorderStyle::Single => "─│┌┐└┘",
                BorderStyle::None => unreachable!(),
            };

            let mut char_indices = border_style.char_indices();
            let top = char_indices.next().unwrap().1;
            let bottom = top;
            let left = char_indices.next().unwrap().1;
            let right = left;
            let top_left = char_indices.next().unwrap().1;
            let top_right = char_indices.next().unwrap().1;
            let bottom_left = char_indices.next().unwrap().1;
            let bottom_right = char_indices.next().unwrap().1;

            for x in self.x..self.x + width {
                buffer.set_char(x, self.y, top, &self.style, &self.theme);
                buffer.set_char(x, self.y + height - 1, bottom, &self.style, &self.theme);
            }

            for y in self.y..self.y + height {
                buffer.set_char(self.x, y, left, &self.style, &self.theme);
                buffer.set_char(self.x + width - 1, y, right, &self.style, &self.theme);
            }

            buffer.set_char(self.x, self.y, top_left, &self.style, &self.theme);
            buffer.set_char(
                self.x + width - 1,
                self.y,
                top_right,
                &self.style,
                &self.theme,
            );
            buffer.set_char(
                self.x,
                self.y + height - 1,
                bottom_left,
                &self.style,
                &self.theme,
            );
            buffer.set_char(
                self.x + width - 1,
                self.y + height - 1,
                bottom_right,
                &self.style,
                &self.theme,
            );
        }

        if let Some(ref title) = self.title {
            let cx = self.x + (width / 2) - (title.len() / 2);
            buffer.set_text(cx, self.y, &format!(" {} ", title), &self.style);
        }

        Ok(())
    }
}
