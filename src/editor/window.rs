use std::sync::{Arc, Mutex};

use crate::{buffer::SharedBuffer, highlighter::Highlighter, theme::Style};

use super::RenderBuffer;

pub struct Window {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub buffer: SharedBuffer,
    pub style: Style,
    pub gutter_style: Style,
    pub highlighter: Arc<Mutex<Highlighter>>,
    pub cx: usize,
    pub cy: usize,
    pub top_line: usize,
    pub left_col: usize,
    pub wrap: bool,
}

impl Window {
    pub fn new(
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        buffer: SharedBuffer,
        style: Style,
        gutter_style: Style,
        highlighter: &Arc<Mutex<Highlighter>>,
    ) -> Self {
        Self {
            x,
            y,
            width,
            height,
            buffer,
            style,
            gutter_style,
            highlighter: highlighter.clone(),
            cx: 0,
            cy: 0,
            top_line: 0,
            left_col: 0,
            wrap: true,
        }
    }

    pub fn move_down(&mut self) {
        if self.cy < self.height - 1 {
            self.cy += 1;
        } else if self.top_line + self.height < self.buffer.lock_read().unwrap().lines.len() {
            self.top_line += 1;
        }
    }

    pub fn move_up(&mut self) {
        if self.cy > 0 {
            self.cy -= 1;
        } else if self.top_line > 0 {
            self.top_line -= 1;
        }
    }

    pub fn move_left(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        } else if self.left_col > 0 {
            self.left_col -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.cx < self.width - 1 {
            self.cx += 1;
        } else {
            self.left_col += 1;
        }
    }

    pub fn move_to_line_start(&mut self) {
        self.cx = 0;
    }

    pub fn move_to_line_end(&mut self) {
        self.cx = self
            .current_line_contents()
            .map(|l| l.len().saturating_sub(1))
            .unwrap_or(0);
    }

    pub fn page_up(&mut self) {
        if self.cy > self.height {
            self.cy -= self.height;
        } else {
            self.cy = 0;
        }
    }

    pub fn page_down(&mut self) {
        if self.cy + self.height < self.line_count() {
            self.top_line += self.height;
        } else {
            self.cy = self.line_count().saturating_sub(1);
        }
    }

    pub fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let mut y = self.y;
        let mut current_line = self.top_line;

        loop {
            y += match self.draw_line(buffer, y, current_line)? {
                DrawLineResult::None => 1,
                DrawLineResult::Wrapped(n) => n,
                DrawLineResult::Clipped => 1,
            };

            if y >= self.height {
                break;
            }

            current_line += 1;
        }

        let line = " ".repeat(self.width);
        while y < self.height {
            buffer.set_text(0, y, &line, &self.style);
            y += 1;
        }

        Ok(())
    }

    pub fn draw_gutter(
        &self,
        buffer: &mut RenderBuffer,
        y: usize,
        line: Option<usize>,
    ) -> anyhow::Result<usize> {
        let max_line_number_len = format!("{}", self.line_count()).len();
        let gutter_style = &self.gutter_style;
        let line_number = if let Some(line) = line {
            format!(" {:>width$} ", line + 1, width = max_line_number_len)
        } else {
            " ".repeat(max_line_number_len + 2)
        };
        buffer.set_text(self.x, y, &line_number, &gutter_style);

        Ok(self.x + max_line_number_len + 2)
    }

    pub fn draw_line(
        &self,
        buffer: &mut RenderBuffer,
        y: usize,
        line_num: usize,
    ) -> anyhow::Result<DrawLineResult> {
        let mut result = DrawLineResult::None;

        if let Some(line) = self.line_contents(line_num) {
            let style_info = self
                .highlighter
                .lock()
                .expect("poisoned lock")
                .highlight(&line)
                .unwrap_or_default();

            let initial_x = self.draw_gutter(buffer, y, Some(line_num))?;
            let initial_y = y;

            let mut x = initial_x;
            let mut y = y;

            if self.wrap {
                for (pos, c) in line.chars().enumerate() {
                    let style = style_info
                        .iter()
                        .find(|s| s.contains(pos))
                        .map(|s| &s.style)
                        .unwrap_or(&self.style);

                    buffer.set_char(x, y, c, style);
                    x += 1;
                    if x >= self.width {
                        x = initial_x;
                        y += 1;
                        self.draw_gutter(buffer, y, None)?;
                    }
                }
                result = DrawLineResult::Wrapped(y - initial_y + 1);
            } else {
                if line.len() >= self.left_col {
                    for (pos, c) in line[self.left_col..].chars().enumerate() {
                        let style = style_info
                            .iter()
                            .find(|s| s.contains(self.left_col + pos))
                            .map(|s| &s.style)
                            .unwrap_or(&self.style);

                        if x + pos >= self.width {
                            result = DrawLineResult::Clipped;
                            break;
                        }
                        buffer.set_char(x + pos, y, c, style);
                    }
                    x = initial_x + line.len().saturating_sub(self.left_col);
                }
            }

            let padding = " ".repeat(self.width.saturating_sub(x));
            buffer.set_text(x, y, &padding, &self.style);
        }

        Ok(result)
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        ((self.gutter_width() + self.cx) as u16, self.cy as u16)
    }

    fn gutter_width(&self) -> usize {
        format!("{}", self.line_count()).len() + 2
    }

    fn current_line(&self) -> Option<usize> {
        if self.cy + self.top_line < self.line_count() {
            Some(self.cy + self.top_line)
        } else {
            None
        }
    }

    fn current_line_contents(&self) -> Option<String> {
        self.line_contents(self.cy + self.top_line)
    }

    fn line_contents(&self, line: usize) -> Option<String> {
        self.buffer.lock_read().unwrap().get(line)
    }

    fn line_count(&self) -> usize {
        self.buffer.lock_read().unwrap().lines.len()
    }
}

#[derive(Debug, PartialEq)]
pub enum DrawLineResult {
    None,
    Wrapped(usize),
    Clipped,
}
