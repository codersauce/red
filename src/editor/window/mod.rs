use std::sync::{Arc, Mutex};

use crossterm::style::Color;

use crate::{
    buffer::SharedBuffer,
    highlighter::Highlighter,
    theme::{Style, Theme},
};

pub use self::window::Window;

use super::RenderBuffer;

mod window;

pub struct WindowManager {
    width: usize,
    height: usize,
    theme: Theme,
    highlighter: Arc<Mutex<Highlighter>>,

    windows: Vec<Window>,
    current_window: usize,
}

impl WindowManager {
    pub fn new(
        width: usize,
        height: usize,
        theme: &Theme,
        highlighter: Arc<Mutex<Highlighter>>,
        buffers: &[SharedBuffer],
    ) -> WindowManager {
        let windows = vec![Window::new(
            0,
            0,
            width,
            height - 2,
            buffers.get(0).unwrap().clone(),
            theme.style.clone(),
            theme.gutter_style.clone(),
            &highlighter,
        )];

        WindowManager {
            width,
            height,
            theme: theme.clone(),
            highlighter,

            windows,
            current_window: 0,
        }
    }

    pub fn draw(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        for window in &mut self.windows {
            window.draw(buffer)?;
            // TODO: draw divider
            // if i < self.windows.len() - 1 {
            //     self.draw_divider(buffer, &window)?;
            // }
        }

        Ok(())
    }

    fn draw_divider(&self, buffer: &mut RenderBuffer, window: &Window) -> anyhow::Result<()> {
        let x = window.x + window.width;
        let y = window.y;
        let height = window.height;
        // TODO: let style = self.theme.divider_style.clone();

        let style = Style {
            fg: Some(Color::Rgb {
                r: 0x20,
                g: 0x20,
                b: 0x20,
            }),
            bg: None,
            ..Default::default()
        };

        for i in 0..height {
            buffer.set_text(x, y + i, "│", &style);
        }

        Ok(())
    }

    pub fn split_horizontal(&mut self) {
        let num_windows = self.windows.len() + 1;
        let num_dividers = num_windows - 1;
        let width = (self.width - num_dividers) / num_windows;
        let height = self.height;

        self.windows.push(Window::new(
            0,
            0,
            width,
            height,
            self.current().buffer.clone(),
            self.theme.style.clone(),
            self.theme.gutter_style.clone(),
            &self.highlighter.clone(),
        ));

        for n in 0..self.windows.len() {
            let x = n * width + n;
            let mut width = width;
            if n == self.windows.len() - 1 {
                width = self.width - x;
            }
            self.windows
                .get_mut(n)
                .unwrap()
                .resize_move(x, 0, width, height - 2);
        }
    }

    pub fn next(&mut self) {
        self.current_window = (self.current_window + 1) % self.windows.len();
    }

    pub fn resize_all(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;

        for window in &mut self.windows {
            window.resize(self.width, self.height);
        }
    }

    pub fn set_current(&mut self, n: usize) {
        self.current_window = n;
    }

    pub fn current(&self) -> &Window {
        &self.windows[self.current_window]
    }

    pub fn current_mut(&mut self) -> &mut Window {
        &mut self.windows[self.current_window]
    }

    pub fn find_at(&self, x: usize, y: usize) -> Option<usize> {
        for (i, window) in self.windows.iter().enumerate() {
            if window.contains(x, y) {
                return Some(i);
            }
        }
        None
    }
}

mod test {
    use crate::buffer::Buffer;

    use super::*;

    #[test]
    fn test_window_manager() {
        let theme = Theme::default();
        let highlighter = Arc::new(Mutex::new(Highlighter::new(theme.clone()).unwrap()));
        let buffer = SharedBuffer::new(Buffer::new(None, "test".to_string()));
        let wm = WindowManager::new(80, 24, &theme, highlighter, &[buffer]);
        assert_eq!(wm.windows.len(), 1);
        assert_eq!(wm.current_window, 0);
    }
}

#[cfg(test)]
pub mod test_util {
    #[macro_export]
    macro_rules! setup_window {
        ($x:expr, $y:expr, $width:expr, $height:expr, $lines:expr) => {{
            let lines = $lines.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            let buffer = Buffer::with_lines(None, lines);
            let highlighter = Highlighter::new(Theme::default()).unwrap();
            let mut window = Window::new(
                $x,
                $y,
                $width,
                $height,
                buffer.into(),
                Style::default(),
                Style::default(),
                &Arc::new(Mutex::new(highlighter)),
            );
            let mut render_buffer = RenderBuffer::new(19, 15, Style::default());
            window.draw(&mut render_buffer).unwrap();
            window
        }};
    }
}
