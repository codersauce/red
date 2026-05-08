use crate::{
    color::Color,
    editor::{Editor, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::{display_width, fit_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog},
    Component,
};

pub struct Info {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub style: Style,
    pub text: String,
    pub theme: Theme,

    dialog: Dialog,
}

fn fit_info_geometry(
    cursor: (usize, usize),
    editor_width: usize,
    editor_height: usize,
    text_width: usize,
    text_height: usize,
) -> (usize, usize, usize, usize) {
    let (mut x, cursor_y) = cursor;
    let y = cursor_y.saturating_add(1);
    let mut height = text_height;

    if x.saturating_add(text_width) >= editor_width {
        x = editor_width.saturating_sub(text_width.saturating_add(3));
    }

    if y.saturating_add(height) >= editor_height.saturating_sub(2) {
        height = editor_height.saturating_sub(y.saturating_add(2));
    }

    let width = std::cmp::min(text_width, editor_width.saturating_sub(2));

    (x, y, width, height)
}

impl Info {
    pub fn new(editor: &Editor, text: String) -> Self {
        let style = Style {
            fg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            bg: Some(Color::Rgb {
                r: 67,
                g: 70,
                b: 89,
            }),
            ..Default::default()
        };

        let width = text.lines().map(display_width).max().unwrap_or(0);
        let height = text.lines().count();
        let (x, y, width, height) = fit_info_geometry(
            editor.cursor_position(),
            editor.vwidth(),
            editor.vheight(),
            width,
            height,
        );

        Self {
            x,
            y,
            width,
            height,
            style: style.clone(),
            text,
            dialog: Dialog::new(
                None,
                x,
                y,
                width,
                height,
                &style,
                BorderStyle::Single,
                &editor.theme,
            ),
            theme: editor.theme.clone(),
        }
    }
}

impl Component for Info {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;

        for (row, line) in self.text.lines().take(self.height).enumerate() {
            buffer.set_text(
                self.x + 1,
                self.y + 1 + row,
                &fit_display_width(line, self.width),
                &self.style,
            );
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
    fn info_draws_text_at_dialog_content_origin() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(20, 6, &style);
        let info = Info {
            x: 3,
            y: 1,
            width: 5,
            height: 1,
            style: style.clone(),
            text: "hello".to_string(),
            theme: theme.clone(),
            dialog: Dialog::new(None, 3, 1, 5, 1, &style, BorderStyle::Single, &theme),
        };

        info.draw(&mut buffer).unwrap();

        assert_eq!(
            rendered_cells(&buffer, 2, 3, 7),
            vec!['│', 'h', 'e', 'l', 'l', 'o', '│']
        );
    }

    #[test]
    fn info_draws_wide_text_by_display_width() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(20, 6, &style);
        let info = Info {
            x: 0,
            y: 1,
            width: 4,
            height: 1,
            style: style.clone(),
            text: "👋ab".to_string(),
            theme: theme.clone(),
            dialog: Dialog::new(None, 0, 1, 4, 1, &style, BorderStyle::Single, &theme),
        };

        info.draw(&mut buffer).unwrap();

        assert_eq!(
            rendered_cells(&buffer, 2, 0, 6),
            vec!['│', '👋', ' ', 'a', 'b', '│']
        );
    }

    #[test]
    fn info_geometry_does_not_underflow_on_tiny_height() {
        assert_eq!(fit_info_geometry((0, 0), 1, 0, 5, 3), (0, 1, 0, 0));
        assert_eq!(fit_info_geometry((0, 0), 1, 1, 5, 3), (0, 1, 0, 0));
    }
}
