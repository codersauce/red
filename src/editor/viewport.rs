use crate::buffer::Buffer;

pub struct Viewport<'a> {
    width: usize,
    height: usize,
    wrap: bool,
    buffer: &'a Buffer,
}

impl<'a> Viewport<'a> {
    pub fn new(width: usize, height: usize, buffer: &'a Buffer) -> Self {
        Self {
            width,
            height,
            wrap: true,
            buffer,
        }
    }

    pub fn get(&self, line: usize) -> anyhow::Result<Option<String>> {
        if line >= self.height {
            return Err(anyhow::anyhow!(
                "requested line {line} but viewport only has {} lines",
                self.height
            ));
        }

        let contents = self.buffer.contents();
        let mut current_line = 0;
        let mut current_line_len = 0;
        let mut line_contents = String::new();

        let mut pos = 0;
        loop {
            if pos >= contents.len() {
                break;
            }

            let c = contents.chars().nth(pos);
            println!("{current_line} pos: {pos} c: {:?}", c);
            match c {
                Some('\n') => {
                    if current_line == line {
                        break;
                    }
                    current_line += 1;
                    current_line_len = 0;
                }
                Some(c) => {
                    if current_line == line {
                        line_contents.push(c);
                        if line_contents.len() == self.width {
                            println!("Got the line, break");
                            break;
                        }
                    }

                    current_line_len += 1;
                    let next_char_is_newline = contents.chars().nth(pos + 1) == Some('\n');
                    if current_line_len == self.width && !next_char_is_newline {
                        println!("Moving to next line");
                        current_line_len = 0;
                        current_line += 1;
                    }
                }
                None => break,
            }

            pos += 1;
        }

        println!("line_contents: {}", line_contents);
        Ok(Some(format!("{line_contents:<width$}", width = self.width)))
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
        let viewport = Viewport::new(60, 3, &buffer);
        assert_eq!(
            viewport.get(0)?,
            Some("....|....1....|....2....|....3....|....4....|....5....|....6".to_string())
        );
        assert_eq!(
            viewport.get(1)?,
            Some("Hello, my dear and beloved friend! I hope you are doing well".to_string())
        );
        assert_eq!(
            viewport.get(2)?,
            Some(" today.                                                     ".to_string())
        );
        assert!(viewport.get(3).is_err());

        Ok(())
    }
}
