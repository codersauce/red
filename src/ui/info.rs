use crate::{
    editor::{Editor, RenderBuffer},
    theme::Style,
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

    dialog: Dialog,
}

impl Info {
    pub fn new(editor: &Editor, text: String) -> Self {
        let style = Style {
            fg: Some(crossterm::style::Color::White),
            bg: Some(crossterm::style::Color::Black),
            ..Default::default()
        };

        let (mut x, y) = editor.cursor_position();
        let y = y + 1;

        let width = text.lines().map(|l| l.len()).max().unwrap_or(0);
        let height = text.lines().count();

        if x + width >= editor.vwidth() as usize {
            x = editor.vwidth() as usize - width - 2;
        }

        Self {
            x,
            y,
            width,
            height,
            style: style.clone(),
            text,
            dialog: Dialog::new(x, y, width, height, &style, BorderStyle::Single),
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
                    buffer.set_char(x + 1 + self.x, y, c, &self.style);
                }
            } else {
                break;
            }
        }

        Ok(())
    }
}
