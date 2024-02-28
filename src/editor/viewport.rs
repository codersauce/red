use crate::{highlighter::Highlighter, log, theme::Theme};

use super::{RenderBuffer, StyleInfo};

pub struct Viewport<'a> {
    width: usize,
    height: usize,
    top: usize,
    left: usize,
    wrap: bool,
    contents: &'a str,
    theme: &'a Theme,
    highlighter: Highlighter,
}

impl<'a> Viewport<'a> {
    pub fn new(
        theme: &'a Theme,
        width: usize,
        height: usize,
        left: usize,
        top: usize,
        contents: &'a str,
    ) -> anyhow::Result<Self> {
        let highlighter = Highlighter::new(theme)?;

        Ok(Self {
            width,
            height,
            top,
            left,
            wrap: true,
            contents,
            theme,
            highlighter,
        })
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
        self.left = 0;
    }

    pub fn set_top(&mut self, top: usize) {
        self.top = top;
    }

    pub fn set_left(&mut self, left: usize) {
        self.left = left;
        self.wrap = false;
    }

    pub fn draw(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize) -> anyhow::Result<()> {
        let styles = self.highlighter.highlight(&self.contents)?;

        let mut x = x;
        let mut y = y;
        let mut pos = 0;
        let mut current_line = 1;

        let mut wrapped = false;
        let mut print_line = true;

        let max_line_number_len = format!("{}", self.contents.lines().count()).len();

        loop {
            if print_line {
                let line_padding =
                    " ".repeat(self.width.saturating_sub(max_line_number_len + x - 2));
                buffer.set_text(x, y, &line_padding, &self.theme.style);

                x = 0;

                let line_content = if wrapped {
                    "".to_string()
                } else {
                    current_line.to_string()
                };
                let line = format!(" {line_content:>width$} ", width = max_line_number_len);
                log!("{x} {y} [{line}]");
                buffer.set_text(x, y, &line, &self.theme.gutter_style);
                x += line.len();
                print_line = false;
            }

            let Some(c) = self.contents.chars().nth(pos) else {
                break;
            };

            let style = styles
                .iter()
                .find(|s| s.contains(pos))
                .map(|s| &s.style)
                .unwrap_or(&self.theme.style);
            pos += 1;

            if c == '\n' {
                y += 1;

                if y >= self.height {
                    break;
                }

                print_line = true;
                wrapped = false;
                current_line += 1;
                continue;
            }

            log!("{x} {y} [{c}]");
            buffer.set_char(x, y, c, style);
            x += 1;

            if x >= self.width {
                if self.wrap {
                    // if wrap, we continue on this line but advance the y
                    y += 1;
                    wrapped = true;
                    print_line = true;
                } else {
                    // if not wrap, we need to advance to after the next \n
                    let next_newline = self.contents[pos..].find('\n');
                    if let Some(next_newline) = next_newline {
                        pos += next_newline;
                        y += 1;
                        wrapped = false;
                        print_line = true;
                        current_line += 1;
                    } else {
                        break;
                    }
                }
            }
        }

        while y < self.height {
            let line = " ".repeat(self.width);
            buffer.set_text(0, y, &line, &self.theme.style);
            y += 1;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_viewport() {
        let theme = Theme::builder()
            .style("#ffffff", "#000000")
            .gutter("#000000", "#ffffff")
            .scope("keyword", "#000001", "#000000")
            .build();

        let code = trim(
            r#"
pub fn draw(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize) -> anyhow::Result<()> {
    let styles = self.highlighter.highlight(&self.contents)?;

    let mut x = 0;
    let mut y = 0;
    for (pos, c) in self.contents.chars().enumerate() {
        let style = styles
            .iter()
            .find(|s| s.contains(pos))
            .map(|s| &s.style)
            .unwrap_or(&self.theme.style);

        buffer.set_char(x + pos, y, c, style);
    }
    Ok(())
}
            "#,
        );

        let mut viewport = Viewport::new(&theme, 43, 5, 0, 0, &code).unwrap();
        let mut buffer = RenderBuffer::new(40, 5, theme.style.clone());
        viewport.draw(&mut buffer, 0, 0).unwrap();

        let expected = trim(
            r#"
             1 pub fn draw(&mut self, buffer: &mut Rende
             2 rBuffer, x: usize, y: usize) -> anyhow::R
             3 esult<()> {
             4     let styles = self.highlighter.highlig
             5 ht(&self.contents)?;
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

        s.lines()
            .skip(1)
            .map(|l| l.chars().skip(left_margin).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
