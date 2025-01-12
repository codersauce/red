use ropey::Rope;
use std::path::Path;

use path_absolutize::Absolutize;

use crate::lsp::LspClient;

/// Buffer represents an editable text buffer, which may be associated with a file.
/// It maintains the text content as a rope data structure for efficient editing operations.
#[derive(Debug)]
pub struct Buffer {
    /// Optional path to the file this buffer represents
    pub file: Option<String>,

    /// The text content stored as a rope for efficient editing
    content: Rope,

    /// Whether the buffer has unsaved changes
    pub dirty: bool,

    /// Current cursor position as (x, y) coordinates
    pub pos: (usize, usize),

    /// Top line number of the viewport (for scrolling)
    pub vtop: usize,
}

impl Buffer {
    /// Creates a new Buffer instance with the given file path and contents
    pub fn new(file: Option<String>, contents: String) -> Self {
        Self {
            file,
            content: Rope::from_str(&contents),
            dirty: false,
            pos: (0, 0),
            vtop: 0,
        }
    }

    /// Creates a new Buffer by reading contents from a file
    pub async fn from_file(
        lsp: &mut Box<dyn LspClient>,
        file: Option<String>,
    ) -> anyhow::Result<Self> {
        match &file {
            Some(file) => {
                let path = Path::new(file);
                if !path.exists() {
                    return Err(anyhow::anyhow!("file {:?} not found", file));
                }
                let contents = std::fs::read_to_string(file)?;
                lsp.did_open(file, &contents).await?;
                Ok(Self::new(Some(file.to_string()), contents))
            }
            None => Ok(Self::new(file, "\n".to_string())),
        }
    }

    /// Gets the full contents of the buffer as a single string
    pub fn contents(&self) -> String {
        self.content.to_string()
    }

    /// Saves the buffer contents to its associated file
    pub fn save(&mut self) -> anyhow::Result<String> {
        if let Some(file) = &self.file {
            let contents = self.contents();
            std::fs::write(file, &contents)?;
            self.dirty = false;
            let message = format!(
                "{:?} {}L, {}B written",
                file,
                self.len(),
                contents.as_bytes().len()
            );
            Ok(message)
        } else {
            Err(anyhow::anyhow!("No file name"))
        }
    }

    /// Saves the buffer contents to a new file path
    pub fn save_as(&mut self, new_file_name: &str) -> anyhow::Result<String> {
        let contents = self.contents();
        std::fs::write(new_file_name, &contents)?;
        self.dirty = false;
        self.file = Some(new_file_name.to_string());
        let message = format!(
            "{:?} {}L, {}B written",
            new_file_name,
            self.len(),
            contents.as_bytes().len()
        );
        Ok(message)
    }

    pub fn name(&self) -> &str {
        self.file.as_deref().unwrap_or("[No Name]")
    }

    pub fn uri(&self) -> anyhow::Result<Option<String>> {
        let Some(file) = &self.file else {
            return Ok(None);
        };
        Ok(Some(format!(
            "file://{}",
            Path::new(&file).absolutize()?.to_string_lossy()
        )))
    }

    /// Gets a line from the buffer by line number
    pub fn get(&self, line: usize) -> Option<String> {
        if line >= self.len() {
            return None;
        }
        Some(self.content.line(line).to_string())
    }

    /// Sets the content of a line
    pub fn set(&mut self, line: usize, content: String) {
        if line >= self.len() {
            return;
        }
        let start_byte = self.content.line_to_byte(line);
        let end_byte = if line + 1 < self.len() {
            self.content.line_to_byte(line + 1)
        } else {
            self.content.len_bytes()
        };
        self.content.remove(start_byte..end_byte);
        self.content.insert(start_byte, &content);
        self.dirty = true;
    }

    /// Gets the number of lines in the buffer
    pub fn len(&self) -> usize {
        self.content.len_lines()
    }

