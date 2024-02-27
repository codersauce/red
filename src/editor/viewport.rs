use crate::buffer::Buffer;

#[derive(Debug)]
pub struct Viewport<'a> {
    width: usize,
    height: usize,
    left: usize,
    wrap: bool,
    buffer: &'a Buffer,
}

impl<'a> Viewport<'a> {
    pub fn new(width: usize, height: usize, buffer: &'a Buffer) -> Self {
        Self {
            width,
            height,
            left: 0,
            wrap: true,
            buffer,
        }
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
        self.left = 0;
    }

    pub fn set_left(&mut self, left: usize) {
        self.left = left;
        self.wrap = false;
    }

    pub fn get(&self, line: usize) -> anyhow::Result<Option<ViewportLine>> {
        if line >= self.height {
            return Err(anyhow::anyhow!(
                "requested line {line} but viewport only has {} lines",
                self.height
            ));
        }

        let contents = self.buffer.contents();
        let mut logical_line = 1;
        let mut current_line = 0;
        let mut current_line_len = 0;
        let mut line_contents = String::new();
        let mut did_wrap = false;
        let mut at_beginning = true;

        let mut pos = 0;
        loop {
            if pos >= contents.len() {
                break;
            }

            let c = contents.chars().nth(pos);
            match c {
                Some('\n') => {
                    if current_line == line {
                        break;
                    }
                    current_line += 1;
                    logical_line += 1;
                    current_line_len = 0;
                    at_beginning = true;
                }
                Some(mut c) => {
                    if at_beginning {
                        at_beginning = false;
                        pos += self.left;
                        c = match contents.chars().nth(pos) {
                            Some(c) => c,
                            None => break,
                        };
                    }
                    if current_line == line {
                        line_contents.push(c);
                        if line_contents.len() == self.width - self.left {
                            break;
                        }
                    }

                    current_line_len += 1;
                    let next_char_is_newline = contents.chars().nth(pos + 1) == Some('\n');
                    if self.wrap && current_line_len == self.width && !next_char_is_newline {
                        did_wrap = true;
                        current_line_len = 0;
                        current_line += 1;
                    }
                }
                None => break,
            }

            pos += 1;
        }

        let line_contents = format!("{line_contents:<width$}", width = self.width - self.left);
        let line = ViewportLine::new(logical_line, line_contents, did_wrap);
        Ok(Some(line))
    }
}

#[derive(Debug)]
pub struct ViewportLine {
    num: usize,
    contents: String,
    wrapped: bool,
}

impl ViewportLine {
    pub fn new(num: usize, contents: String, truncated: bool) -> Self {
        Self {
            num,
            contents,
            wrapped: truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::buffer::Buffer;

    use super::*;

    #[test]
    fn test_viewport_with_wrap() -> anyhow::Result<()> {
        let lines = r#"
        ....|....1....|....2....|....3....|....4....|....5....|....6
        Hello, my dear and beloved friend! I hope you are doing well today.
        This is a test of the viewport.
        "#
        .trim();

        let lines: Vec<_> = lines.lines().map(|s| s.trim()).collect();
        let buffer = Buffer::new(None, lines.join("\n"));
        let mut viewport = Viewport::new(60, 3, &buffer);
        viewport.set_wrap(true);

        let line = viewport.get(0)?.unwrap();
        assert!(!line.wrapped);
        assert_eq!(line.num, 1);
        assert_eq!(
            line.contents,
            "....|....1....|....2....|....3....|....4....|....5....|....6".to_string()
        );

        let line = viewport.get(1)?.unwrap();
        assert!(!line.wrapped);
        assert_eq!(line.num, 2);
        assert_eq!(
            line.contents,
            "Hello, my dear and beloved friend! I hope you are doing well".to_string()
        );

        let line = viewport.get(2)?.unwrap();
        assert!(line.wrapped);
        assert_eq!(line.num, 2);
        assert_eq!(
            line.contents,
            " today.                                                     ".to_string()
        );
        assert!(viewport.get(3).is_err());

        Ok(())
    }

    #[test]
    fn test_viewport_with_scroll() -> anyhow::Result<()> {
        let lines = r#"
        ....|....1....|....2....|....3....|....4....|....5....|....6
        Hello, my dear and beloved friend! I hope you are doing well today.
        This is a test of the viewport.
        "#
        .trim();

        let lines: Vec<_> = lines.lines().map(|s| s.trim()).collect();
        let buffer = Buffer::new(None, lines.join("\n"));
        let mut viewport = Viewport::new(60, 3, &buffer);
        viewport.set_wrap(false);

        let line = viewport.get(0)?.unwrap();
        assert_eq!(
            line.contents,
            "....|....1....|....2....|....3....|....4....|....5....|....6".to_string()
        );

        let line = viewport.get(1)?.unwrap();
        assert_eq!(
            line.contents,
            "Hello, my dear and beloved friend! I hope you are doing well".to_string()
        );

        let line = viewport.get(2)?.unwrap();
        assert_eq!(
            line.contents,
            "This is a test of the viewport.                             ".to_string()
        );

        Ok(())
    }

    #[test]
    fn test_viewport_with_scroll_and_offset() -> anyhow::Result<()> {
        let lines = r#"
        ....|....1....|....2....|....3....|....4....|....5....|....6
        Hello, my dear and beloved friend! I hope you are doing well today.
        This is a test of the viewport.
        "#
        .trim();

        let lines: Vec<_> = lines.lines().map(|s| s.trim()).collect();
        let buffer = Buffer::new(None, lines.join("\n"));
        let mut viewport = Viewport::new(60, 3, &buffer);
        viewport.set_left(10);

        let line = viewport.get(0)?.unwrap();
        assert_eq!(
            line.contents,
            "....|....2....|....3....|....4....|....5....|....6".to_string()
        );

        let line = viewport.get(1)?.unwrap();
        assert_eq!(line.contents.len(), 50);
        assert_eq!(
            line.contents,
            "dear and beloved friend! I hope you are doing well".to_string()
        );

        Ok(())
    }
}
