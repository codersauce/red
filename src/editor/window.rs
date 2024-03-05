use std::sync::{Arc, Mutex};

use crate::{buffer::SharedBuffer, highlighter::Highlighter, theme::Style};

use super::{
    action::{ActionEffect, GoToLinePosition},
    RenderBuffer,
};

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

    pub fn move_down(&mut self) -> ActionEffect {
        if self.top_line + self.cy < self.line_count() - 1 {
            self.cy += 1;
            if self.cy >= self.height {
                self.top_line += 1;
                self.cy -= 1;
                return ActionEffect::RedrawWindow;
            }
        }
        ActionEffect::RedrawCursor
    }

    pub fn move_up(&mut self) -> ActionEffect {
        crate::log!(
            "move_up with self.cy = {} self.top_line = {}",
            self.cy,
            self.top_line
        );
        if self.cy == 0 {
            crate::log!("self.cy is zero, checking topline");
            if self.top_line > 0 {
                crate::log!("topline is greater than zero, decrementing");
                self.top_line -= 1;
                return ActionEffect::RedrawWindow;
            }
            crate::log!("topline is zero, returning none");
            return ActionEffect::None;
        }

        crate::log!("decrementing self.cy");
        self.cy = self.cy.saturating_sub(1);
        ActionEffect::RedrawCursor
    }

    pub fn move_left(&mut self) -> ActionEffect {
        if self.cx > 0 {
            self.cx -= 1;
            return ActionEffect::RedrawCursor;
        } else if self.left_col > 0 {
            self.left_col -= 1;
            return ActionEffect::RedrawWindow;
        }

        ActionEffect::None
    }

    pub fn move_right(&mut self) -> ActionEffect {
        if self.cx < self.width - 1 {
            self.cx += 1;
            return ActionEffect::RedrawCursor;
        } else {
            self.left_col += 1;
            return ActionEffect::RedrawWindow;
        }
    }

    pub fn move_to_line_start(&mut self) -> ActionEffect {
        self.cx = 0;

        ActionEffect::RedrawCursor
    }

    pub fn move_to_line_end(&mut self) -> ActionEffect {
        self.cx = self
            .current_line_contents()
            .map(|l| l.len().saturating_sub(1))
            .unwrap_or(0);

        ActionEffect::RedrawCursor
    }

    pub fn page_up(&mut self) -> ActionEffect {
        if self.top_line > 0 {
            self.top_line = self.top_line.saturating_sub(self.height);
            return ActionEffect::RedrawWindow;
        }

        ActionEffect::None
    }

    pub fn page_down(&mut self) -> ActionEffect {
        if self.line_count() > self.top_line + self.height {
            self.top_line += self.height;
            return ActionEffect::RedrawWindow;
        }

        ActionEffect::None
    }

    pub fn move_to_top(&mut self) -> ActionEffect {
        self.cx = 0;
        self.cy = 0;
        self.top_line = 0;

        ActionEffect::RedrawWindow
    }

    pub fn move_to_bottom(&mut self) -> ActionEffect {
        let buffer_size = self.line_count();
        if buffer_size > self.height {
            self.cy = self.height - 1;
            self.top_line = buffer_size - self.height;
        } else {
            self.cy = buffer_size - 1;
            self.top_line = 0;
        }

        ActionEffect::RedrawWindow
    }

    pub fn move_to_next_word(&mut self) -> ActionEffect {
        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };

        let next_word = self
            .buffer
            .lock_read()
            .expect("poisoned lock")
            .find_next_word((self.cx, line));

        if let Some((x, y)) = next_word {
            self.cx = x;
            self.go_to_line(y + 1, GoToLinePosition::Top);
            return ActionEffect::RedrawCursor;
        }

        ActionEffect::None
    }

    pub fn move_to_previous_word(&mut self) -> ActionEffect {
        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };

        let previous_word = self
            .buffer
            .lock_read()
            .expect("poisoned lock")
            .find_prev_word((self.cx, line));

        if let Some((x, y)) = previous_word {
            self.cx = x;
            self.go_to_line(y + 1, GoToLinePosition::Top);
            return ActionEffect::RedrawCursor;
        }

        ActionEffect::None
    }

    pub fn move_line_to_middle(&mut self) -> ActionEffect {
        let viewport_center = self.height / 2;
        let distance_to_center = self.cy as isize - viewport_center as isize;

        if distance_to_center == 0 {
            // already at the middle
            return ActionEffect::None;
        }

        if distance_to_center > 0 {
            // if distance > 0 we need to scroll up
            let distance_to_center = distance_to_center.abs() as usize;
            if self.top_line > distance_to_center {
                let new_vtop = self.top_line + distance_to_center;
                self.top_line = new_vtop;
                self.cy = viewport_center;
                return ActionEffect::RedrawWindow;
            }
        }

        // if distance < 0 we need to scroll down
        let distance_to_center = distance_to_center.abs() as usize;
        let new_vtop = self.top_line.saturating_sub(distance_to_center);
        let distance_to_go = self.top_line as usize + distance_to_center;
        if self.buffer.lock_read().expect("poisoned lock").len() > distance_to_go
            && new_vtop != self.top_line
        {
            self.top_line = new_vtop;
            self.cy = viewport_center;
            return ActionEffect::RedrawWindow;
        }

        ActionEffect::None
    }

    pub fn move_line_to_bottom(&mut self) -> ActionEffect {
        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };

        if line > self.top_line + self.height {
            self.top_line = line - self.height;
            self.cy = self.height - 1;

            return ActionEffect::RedrawWindow;
        }

        ActionEffect::None
    }

    pub fn insert_line_below_cursor(&mut self) -> ActionEffect {
        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };

        // TODO: undo
        // editor
        //     .undo_actions
        //     .push(Action::DeleteLineAt(editor.buffer_line() + 1));

        let leading_spaces = self.current_line_indentation();
        self.buffer
            .lock()
            .expect("poisoned lock")
            .insert_line(line + 1, " ".repeat(leading_spaces));
        // TODO: notify_change(lsp, editor).await?;
        self.cy += 1;
        self.cx = leading_spaces;

        ActionEffect::RedrawWindow
    }

    pub fn insert_line_at_cursor(&mut self) -> ActionEffect {
        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };

        // TODO: undo
        // self.undo_actions
        //     .push(Action::DeleteLineAt(editor.buffer_line()));

        // if the current line is empty, let's use the indentation from the line above
        let leading_spaces = if let Some(line) = self.current_line_contents() {
            if line.is_empty() {
                self.previous_line_indentation()
            } else {
                self.current_line_indentation()
            }
        } else {
            self.previous_line_indentation()
        };

        self.buffer
            .lock()
            .expect("poisoned lock")
            .insert_line(line, " ".repeat(leading_spaces));
        // TODO: notify_change(lsp, self).await?;
        self.cx = leading_spaces;

        ActionEffect::RedrawWindow
    }

    pub fn insert_char_at_cursor(&mut self, c: char) -> ActionEffect {
        let Some(current_line) = self.current_line() else {
            return ActionEffect::None;
        };

        // TODO: buffer undo stack
        self.buffer.lock().unwrap().insert(self.cx, current_line, c);

        // TODO: notify_change(lsp, editor).await?;
        self.cx += 1;

        ActionEffect::RedrawLine
    }

    pub fn insert_new_line(&mut self) -> ActionEffect {
        // TODO: notify_change
        // TODO: undo
        // editor.insert_undo_actions.extend(vec![
        //     Action::MoveTo(editor.cx, editor.buffer_line() + 1),
        //     Action::DeleteLineAt(editor.buffer_line() + 1),
        //     Action::ReplaceLineAt(
        //         editor.buffer_line(),
        //         editor.current_line_contents().unwrap_or_default(),
        //     ),
        // ]);
        let spaces = self.current_line_indentation();

        let current_line = self.current_line_contents().unwrap_or_default();
        let before_cursor = current_line[..self.cx].to_string();
        let after_cursor = current_line[self.cx..].to_string();

        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };

        self.buffer
            .lock()
            .expect("poisoned lock")
            .replace_line(line, before_cursor);
        // TODO: notify_change(lsp, self).await?;

        self.cx = spaces;
        self.cy += 1;

        let new_line = format!("{}{}", " ".repeat(spaces), &after_cursor);
        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };

        self.buffer.lock().unwrap().insert_line(line, new_line);

        ActionEffect::RedrawWindow
    }

    pub fn insert_tab(&mut self) -> ActionEffect {
        // TODO: Tab configuration
        let tabsize = 4;

        let cx = self.cx;
        let Some(line) = self.current_line() else {
            return ActionEffect::None;
        };
        self.buffer
            .lock()
            .expect("poisoned lock")
            .insert_str(cx, line, &" ".repeat(tabsize));
        // TODO: notify_change(lsp, editor).await?;
        self.cx += tabsize;

        ActionEffect::RedrawLine
    }

    pub fn delete_char_at_cursor(&mut self) -> ActionEffect {
        // TODO: buffer undo stack

        let Some(current_line) = self.current_line() else {
            return ActionEffect::None;
        };

        self.buffer.lock().unwrap().remove(self.cx, current_line);
        // TODO: notify_change(lsp, editor).await?;

        ActionEffect::RedrawLine
    }

    pub fn delete_char_at(&mut self, x: usize, y: usize) -> ActionEffect {
        // TODO: notify_change(lsp, editor).await?;
        self.buffer.lock().unwrap().remove(x, y);

        ActionEffect::RedrawLine
    }

    pub fn delete_previous_char(&mut self) -> ActionEffect {
        if self.cx > 0 {
            self.cx -= 1;
            let cx = self.cx;
            let Some(line) = self.current_line() else {
                return ActionEffect::None;
            };
            self.buffer.lock().expect("poisoned lock").remove(cx, line);
            // TODO: notify_change(lsp, editor).await?;
            return ActionEffect::RedrawLine;
        }

        ActionEffect::None
    }

    pub fn go_to_line(&mut self, line: usize, pos: GoToLinePosition) -> ActionEffect {
        if line == 0 {
            return self.move_to_top();
        }

        let buffer_size = self.buffer.lock_read().expect("poisoned lock").len();
        if line <= buffer_size {
            let y = line - 1;

            if self.is_visible(y) {
                self.cy = y - self.top_line;
                return ActionEffect::RedrawCursor;
            }

            if self.is_within_first_page(y) {
                self.top_line = 0;
                self.cy = y;

                return ActionEffect::RedrawWindow;
            }

            if self.is_within_last_page(y) {
                self.top_line = buffer_size - self.height;
                self.cy = y - self.top_line;

                return ActionEffect::RedrawWindow;
            };

            if matches!(pos, GoToLinePosition::Bottom) {
                let Some(line) = self.current_line() else {
                    return ActionEffect::None;
                };

                self.top_line = y - self.height;
                self.cy = line - self.top_line;
            } else {
                self.top_line = y;
                self.cy = 0;
                if matches!(pos, GoToLinePosition::Center) {
                    return self.move_to_top();
                }
            }

            // FIXME: this is wasteful when move to viewport center worked
            // but we have to account for the case where it didn't and also
            return ActionEffect::RedrawWindow;
        }

        ActionEffect::None
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

    pub fn draw_current_line(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if let Some(line) = self.current_line() {
            let y = self.y + self.cy;
            self.draw_line(buffer, y, line)?;
        }
        Ok(())
    }

    fn draw_gutter(
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

    fn draw_line(
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

    pub fn cursor_location(&self) -> (usize, usize) {
        (self.left_col + self.cx, self.current_line().unwrap())
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        ((self.gutter_width() + self.cx) as u16, self.cy as u16)
    }

    pub fn buffer_name(&self) -> String {
        self.buffer.lock_read().unwrap().name().to_string()
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.lock_read().unwrap().is_dirty()
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

    fn previous_line_indentation(&self) -> usize {
        let Some(line) = self.current_line() else {
            return 0;
        };

        if line > 0 {
            self.buffer
                .lock()
                .expect("poisoned lock")
                .get(line - 1)
                .unwrap_or_default()
                .chars()
                .position(|c| !c.is_whitespace())
                .unwrap_or(0)
        } else {
            0
        }
    }

    fn current_line_indentation(&self) -> usize {
        self.current_line_contents()
            .unwrap_or_default()
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    fn line_contents(&self, line: usize) -> Option<String> {
        self.buffer.lock_read().unwrap().get(line)
    }

    fn line_count(&self) -> usize {
        self.buffer.lock_read().unwrap().lines.len()
    }

    fn is_visible(&self, y: usize) -> bool {
        (self.top_line..self.top_line + self.height).contains(&y)
    }

    fn is_within_last_page(&self, y: usize) -> bool {
        y > self.buffer.lock_read().expect("poisoned lock").len() - self.height
    }

    fn is_within_first_page(&self, y: usize) -> bool {
        y < self.height
    }
}

#[derive(Debug, PartialEq)]
pub enum DrawLineResult {
    None,
    Wrapped(usize),
    Clipped,
}