    /// Returns true if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.content.len_bytes() == 0
    }

    /// Inserts a string at the given position
    pub fn insert_str(&mut self, x: usize, y: usize, s: &str) {
        let byte_idx = self.pos_to_byte(x, y);
        self.content.insert(byte_idx, s);
        self.dirty = true;
    }

    /// Inserts a character at the given position
    pub fn insert(&mut self, x: usize, y: usize, c: char) {
        let byte_idx = self.pos_to_byte(x, y);
        self.content.insert_char(byte_idx, c);
        self.dirty = true;
    }

    /// Removes a character at the given position
    pub fn remove(&mut self, x: usize, y: usize) {
        let byte_idx = self.pos_to_byte(x, y);
        if byte_idx < self.content.len_bytes() {
            self.content.remove(byte_idx..byte_idx + 1);
            self.dirty = true;
        }
    }

    /// Inserts a new line at the given line number
    pub fn insert_line(&mut self, y: usize, content: String) {
        let byte_idx = if y >= self.len() {
            self.content.len_bytes()
        } else {
            self.content.line_to_byte(y)
        };
        self.content.insert(byte_idx, &format!("{}\n", content));
        self.dirty = true;
    }

    /// Removes a line at the given line number
    pub fn remove_line(&mut self, line: usize) {
        if line >= self.len() {
            return;
        }
        let start_byte = self.content.line_to_byte(line);
        let end_byte = if line + 1 < self.len() {
            self.content.line_to_byte(line + 1)
        } else {
            self.content.len_bytes()
        };
        self.content.remove(start_byte..end_byte);
        self.dirty = true;
    }

    /// Replaces a line with new content
    pub fn replace_line(&mut self, line: usize, new_line: String) {
        if line >= self.len() {
            return;
        }
        let start_byte = self.content.line_to_byte(line);
        let end_byte = if line + 1 < self.len() {
            self.content.line_to_byte(line + 1)
        } else {
            self.content.len_bytes()
        };
        self.content.remove(start_byte..end_byte);
        self.content.insert(start_byte, &format!("{}\n", new_line));
        self.dirty = true;
    }

    /// Gets a portion of the buffer for viewport rendering
    pub fn viewport(&self, vtop: usize, vheight: usize) -> String {
        let height = std::cmp::min(vtop + vheight, self.len());
        let mut result = String::new();
        for i in vtop..height {
            result.push_str(&self.content.line(i).to_string());
        }
        result
    }

    /// Checks if a position is within a word
    pub fn is_in_word(&self, (x, y): (usize, usize)) -> bool {
        if let Some(line) = self.get(y) {
            if x >= line.len() {
                return false;
            }
            let c = line.chars().nth(x).unwrap();
            c.is_alphanumeric() || c == '_'
        } else {
            false
        }
    }

    /// Finds the end of the current word
    pub fn find_word_end(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let line = self.get(y)?;
        let mut x = x;
        let chars = line.chars().skip(x);
        for c in chars {
            if x >= line.len() {
                return Some((x, y));
            }
            if !c.is_alphanumeric() && c != '_' {
                return Some((x, y));
            }
            x += 1;
        }
        Some((x, y))
    }

    /// Finds the start of the current word
    pub fn find_word_start(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let line = self.get(y)?;
        let mut x = x;
        let chars = line.chars().rev().skip(line.len() - x);
        for c in chars {
            if x == 0 {
                return Some((x, y));
            }
            if !c.is_alphanumeric() && c != '_' {
                return Some((x, y));
            }
            x -= 1;
        }
        Some((x, y))
    }

    /// Finds the next word from the current position
    pub fn find_next_word(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_end((x, y))?;
        let line = self.get(y)?;
        let mut line = line[x..].to_string();

        loop {
            for c in line.chars() {
                if c.is_alphanumeric() || c == '_' {
                    return Some((x, y));
                }
                x += 1;
            }
            x = 0;
            y += 1;
            if y >= self.len() {
                return None;
            }
            line = self.get(y)?;
        }
    }

    /// Finds the previous word from the current position
    pub fn find_prev_word(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_start((x, y))?;

        loop {
            if x == 0 && y == 0 {
                return None;
            }

            if let Some(pos) = self.pos_left_of(x, y) {
                x = pos.0;
                y = pos.1;
            } else {
                return None;
            }

            if self.is_in_word((x, y)) {
                return self.find_word_start((x, y));
            }
        }
    }

    /// Finds the next occurrence of a search query
    pub fn find_next(&self, query: &str, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_end((x, y))?;

        loop {
            if y >= self.len() {
                return None;
            }

            let line = self.get(y)?;
            if let Some(pos) = line[x..].find(query) {
                return Some((pos + x, y));
            }

            x = 0;
            y += 1;
        }
    }

    /// Finds the previous occurrence of a search query
    pub fn find_prev(&self, query: &str, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_start((x, y))?;

        loop {
            if y >= self.len() {
                return None;
            }

            let line = self.get(y)?;
            if let Some(pos) = line[..x].rfind(query) {
                return Some((pos, y));
            }

            if y == 0 {
                return None;
            }

            y -= 1;
            x = self.get(y)?.len();
        }
    }

    /// Deletes the word at the current position
    pub fn delete_word(&mut self, (x, y): (usize, usize)) {
        let Some(start) = self.find_word_start((x, y)) else {
            return;
        };
        let Some(end) = self.find_word_end((x, y)) else {
            return;
        };

        let start_byte = self.pos_to_byte(start.0, start.1);
        let end_byte = self.pos_to_byte(end.0, end.1);
        self.content.remove(start_byte..end_byte);
        self.dirty = true;
    }

    /// Returns whether the buffer has unsaved changes
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    // Helper method to convert (x,y) coordinates to byte index
    fn pos_to_byte(&self, x: usize, y: usize) -> usize {
        if y >= self.len() {
            return self.content.len_bytes();
        }
        let line_start = self.content.line_to_byte(y);
        let line = self.content.line(y);
        let x = std::cmp::min(x, line.len_chars());
        line_start + line.char_to_byte(x)
    }

    // Helper method to find the position to the left
    fn pos_left_of(&self, x: usize, y: usize) -> Option<(usize, usize)> {
        let mut x = x;
        let mut y = y;

        loop {
            if x == 0 {
                if y == 0 {
                    return None;
                }
                y -= 1;
                x = self.get(y)?.len();
            }

            if x == 0 {
                continue;
            }

            x -= 1;
            if let Some(line) = self.get(y) {
                if x < line.len() {
                    return Some((x, y));
                }
            }
        }
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

        assert_eq!(buffer.viewport(0, 2), "a\nb\n");
    }

    #[test]
    fn test_viewport_with_small_buffer() {
        let buffer = Buffer::new(Some("sample".to_string()), "a\nb".to_string());
        assert_eq!(buffer.viewport(0, 5), "a\nb");
    }

    #[test]
    fn test_is_in_word() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        assert!(buffer.is_in_word((0, 0)));
        assert!(buffer.is_in_word((1, 0)));
        assert!(buffer.is_in_word((2, 0)));
        assert!(!buffer.is_in_word((3, 0)));
        assert!(!buffer.is_in_word((7, 0)));
        assert!(!buffer.is_in_word((8, 0)));
    }

    #[test]
    fn test_find_word_end() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        let word_end = buffer.find_word_end((0, 0));
        assert_eq!(word_end.unwrap(), (3, 0));

        let word_end = buffer.find_word_end((3, 0));
        assert_eq!(word_end.unwrap(), (3, 0));

        let word_end = buffer.find_word_end((4, 0));
        assert_eq!(word_end.unwrap(), (7, 0));

        let word_end = buffer.find_word_end((7, 0));
        assert_eq!(word_end.unwrap(), (7, 0));
    }

    #[test]
    fn test_find_word_start() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        let word_start = buffer.find_word_start((0, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        let word_start = buffer.find_word_start((2, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        let word_start = buffer.find_word_start((1, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        let word_start = buffer.find_word_start((3, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        let word_start = buffer.find_word_start((4, 0));
        assert_eq!(word_start.unwrap(), (4, 0));

        let word_start = buffer.find_word_start((7, 0));
        assert_eq!(word_start.unwrap(), (4, 0));

        let word_start = buffer.find_word_start((5, 1));
        assert_eq!(word_start.unwrap(), (4, 1));
    }
}
