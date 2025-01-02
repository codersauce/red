use crate::{
    color::Color,
    editor::{Editor, RenderBuffer},
    theme::{Style, Theme},
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

        let width = text.lines().map(|l| l.len()).max().unwrap_or(0);
        let mut height = text.lines().count();

        if x + width >= editor.vwidth() as usize {
            x = editor.vwidth().saturating_sub(width + 3);
        }

        if y + height >= editor.vheight() - 2 as usize {
            height = editor.vheight().saturating_sub(y + 2);
            // TODO: we need scroll if this happens
        }

        let width = std::cmp::min(width, editor.vwidth()) - 2;

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

        let mut lines = self.text.lines();
        for y in self.y + 1..self.y + 1 + self.height {
            if let Some(line) = lines.next() {
                for (x, c) in line.chars().enumerate() {
                    let x = x + 1 + self.x;
                    if x < self.width - 2 {
                        buffer.set_char(x + 1 + self.x, y, c, &self.style, &self.theme);
                    }
                }
            } else {
                break;
            }
        }

        Ok(())
    }
}
