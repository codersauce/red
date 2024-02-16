use std::path::Path;

use crate::lsp::LspClient;

#[derive(Debug)]
pub struct Buffer {
    pub file: Option<String>,
    pub lines: Vec<String>,
}

impl Buffer {
    pub fn new(file: Option<String>, contents: String) -> Self {
        let lines = contents.lines().map(|s| s.to_string()).collect();
        Self { file, lines }
    }

    pub async fn from_file(lsp: &mut LspClient, file: Option<String>) -> anyhow::Result<Self> {
        match &file {
            Some(file) => {
                let path = Path::new(file);
                if !path.exists() {
                    return Err(anyhow::anyhow!("file {:?} not found", file));
                }
                let contents = std::fs::read_to_string(file)?;
                lsp.did_open(file, &contents).await?;
                Ok(Self::new(Some(file.to_string()), contents.to_string()))
            }
            None => Ok(Self::new(file, String::new())),
        }
    }

    pub fn get(&self, line: usize) -> Option<String> {
        if self.lines.len() > line {
            return Some(self.lines[line].clone());
        }

        None
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn insert(&mut self, x: usize, y: usize, c: char) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).insert(x as usize, c);
        }
    }

    /// removes a character from the buffer
    pub fn remove(&mut self, x: usize, y: usize) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).remove(x as usize);
        }
    }

    pub fn insert_line(&mut self, y: usize, content: String) {
        self.lines.insert(y, content);
    }

    pub fn remove_line(&mut self, line: usize) {
        if self.len() > line {
            self.lines.remove(line);
        }
    }

    pub fn viewport(&self, vtop: usize, vheight: usize) -> String {
        let height = std::cmp::min(vtop + vheight, self.lines.len());
        self.lines[vtop..height].join("\n")
    }

    fn find_next_word(&self, position: (usize, usize)) -> Option<(usize, usize)> {
        let lines = &self.lines;
        let (mut y, mut x) = position;

        // Ensure we start within the bounds of the text.
        if y >= lines.len() {
            return None;
        }

        // Indicates whether we're currently scanning through a word.
        let mut in_word = false;

        while y < lines.len() {
            let line = &lines[y];
            // Adjust iterator based on the current line and position.
            let chars_iter = line.chars().enumerate().skip(x);

            for (i, c) in chars_iter {
                let is_word_char = c.is_alphanumeric() || c == '_';

                if in_word {
                    if !is_word_char {
                        // We've found the end of the current word; return the start of the next "word".
                        return Some((y, i));
                    }
                } else {
                    if is_word_char {
                        // We've found the start of a word, mark as in_word and look for the end.
                        in_word = true;
                    } else if i > x {
                        // If we are not in a word and find a non-word character after the initial position,
                        // this is our stop point.
                        return Some((y, i));
                    }
                }
            }

            // If we reach the end of a line while in a word, we need to continue to the next line.
            // If not in a word, reset in_word for the new line.
            in_word = false;
            y += 1;
            x = 0; // Reset x to start at the beginning of the next line.
        }

        // If we exit the loop, it means we've reached the end of the text without finding another stop point.
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_viewport() {
        let buffer = Buffer::new(
            Some("sample".to_string()),
            "a\nb\nc\nd\n\ne\n\nf".to_string(),
        );

        assert_eq!(buffer.viewport(0, 2), "a\nb");
    }

    #[test]
    fn test_viewport_with_small_buffer() {
        let buffer = Buffer::new(Some("sample".to_string()), "a\nb".to_string());
        assert_eq!(buffer.viewport(0, 5), "a\nb");
    }

    #[test]
    fn test_find_next_word() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        let line = buffer.get(0).unwrap()[4..].to_string();
        println!("line: {}", line);
        let next_word = buffer.find_next_word((0, 4));
        let line = buffer.get(next_word.unwrap().0).unwrap()[next_word.unwrap().1..].to_string();
        assert_eq!(line, "::{");
    }
}
