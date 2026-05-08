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

        let (mut x, y) = editor.cursor_position();
        let y = y + 1;

        let width = text.lines().map(display_width).max().unwrap_or(0);
        let mut height = text.lines().count();

        if x + width >= editor.vwidth() {
            x = editor.vwidth().saturating_sub(width + 3);
        }

        if y + height >= editor.vheight() - 2 {
            height = editor.vheight().saturating_sub(y + 2);
            // TODO: we need scroll if this happens
        }

        let width = std::cmp::min(width, editor.vwidth().saturating_sub(2));

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
}
