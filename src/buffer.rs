use ropey::Rope;
use std::path::Path;

use path_absolutize::Absolutize;

use crate::lsp::LspClient;
use crate::unicode_utils::{char_to_column, column_to_char, display_width};

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
        let contents = if contents.is_empty() {
            "\n".to_string()
        } else {
            contents
        };

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

                // Debug: Check for emoji in loaded content
                if contents
                    .chars()
                    .any(|c| c as u32 >= 0x1F300 && c as u32 <= 0x1F9FF)
                {
                    crate::log!(
                        "from_file: Loaded file contains emoji. First 100 chars: {:?}",
                        &contents.chars().take(100).collect::<String>()
                    );
                }

                lsp.did_open(file, &contents).await?;

                Ok(Self::new(Some(file.to_string()), contents))
            }
            None => Ok(Self::new(file, "\n".to_string())),
        }
    }

    pub async fn load_or_create(
        lsp: &mut Box<dyn LspClient>,
        file: Option<String>,
    ) -> anyhow::Result<Self> {
        match &file {
            Some(file) => {
                let path = Path::new(file);
                if !path.exists() {
                    return Ok(Self::new(Some(file.to_string()), "\n".to_string()));
                }

                let contents = std::fs::read_to_string(file)?;
                lsp.did_open(file, &contents).await?;

                Ok(Self::new(Some(file.to_string()), contents))
            }
            None => Ok(Self::new(file, "\n".to_string())),
        }
    }

    /// Gets the file type based on the file extension
    pub fn file_type(&self) -> Option<String> {
        // TODO: use PathBuf?
        self.file.as_ref().and_then(|file| {
            file.split('.')
                .next_back()
                .map(|ext| ext.to_string().to_lowercase())
        })
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
            let message = format!("{:?} {}L, {}B written", file, self.len(), contents.len());
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
            contents.len()
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
        if line > self.len() {
            return None;
        }
        Some(self.content.line(line).to_string())
    }

    /// Sets the content of a line
    pub fn set(&mut self, line: usize, content: String) {
        if line >= self.len() {
            return;
        }
        let start_char = self.content.line_to_char(line);
        let end_char = if line + 1 < self.len() {
            self.content.line_to_char(line + 1)
        } else {
            self.content.len_chars()
        };
        self.content.remove(start_char..end_char);
        self.content.insert(start_char, &content);
        self.dirty = true;
    }

    /// Gets the number of lines in the buffer
    pub fn len(&self) -> usize {
        self.content.len_lines() - 1
    }

    /// Returns true if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.content.len_bytes() == 0
    }

    /// Inserts a string at the given position
    pub fn insert_str(&mut self, x: usize, y: usize, s: &str) {
        // Calculate the character index within the rope
        let char_idx = self.xy_to_char_idx(x, y);
        self.content.insert(char_idx, s);
        self.dirty = true;
    }

    /// Inserts a character at the given position
    pub fn insert(&mut self, x: usize, y: usize, c: char) {
        use crate::log;

        let char_idx = self.xy_to_char_idx(x, y);
        let total_chars = self.content.len_chars();

        log!(
            "Buffer::insert - x: {}, y: {}, char: '{}', char_idx: {}, total_chars: {}",
            x,
            y,
            c,
            char_idx,
            total_chars
        );

        if char_idx > total_chars {
            log!(
                "ERROR: char_idx {} exceeds total_chars {}! Clamping to end.",
                char_idx,
                total_chars
            );
            self.content.insert_char(total_chars, c);
        } else {
            self.content.insert_char(char_idx, c);
        }
        self.dirty = true;
    }

    /// Removes a character at the given position
    pub fn remove(&mut self, x: usize, y: usize) {
        let char_idx = self.xy_to_char_idx(x, y);
        if char_idx < self.content.len_chars() {
            // rope.remove expects character indices, not byte indices!
            self.content.remove(char_idx..char_idx + 1);
            self.dirty = true;
        }
    }

    pub fn remove_range(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        let start_char = self.xy_to_char_idx(x0, y0);
        let end_char = self.xy_to_char_idx(x1, y1);
        self.content.remove(start_char..end_char);
        self.dirty = true;
    }

    /// Inserts a new line at the given line number
    pub fn insert_line(&mut self, y: usize, content: String) {
        let char_idx = if y >= self.content.len_lines() {
            self.content.len_chars()
        } else {
            self.content.line_to_char(y)
        };
        self.content.insert(char_idx, &format!("{}\n", content));
        self.dirty = true;
    }

    /// Removes a line at the given line number
    pub fn remove_line(&mut self, line: usize) {
        if line >= self.content.len_lines() {
            return;
        }
        let start_char = self.content.line_to_char(line);
        let end_char = if line + 1 < self.content.len_lines() {
            self.content.line_to_char(line + 1)
        } else {
            self.content.len_chars()
        };
        self.content.remove(start_char..end_char);
        self.dirty = true;
    }

    /// Replaces a line with new content
    pub fn replace_line(&mut self, line: usize, new_line: String) {
        if line >= self.len() {
            return;
        }
        let start_char = self.content.line_to_char(line);
        let end_char = if line + 1 < self.len() {
            self.content.line_to_char(line + 1)
        } else {
            self.content.len_chars()
        };
        self.content.remove(start_char..end_char);
        self.content.insert(start_char, &format!("{}\n", new_line));
        self.dirty = true;
    }

    /// Gets a portion of the buffer for viewport rendering
    pub fn viewport(&self, vtop: usize, vheight: usize) -> String {
        let height = std::cmp::min(vtop + vheight, self.content.len_lines());
        let mut result = String::new();
        for i in vtop..height {
            result.push_str(&self.content.line(i).to_string());
        }
        result
    }

    /// Checks if a position is within a word
    /// Note: x is a character index, not a display column
    pub fn is_in_word(&self, (x, y): (usize, usize)) -> bool {
        if let Some(line) = self.get(y) {
            if x >= line.chars().count() {
                return false;
            }
            let c = line.chars().nth(x).unwrap();
            c.is_alphanumeric() || c == '_'
        } else {
            false
        }
    }

    /// Finds the start of the current word
    pub fn find_word_start(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let mut x = x;
        let mut y = y;

        loop {
            let line = self.get(y)?;
            if x >= line.len() {
                // Move to next line if at end
                y += 1;
                x = 0;
                if y >= self.len() {
                    return None;
                }
                continue;
            }

            let current_char = line.chars().nth(x)?;
            let current_type = Self::get_char_type(current_char);

            // Skip current word/sequence
            while x < line.len() {
                let c = line.chars().nth(x)?;
                if Self::get_char_type(c) != current_type {
                    break;
                }
                x += 1;
            }

            // Skip whitespace
            while x < line.len() {
                let c = line.chars().nth(x)?;
                if !c.is_whitespace() {
                    return Some((x, y));
                }
                x += 1;
            }

            // If we reach end of line, continue to next line
            if x >= line.len() {
                y += 1;
                x = 0;
                if y >= self.len() {
                    return None;
                }
            }
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

    /// Finds the next word from the current position
    pub fn find_next_word(&self, (mut x, mut y): (usize, usize)) -> Option<(usize, usize)> {
        // Get current line
        let line = self.get(y)?;

        // Check if we're at the last character of the buffer
        if y >= self.len().saturating_sub(1) && x >= line.len().saturating_sub(1) {
            return None;
        }

        // If we're on an empty line now, move to start of next line
        // without doing anything else
        if line.is_empty() {
            y += 1;
            if y >= self.len() {
                return None;
            }
            return Some((0, y));
        }

        let chars: Vec<char> = line.chars().collect();

        // If we're at the end of current line, move to next line
        if x >= chars.len() {
            y += 1;
            if y >= self.len() {
                return None;
            }
            x = 0;
            let next_line = self.get(y)?;
            if next_line.is_empty() {
                return Some((0, y));
            }
            // Find first non-whitespace on next line
            let chars = next_line.chars().collect::<Vec<char>>();
            for (i, &c) in chars.iter().enumerate() {
                if Self::get_char_type(c) != CharType::Whitespace {
                    return Some((i, y));
                }
            }
        } else {
            // Only move forward if we're not already at end of line
            x += 1;
        }

        let current_line = self.get(y)?;
        if current_line.is_empty() {
            return Some((0, y));
        }

        let chars = current_line.chars().collect::<Vec<char>>();
        let start_type = if x < chars.len() {
            Self::get_char_type(chars[x])
        } else {
            CharType::Whitespace
        };

        if start_type != CharType::Whitespace {
            while x < chars.len() {
                let current_type = Self::get_char_type(chars[x]);
                if current_type != start_type {
                    break;
                }
                x += 1;
            }
        }

        while x < chars.len() {
            let current_type = Self::get_char_type(chars[x]);
            if current_type != CharType::Whitespace {
                return Some((x, y));
            }
            x += 1;
        }

        y += 1;
        if y >= self.len() {
            return None;
        }

        // Find first non-whitespace on next line
        let next_line = self.get(y)?;
        let chars = next_line.chars().collect::<Vec<char>>();
        for (i, &c) in chars.iter().enumerate() {
            if Self::get_char_type(c) != CharType::Whitespace {
                return Some((i, y));
            }
        }

        Some((0, y))
    }

    /// Finds the previous word from the current position
    pub fn find_prev_word(&self, (mut x, mut y): (usize, usize)) -> Option<(usize, usize)> {
        // Get current line
        let line = self.get(y)?;

        // Check if we're at start of buffer
        if y == 0 && x == 0 {
            return None;
        }

        let chars: Vec<char> = line.chars().collect();

        // If we're at the end of line, move back one
        if x >= chars.len() {
            x = chars.len().saturating_sub(1);
        }

        // Move one character backward
        if x == 0 {
            // Move to end of previous line
            if y == 0 {
                return None;
            }
            y -= 1;
            let prev_line = self.get(y)?;
            if prev_line.is_empty() {
                return Some((0, y));
            }
            let prev_chars: Vec<char> = prev_line.chars().collect();
            x = prev_chars.len() - 1;
        } else {
            x -= 1;
        }

        let current_line = self.get(y)?;
        let chars: Vec<char> = current_line.chars().collect();

        // Get the type of character we landed on
        let start_type = Self::get_char_type(chars[x]);

        // Skip whitespace backward
        if start_type == CharType::Whitespace {
            while x > 0 && Self::get_char_type(chars[x]) == CharType::Whitespace {
                x -= 1;
            }

            // If we hit start of line while skipping whitespace, go to previous line
            if x == 0 && Self::get_char_type(chars[0]) == CharType::Whitespace {
                if y == 0 {
                    return None;
                }
                y -= 1;
                let prev_line = self.get(y)?;
                if prev_line.is_empty() {
                    return Some((0, y));
                }
                let prev_chars: Vec<char> = prev_line.chars().collect();
                x = prev_chars.len() - 1;
                while x > 0 && Self::get_char_type(prev_chars[x]) == CharType::Whitespace {
                    x -= 1;
                }
            }
        }

        let current_line = self.get(y)?;
        let chars: Vec<char> = current_line.chars().collect();
        let current_type = Self::get_char_type(chars[x]);

        // Move backward to start of current word/symbol
        while x > 0 {
            let prev_type = Self::get_char_type(chars[x - 1]);
            if prev_type != current_type {
                break;
            }
            x -= 1;
        }

        // If we're at start of line, check previous line
        if x == 0 && y > 0 {
            let prev_line = self.get(y - 1)?;
            if prev_line.is_empty() {
                return Some((0, y - 1));
            }
        }

        Some((x, y))
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
    pub fn delete_word(&mut self, (x, y): (usize, usize)) -> Option<String> {
        let start = (x, y);
        let end = self.find_next_word((x, y))?;

        let start_char = self.xy_to_char_idx(start.0, start.1);
        let end_char = self.xy_to_char_idx(end.0, end.1);

        // Get the text before removing (need to use byte indices for slice)
        let start_byte = self.content.char_to_byte(start_char);
        let end_byte = self.content.char_to_byte(end_char);
        let result = self
            .content
            .get_slice(start_byte..end_byte)
            .map(|s| s.to_string());

        self.content.remove(start_char..end_char);
        self.dirty = true;

        result
    }

    /// Returns whether the buffer has unsaved changes
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    // Helper method to convert (x,y) coordinates to character index in the rope
    fn xy_to_char_idx(&self, x: usize, y: usize) -> usize {
        if y >= self.content.len_lines() {
            return self.content.len_chars();
        }

        // Get the line start character index
        let line_start_char = self.content.line_to_char(y);

        // Get the actual line content to handle the x position correctly
        let line = self.content.line(y);
        let line_chars = line.len_chars();

        // Handle newline - Ropey includes newlines in char count
        let line_chars_without_newline = if line_chars > 0 && line.char(line_chars - 1) == '\n' {
            line_chars - 1
        } else {
            line_chars
        };

        // Clamp x to valid range
        let x = x.min(line_chars_without_newline);

        line_start_char + x
    }

    /// Get the display width of a line
    pub fn line_display_width(&self, y: usize) -> usize {
        if let Some(line) = self.get(y) {
            display_width(line.trim_end_matches('\n'))
        } else {
            0
        }
    }

    /// Convert a display column to a character index
    pub fn column_to_char_index(&self, column: usize, y: usize) -> usize {
        if let Some(line) = self.get(y) {
            let line = line.trim_end_matches('\n');
            column_to_char(line, column)
        } else {
            0
        }
    }

    /// Convert a character index to a display column
    pub fn char_index_to_column(&self, char_idx: usize, y: usize) -> usize {
        if let Some(line) = self.get(y) {
            let line = line.trim_end_matches('\n');
            char_to_column(line, char_idx)
        } else {
            0
        }
    }

    fn get_char_type(c: char) -> CharType {
        if c.is_whitespace() {
            CharType::Whitespace
        } else if c.is_alphabetic() || c == '_' {
            CharType::Word
        } else if c.is_ascii_punctuation() {
            CharType::Punctuation
        } else {
            CharType::Symbol
        }
    }
}

#[derive(Debug, PartialEq)]
enum CharType {
    Whitespace,
    Word,
    Punctuation,
    Symbol,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_find_next_word() {
        let buffer = Buffer::new(
            None,
            [
                "struct Person {".to_string(),
                "    name: String,".to_string(),
                "    age: usize,".to_string(),
                "}".to_string(),
                "".to_string(),
                "fn main() {".to_string(),
                "    let mut person = Person {".to_string(),
                "        name: \"Felipe\".to_string(),".to_string(),
                "        age: 46,".to_string(),
            ]
            .join("\n"),
        );

        // first line
        assert_eq!(buffer.find_next_word((0, 0)), Some((7, 0))); // struct -> Person
        assert_eq!(buffer.find_next_word((7, 0)), Some((14, 0))); // Person -> {
        assert_eq!(buffer.find_next_word((14, 0)), Some((4, 1))); // { -> name:

        // fourth line
        assert_eq!(buffer.find_next_word((0, 3)), Some((0, 4))); // } -> empty line

        // fifth line (empty line)
        assert_eq!(buffer.find_next_word((0, 4)), Some((0, 5))); // empty line -> fn

        // sixth line
        assert_eq!(buffer.find_next_word((0, 5)), Some((3, 5))); // fn -> main
        assert_eq!(buffer.find_next_word((3, 5)), Some((7, 5))); // main -> (
        assert_eq!(buffer.find_next_word((7, 5)), Some((10, 5))); // ( -> skips the closing parens
                                                                  // -> {

        // eighth line
        assert_eq!(buffer.find_next_word((21, 7)), Some((23, 7))); // "Felipe" -> skips the dot -> to_string
    }

    #[test]
    fn test_find_prev_word() {
        let buffer = Buffer::new(
            None,
            [
                "struct Person {".to_string(),
                "    name: String,".to_string(),
                "    age: usize,".to_string(),
                "}".to_string(),
                "".to_string(),
                "fn main() {".to_string(),
                "    let mut person = Person {".to_string(),
                "        name: \"Felipe\".to_string(),".to_string(),
                "        age: 46,".to_string(),
                "    };".to_string(),
                "".to_string(),
                "    println!(\"Hello, {}!\", person.name);".to_string(),
                "".to_string(),
                "    person.age = \"25\";".to_string(),
                "    person.name = \"22\";".to_string(),
                "}".to_string(),
            ]
            .join("\n"),
        );

        assert_eq!(buffer.find_prev_word((0, 15)), Some((21, 14))); // } -> " before ;
        assert_eq!(buffer.find_prev_word((4, 14)), Some((20, 13))); // } -> empty line
        assert_eq!(buffer.find_prev_word((0, 0)), None); // struct -> start of buffer
    }

    #[test]
    fn test_file_end() {
        let buffer = Buffer::new(None, "a\nb\nc".to_string());
        assert_eq!(buffer.get(3), None);
    }

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

        // find_word_start actually finds the start of the NEXT word, not the current word
        // From position (0, 0) which is 'u' in "use", it should find 's' in "std"
        let word_start = buffer.find_word_start((0, 0));
        assert_eq!(word_start.unwrap(), (4, 0)); // 's' in "std"

        let word_start = buffer.find_word_start((2, 0));
        assert_eq!(word_start.unwrap(), (4, 0)); // 's' in "std"

        let word_start = buffer.find_word_start((1, 0));
        assert_eq!(word_start.unwrap(), (4, 0)); // 's' in "std"

        let word_start = buffer.find_word_start((3, 0));
        assert_eq!(word_start.unwrap(), (4, 0)); // space after "use", next word is "std"

        let word_start = buffer.find_word_start((4, 0));
        assert_eq!(word_start.unwrap(), (7, 0)); // From 's' in "std", next is ':'

        let word_start = buffer.find_word_start((7, 0));
        assert_eq!(word_start.unwrap(), (4, 1)); // From ':', skips to 'c' in "collections" on next line

        let word_start = buffer.find_word_start((5, 1));
        assert_eq!(word_start.unwrap(), (15, 1)); // From 'o' in "collections", next is ':' (punctuation)
    }
}
