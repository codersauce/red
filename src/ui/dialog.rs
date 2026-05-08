use crate::{
    editor::RenderBuffer,
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
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
    #[allow(clippy::too_many_arguments)]
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
            let title = format!(" {} ", title);
            let title = truncate_display_width(&title, width);
            let title_width = display_width(&title);
            let cx = self.x + width.saturating_sub(title_width) / 2;
            buffer.set_text(cx, self.y, &title, &self.style);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_cells(buffer: &RenderBuffer, y: usize, x: usize, width: usize) -> Vec<char> {
        buffer.cells[y * buffer.width + x..y * buffer.width + x + width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[test]
    fn long_title_does_not_underflow_when_centered() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(10, 4, &style);
        let dialog = Dialog::new(
            Some("very long title".to_string()),
            0,
            0,
            3,
            1,
            &style,
            BorderStyle::Single,
            &theme,
        );

        dialog.draw(&mut buffer).unwrap();

        assert_eq!(rendered_cells(&buffer, 0, 0, 5).len(), 5);
    }

    #[test]
    fn title_placement_uses_display_width() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(12, 4, &style);
        let dialog = Dialog::new(
            Some("👋".to_string()),
            0,
            0,
            8,
            1,
            &style,
            BorderStyle::Single,
            &theme,
        );

        dialog.draw(&mut buffer).unwrap();

        assert_eq!(rendered_cells(&buffer, 0, 3, 4), vec![' ', '👋', ' ', ' ']);
    }
}
