use std::sync::{Arc, Mutex};

use crate::{highlighter::Highlighter, theme::Theme};

use super::RenderBuffer;

pub struct Viewport<'a> {
    width: usize,
    height: usize,
    top: usize,
    left: usize,
    wrap: bool,
    theme: &'a Theme,
    highlighter: Arc<Mutex<Highlighter>>,
}

impl<'a> Viewport<'a> {
    pub fn new(
        theme: &'a Theme,
        width: usize,
        height: usize,
        left: usize,
        top: usize,
    ) -> anyhow::Result<Self> {
        let highlighter = Highlighter::new(theme)?;

        Ok(Self {
            width,
            height,
            top,
            left,
            wrap: true,
            theme,
            highlighter: Arc::new(Mutex::new(highlighter)),
        })
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
        if wrap {
            self.left = 0;
        }
    }

    pub fn set_top(&mut self, top: usize) {
        self.top = top;
    }

    pub fn set_left(&mut self, left: usize) {
        self.left = left;
        self.wrap = false;
    }

    pub fn draw(
        &self,
        buffer: &mut RenderBuffer,
        contents: &[String],
        x: usize,
        y: usize,
    ) -> anyhow::Result<()> {
        let mut y = y;
        let mut current_line = self.top;

        loop {
            y += match self.draw_line(buffer, contents, x, y, current_line)? {
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
            buffer.set_text(0, y, &line, &self.theme.style);
            y += 1;
        }

        Ok(())
    }

    pub fn draw_gutter(
        &self,
        buffer: &mut RenderBuffer,
        contents: &[String],
        x: usize,
        y: usize,
        line: Option<usize>,
    ) -> anyhow::Result<usize> {
        let max_line_number_len = format!("{}", contents.len()).len();
        let gutter_style = &self.theme.gutter_style;
        let line_number = if let Some(line) = line {
            format!(" {:>width$} ", line + 1, width = max_line_number_len)
        } else {
            " ".repeat(max_line_number_len + 2)
        };
        buffer.set_text(x, y, &line_number, &gutter_style);

        Ok(x + max_line_number_len + 2)
    }

    pub fn draw_line(
        &self,
        buffer: &mut RenderBuffer,
        contents: &[String],
        x: usize,
        y: usize,
        line_num: usize,
    ) -> anyhow::Result<DrawLineResult> {
        let mut result = DrawLineResult::None;

        if let Some(line) = contents.get(line_num) {
            let style_info = self
                .highlighter
                .lock()
                .expect("poisoned lock")
                .highlight(line)
                .unwrap_or_default();

            let initial_x = self.draw_gutter(buffer, contents, x, y, Some(line_num))?;
            let initial_y = y;

            let mut x = initial_x;
            let mut y = y;

            if self.wrap {
                for (pos, c) in line.chars().enumerate() {
                    let style = style_info
                        .iter()
                        .find(|s| s.contains(pos))
                        .map(|s| &s.style)
                        .unwrap_or(&self.theme.style);

                    buffer.set_char(x, y, c, style);
                    x += 1;
                    if x >= self.width {
                        x = initial_x;
                        y += 1;
                        self.draw_gutter(buffer, contents, 0, y, None)?;
                    }
                }
                result = DrawLineResult::Wrapped(y - initial_y + 1);
            } else {
                if line.len() >= self.left {
                    for (pos, c) in line[self.left..].chars().enumerate() {
                        let style = style_info
                            .iter()
                            .find(|s| s.contains(self.left + pos))
                            .map(|s| &s.style)
                            .unwrap_or(&self.theme.style);

                        if x + pos >= self.width {
                            result = DrawLineResult::Clipped;
                            break;
                        }
                        buffer.set_char(x + pos, y, c, style);
                    }
                    x = initial_x + line.len().saturating_sub(self.left);
                }
            }

            let padding = " ".repeat(self.width.saturating_sub(x));
            buffer.set_text(x, y, &padding, &self.theme.style);
        }

        Ok(result)
    }
}

#[derive(Debug, PartialEq)]
pub enum DrawLineResult {
    None,
    Wrapped(usize),
    Clipped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_draw_line() {
        let theme = Theme::builder()
            .style("#ffffff", "#000000")
            .gutter("#000000", "#ffffff")
            .scope("keyword", "#000001", "#000000")
            .build();

        let code =
            vec![
                "pub fn draw(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize) -> anyhow::Result<()> {",
                "    let styles = self.highlighter.highlight(&self.contents)?;",
                "",
                "    let mut x = 0;",
                "    let mut y = 0;",
                "    for (pos, c) in self.contents.chars().enumerate() {",
                "        let style = styles",
                "            .iter()",
                "            .find(|s| s.contains(pos))",
                "            .map(|s| &s.style)",
                "            .unwrap_or(&self.theme.style);",
                "",
                "        buffer.set_char(x + pos, y, c, style);",
                "    }",
                "    Ok(())",
                "}",
            ].iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let mut viewport = Viewport::new(&theme, 43, 5, 0, 0).unwrap();
        let mut buffer = RenderBuffer::new(43, 5, theme.style.clone());
        assert_eq!(
            viewport.draw_line(&mut buffer, &code, 0, 0, 0).unwrap(),
            DrawLineResult::Wrapped(3)
        );

        let expected = trim(
            r#"
            |  1 pub fn draw(&mut self, buffer: &mut Ren|
            |    derBuffer, x: usize, y: usize) -> anyho|
            |    w::Result<()> {                        |
            |                                           |
            |                                           |
            "#,
        );
        assert_eq!(buffer.dump(), expected);
        assert_eq!(
            viewport.draw_line(&mut buffer, &code, 0, 3, 1).unwrap(),
            DrawLineResult::Wrapped(2)
        );

        let expected = trim(
            r#"
            |  1 pub fn draw(&mut self, buffer: &mut Ren|
            |    derBuffer, x: usize, y: usize) -> anyho|
            |    w::Result<()> {                        |
            |  2     let styles = self.highlighter.highl|
            |    ight(&self.contents)?;                 |
            "#,
        );
        assert_eq!(buffer.dump(), expected);

        viewport.set_wrap(false);
        let mut buffer = RenderBuffer::new(43, 5, theme.style.clone());
        assert_eq!(
            viewport.draw_line(&mut buffer, &code, 0, 0, 0).unwrap(),
            DrawLineResult::Clipped
        );

        let expected = trim(
            r#"
            |  1 pub fn draw(&mut self, buffer: &mut Ren|
            |                                           |
            |                                           |
            |                                           |
            |                                           |
            "#,
        );
        assert_eq!(buffer.dump(), expected);

        viewport.set_left(5);
        let mut buffer = RenderBuffer::new(43, 5, theme.style.clone());
        assert_eq!(
            viewport.draw_line(&mut buffer, &code, 0, 0, 0).unwrap(),
            DrawLineResult::Clipped
        );

        let expected = trim(
            r#"
            |  1 n draw(&mut self, buffer: &mut RenderBu|
            |                                           |
            |                                           |
            |                                           |
            |                                           |
            "#,
        );
        assert_eq!(buffer.dump(), expected);
    }

    #[test]
    fn test_draw() {
        let theme = Theme::builder()
            .style("#ffffff", "#000000")
            .gutter("#000000", "#ffffff")
            .scope("keyword", "#000001", "#000000")
            .build();

        let code =
            vec![
                "pub fn draw(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize) -> anyhow::Result<()> {",
                "    let styles = self.highlighter.highlight(&self.contents)?;",
                "",
                "    let mut x = 0;",
                "    let mut y = 0;",
                "    for (pos, c) in self.contents.chars().enumerate() {",
                "        let style = styles",
                "            .iter()",
                "            .find(|s| s.contains(pos))",
                "            .map(|s| &s.style)",
                "            .unwrap_or(&self.theme.style);",
                "",
                "        buffer.set_char(x + pos, y, c, style);",
                "    }",
                "    Ok(())",
                "}",
            ].iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let mut viewport = Viewport::new(&theme, 43, 5, 0, 0).unwrap();
        let mut buffer = RenderBuffer::new(43, 5, theme.style.clone());
        viewport.draw(&mut buffer, &code, 0, 0).unwrap();

        let expected = trim(
            r#"
            |  1 pub fn draw(&mut self, buffer: &mut Ren|
            |    derBuffer, x: usize, y: usize) -> anyho|
            |    w::Result<()> {                        |
            |  2     let styles = self.highlighter.highl|
            |    ight(&self.contents)?;                 |
            "#,
        );
        assert_eq!(buffer.dump(), expected);

        viewport.set_top(3);
        viewport.draw(&mut buffer, &code, 0, 0).unwrap();
        let expected = trim(
            r#"
            |  4     let mut x = 0;                     |
            |  5     let mut y = 0;                     |
            |  6     for (pos, c) in self.contents.chars|
            |    ().enumerate() {                       |
            |  7         let style = styles             |
            "#,
        );
        assert_eq!(buffer.dump(), expected);

        viewport.set_wrap(false);
        viewport.set_top(0);
        viewport.set_left(0);
        viewport.draw(&mut buffer, &code, 0, 0).unwrap();
        let expected = trim(
            r#"
            |  1 pub fn draw(&mut self, buffer: &mut Ren|
            |  2     let styles = self.highlighter.highl|
            |  3                                        |
            |  4     let mut x = 0;                     |
            |  5     let mut y = 0;                     |
            "#,
        );
        assert_eq!(buffer.dump(), expected);

        viewport.set_top(3);
        viewport.draw(&mut buffer, &code, 0, 0).unwrap();
        let expected = trim(
            r#"
            |  4     let mut x = 0;                     |
            |  5     let mut y = 0;                     |
            |  6     for (pos, c) in self.contents.chars|
            |  7         let style = styles             |
            |  8             .iter()                    |
            "#,
        );
        assert_eq!(buffer.dump(), expected);

        viewport.set_left(5);
        viewport.draw(&mut buffer, &code, 0, 0).unwrap();
        let expected = trim(
            r#"
            |  4 et mut x = 0;                          |
            |  5 et mut y = 0;                          |
            |  6 or (pos, c) in self.contents.chars().en|
            |  7    let style = styles                  |
            |  8        .iter()                         |
            "#,
        );
        assert_eq!(buffer.dump(), expected);
    }

    #[test]
    fn test_draw_with_scroll() {
        let theme = Theme::builder()
            .style("#ffffff", "#000000")
            .gutter("#000000", "#ffffff")
            .scope("keyword", "#000001", "#000000")
            .build();

        let code =
            vec![
                "pub fn draw(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize) -> anyhow::Result<()> {",
                "    let styles = self.highlighter.highlight(&self.contents)?;",
                "",
                "    let mut x = 0;",
                "    let mut y = 0;",
                "    for (pos, c) in self.contents.chars().enumerate() {",
                "        let style = styles",
                "            .iter()",
                "            .find(|s| s.contains(pos))",
                "            .map(|s| &s.style)",
                "            .unwrap_or(&self.theme.style);",
                "",
                "        buffer.set_char(x + pos, y, c, style);",
                "    }",
                "    Ok(())",
                "}",
            ].iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let mut viewport = Viewport::new(&theme, 43, 5, 0, 0).unwrap();
        let mut buffer = RenderBuffer::new(43, 5, theme.style.clone());
        viewport.set_top(0);
        viewport.set_left(15);
        viewport.draw(&mut buffer, &code, 0, 0).unwrap();
        let expected = trim(
            r#"
            |  1 t self, buffer: &mut RenderBuffer, x: u|
            |  2 = self.highlighter.highlight(&self.cont|
            |  3                                        |
            |  4  0;                                    |
            |  5  0;                                    |
            "#,
        );
        assert_eq!(buffer.dump(), expected);
    }

    fn trim(s: &str) -> String {
        let left_margin = s
            .lines()
            .filter(|l| !l.is_empty())
            .nth(0)
            .unwrap()
            .char_indices()
            .find(|(_, c)| !c.is_whitespace())
            .map(|(i, _)| i)
            .unwrap_or(0);

        let leading_pipe = s
            .lines()
            .nth(1)
            .map(|s| s.trim().starts_with('|'))
            .unwrap_or(false);

        let trailing_pipe = s
            .lines()
            .nth(1)
            .map(|s| s.trim().ends_with('|'))
            .unwrap_or(false);

        s.lines()
            .skip(1)
            .map(|l| l.chars().skip(left_margin).collect::<String>())
            .map(|l| {
                if leading_pipe && l.starts_with('|') {
                    l[1..].to_owned()
                } else {
                    l.to_owned()
                }
            })
            .map(|l| {
                if trailing_pipe && l.ends_with('|') {
                    l[..l.len() - 1].to_owned()
                } else {
                    l.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
