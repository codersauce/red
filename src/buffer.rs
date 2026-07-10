use ropey::Rope;
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use path_absolutize::Absolutize;
use regex::Regex;

use crate::undo::{TextPosition, TextRange, UndoHistory};
use crate::unicode_utils::{char_to_column, column_to_char, display_width, trim_line_ending};
use crate::utils::expand_user_path;

const FIRST_BUFFER_ID: u64 = 1;
static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(FIRST_BUFFER_ID);

/// Stable identity for one in-memory buffer.
///
/// Unlike a buffer's position in `Editor::buffers`, this value does not change when
/// another buffer is closed. It is process-local and is not a persistence identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferId(u64);

impl BufferId {
    fn next() -> Self {
        const BUFFER_ID_INCREMENT: u64 = 1;
        Self(NEXT_BUFFER_ID.fetch_add(BUFFER_ID_INCREMENT, /*order*/ Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub start_x: usize,
    pub start_y: usize,
    pub end_x: usize,
    pub end_y: usize,
}

/// Buffer represents an editable text buffer, which may be associated with a file.
/// It maintains the text content as a rope data structure for efficient editing operations.
#[derive(Debug)]
pub struct Buffer {
    /// Stable process-local identity.
    id: BufferId,

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

    /// Buffer-local undo and redo history.
    pub undo_history: UndoHistory,

    /// Monotonic content revision used by render caches.
    revision: u64,
}

impl Buffer {
    /// Creates a new Buffer instance with the given file path and contents
    pub fn new(file: Option<String>, contents: String) -> Self {
        let contents = if contents.is_empty() && file.is_none() {
            "\n".to_string()
        } else {
            contents
        };

        Self {
            id: BufferId::next(),
            file,
            content: Rope::from_str(&contents),
            dirty: false,
            pos: (0, 0),
            vtop: 0,
            undo_history: UndoHistory::default(),
            revision: 0,
        }
    }

    /// Creates a new Buffer by reading contents from a file
    pub async fn from_file(file: Option<String>) -> anyhow::Result<Self> {
        match &file {
            Some(file) => {
                let path = expand_user_path(file)?;
                if !path.exists() {
                    return Err(anyhow::anyhow!("file {:?} not found", file));
                }

                let contents = std::fs::read_to_string(&path)?;
                let file = path.to_string_lossy().into_owned();

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

                Ok(Self::new(Some(file), contents))
            }
            None => Ok(Self::new(file, "\n".to_string())),
        }
    }

    pub async fn load_or_create(file: Option<String>) -> anyhow::Result<Self> {
        match &file {
            Some(file) => {
                let path = expand_user_path(file)?;
                let resolved_file = path.to_string_lossy().into_owned();
                if !path.exists() {
                    if std::fs::symlink_metadata(&path).is_ok() {
                        return Err(anyhow::anyhow!("file {:?} not found", resolved_file));
                    }

                    return Ok(Self::new(Some(resolved_file), "\n".to_string()));
                }

                let contents = std::fs::read_to_string(&path)?;

                Ok(Self::new(Some(resolved_file), contents))
            }
            None => Ok(Self::new(file, "\n".to_string())),
        }
    }

    pub fn reload_from_file(&mut self) -> anyhow::Result<String> {
        let Some(file) = self.file.clone() else {
            return Err(anyhow::anyhow!("No file name"));
        };

        let path = expand_user_path(&file)?;
        if !path.exists() {
            return Err(anyhow::anyhow!("file {:?} not found", file));
        }

        let contents = std::fs::read_to_string(&path)?;
        let byte_count = contents.len();

        self.content = Rope::from_str(&contents);
        self.file = Some(path.to_string_lossy().into_owned());
        self.undo_history = UndoHistory::default();
        self.dirty = false;
        self.revision = self.revision.wrapping_add(1);

        Ok(format!(
            "{:?} {}L, {}B read",
            self.file.as_deref().unwrap_or(&file),
            self.len(),
            byte_count
        ))
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

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Reconstructs an in-memory buffer from a trusted, versioned session snapshot.
    /// This never reads or writes the associated file.
    pub fn from_session_snapshot(
        file: Option<String>,
        contents: String,
        dirty: bool,
        revision: u64,
        undo_history: UndoHistory,
    ) -> Self {
        let mut buffer = Self::new(file, contents);
        buffer.dirty = dirty;
        buffer.revision = revision;
        buffer.undo_history = undo_history;
        buffer
    }

    pub fn id(&self) -> BufferId {
        self.id
    }

    fn byte_to_position(&self, byte: usize) -> (usize, usize) {
        let byte = byte.min(self.content.len_bytes());
        let char_index = self.content.byte_to_char(byte);
        let line = self.content.char_to_line(char_index);
        let line_start = self.content.line_to_char(line);
        (char_index.saturating_sub(line_start), line)
    }

    pub fn regex_matches(&self, regex: &Regex) -> Vec<SearchMatch> {
        let contents = self.contents();
        regex
            .find_iter(&contents)
            .filter(|match_| match_.start() != match_.end())
            .map(|match_| {
                let (start_x, start_y) = self.byte_to_position(match_.start());
                let (end_x, end_y) = self.byte_to_position(match_.end());
                SearchMatch {
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                }
            })
            .collect()
    }

    fn mark_changed(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Saves the buffer contents to its associated file
    pub fn save(&mut self) -> anyhow::Result<String> {
        if let Some(file) = self.file.clone() {
            let path = expand_user_path(&file)?;
            let file = path.to_string_lossy().into_owned();
            let contents = self.contents();
            std::fs::write(&path, &contents)?;
            self.file = Some(file.clone());
            self.mark_saved();
            let message = format!("{:?} {}L, {}B written", file, self.len(), contents.len());
            Ok(message)
        } else {
            Err(anyhow::anyhow!("No file name"))
        }
    }

    /// Saves the buffer contents to a new file path
    pub fn save_as(&mut self, new_file_name: &str) -> anyhow::Result<String> {
        let path = expand_user_path(new_file_name)?;
        let file = path.to_string_lossy().into_owned();
        let contents = self.contents();
        std::fs::write(&path, &contents)?;
        self.file = Some(file.clone());
        self.mark_saved();
        let message = format!("{:?} {}L, {}B written", file, self.len(), contents.len());
        Ok(message)
    }

    pub fn name(&self) -> &str {
        self.file.as_deref().unwrap_or("[No Name]")
    }

    pub fn uri(&self) -> anyhow::Result<Option<String>> {
        let Some(file) = &self.file else {
            return Ok(None);
        };
        let file = expand_user_path(file)?;
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
        if line > self.len() {
            return;
        }
        let start_char = self.content.line_to_char(line);
        let end_char = if line + 1 < self.content.len_lines() {
            self.content.line_to_char(line + 1)
        } else {
            self.content.len_chars()
        };
        self.content.remove(start_char..end_char);
        self.content.insert(start_char, &content);
        self.mark_changed();
    }

    /// Gets the number of lines in the buffer
    pub fn len(&self) -> usize {
        self.content.len_lines() - 1
    }

    pub fn last_navigable_line(&self) -> usize {
        let last_line = self.len();
        if last_line > 0 && self.get(last_line).is_some_and(|line| line.is_empty()) {
            last_line - 1
        } else {
            last_line
        }
    }

    pub fn navigable_line_count(&self) -> usize {
        self.last_navigable_line() + 1
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
        self.mark_changed();
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
        self.mark_changed();
    }

    /// Removes a character at the given position
    pub fn remove(&mut self, x: usize, y: usize) {
        let char_idx = self.xy_to_char_idx(x, y);
        if char_idx < self.content.len_chars() {
            // rope.remove expects character indices, not byte indices!
            self.content.remove(char_idx..char_idx + 1);
            self.mark_changed();
        }
    }

    pub fn remove_range(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        let start_char = self.xy_to_char_idx(x0, y0);
        let end_char = self.xy_to_char_idx(x1, y1);
        self.content.remove(start_char..end_char);
        self.mark_changed();
    }

    pub fn text_in_range(&self, range: TextRange) -> String {
        let start_char = self.position_to_char_idx(range.start);
        let end_char = self.position_to_char_idx(range.end);
        self.content
            .get_slice(start_char..end_char)
            .map(|slice| slice.to_string())
            .unwrap_or_default()
    }

    pub fn replace_range_raw(&mut self, range: TextRange, text: &str) {
        let start_char = self.position_to_char_idx(range.start);
        let end_char = self.position_to_char_idx(range.end);
        self.content.remove(start_char..end_char);
        self.content.insert(start_char, text);
        self.mark_changed();
    }

    pub fn range_for_text(&self, start: TextPosition, text: &str) -> TextRange {
        let mut line = start.line;
        let mut character = start.character;

        for c in text.chars() {
            if c == '\n' {
                line += 1;
                character = 0;
            } else {
                character += 1;
            }
        }

        TextRange::new(start, TextPosition::new(line, character))
    }

    pub fn position_to_char_idx(&self, position: TextPosition) -> usize {
        if position.line >= self.content.len_lines() {
            return self.content.len_chars();
        }

        let line_start = self.content.line_to_char(position.line);
        let line = self.content.line(position.line).to_string();
        let line_len = trim_line_ending(&line).chars().count();
        line_start + position.character.min(line_len)
    }

    pub fn char_idx_to_position(&self, char_index: usize) -> TextPosition {
        let char_index = char_index.min(self.content.len_chars());
        let line = self.content.char_to_line(char_index);
        let line_start = self.content.line_to_char(line);
        TextPosition::new(line, char_index.saturating_sub(line_start))
    }

    /// Inserts a new line at the given line number
    pub fn insert_line(&mut self, y: usize, content: String) {
        let char_idx = if y >= self.content.len_lines() {
            self.content.len_chars()
        } else {
            self.content.line_to_char(y)
        };
        self.content.insert(char_idx, &format!("{}\n", content));
        self.mark_changed();
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
        self.mark_changed();
    }

    /// Replaces a line with new content
    pub fn replace_line(&mut self, line: usize, new_line: String) {
        if line > self.len() {
            return;
        }
        let start_char = self.content.line_to_char(line);
        let end_char = if line + 1 < self.content.len_lines() {
            self.content.line_to_char(line + 1)
        } else {
            self.content.len_chars()
        };
        self.content.remove(start_char..end_char);
        self.content.insert(start_char, &format!("{}\n", new_line));
        self.mark_changed();
    }

    /// Gets a portion of the buffer for viewport rendering
    pub fn viewport(&self, vtop: usize, vheight: usize) -> String {
        let height = std::cmp::min(vtop + vheight, self.navigable_line_count());
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
            if x >= line.chars().count() {
                // Move to next line if at end
                y += 1;
                x = 0;
                if y > self.len() {
                    return None;
                }
                continue;
            }

            let current_char = line.chars().nth(x)?;
            let current_type = Self::get_char_type(current_char);

            // Skip current word/sequence
            let line_len = line.chars().count();
            while x < line_len {
                let c = line.chars().nth(x)?;
                if Self::get_char_type(c) != current_type {
                    break;
                }
                x += 1;
            }

            // Skip whitespace
            while x < line_len {
                let c = line.chars().nth(x)?;
                if !c.is_whitespace() {
                    return Some((x, y));
                }
                x += 1;
            }

            // If we reach end of line, continue to next line
            if x >= line_len {
                y += 1;
                x = 0;
                if y > self.len() {
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
        let line_len = line.chars().count();
        for c in chars {
            if x >= line_len {
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
        let line = trim_line_ending(&line);

        // Check if we're at the last character of the buffer
        let line_len = Self::char_len(line);
        if y >= self.len() && x >= line_len.saturating_sub(1) {
            return None;
        }

        // If we're on an empty line now, move to start of next line
        // without doing anything else
        if line.is_empty() {
            y += 1;
            if y > self.len() {
                return None;
            }
            return Some((0, y));
        }

        // If we're at the end of current line, move to next line
        if x >= line_len {
            y += 1;
            if y > self.len() {
                return None;
            }
            x = 0;
            let next_line = self.get(y)?;
            let next_line = trim_line_ending(&next_line);
            if next_line.is_empty() {
                return Some((0, y));
            }
            // Find first non-whitespace on next line
            if let Some(i) = Self::first_non_whitespace_char(next_line) {
                return Some((i, y));
            }
        }

        let current_line = self.get(y)?;
        let current_line = trim_line_ending(&current_line);
        if current_line.is_empty() {
            return Some((0, y));
        }

        let line_len = Self::char_len(current_line);
        let last_char_position = line_len.checked_sub(1).map(|last_x| (last_x, y));

        if x < line_len {
            let start_type = Self::char_type_at(current_line, x)?;
            x += 1;

            for (i, current_type) in Self::char_types(current_line).skip(x) {
                if start_type == CharType::Whitespace {
                    break;
                }
                if current_type != start_type {
                    break;
                }
                x = i + 1;
            }
        }

        for (i, current_type) in Self::char_types(current_line).skip(x) {
            if current_type != CharType::Whitespace {
                return Some((i, y));
            }
        }

        y += 1;
        if y > self.len() {
            return last_char_position;
        }

        // Find first non-whitespace on next line
        let next_line = self.get(y)?;
        let next_line = trim_line_ending(&next_line);
        if let Some(i) = Self::first_non_whitespace_char(next_line) {
            return Some((i, y));
        }

        Some((0, y))
    }

    /// Finds the previous word from the current position
    pub fn find_prev_word(&self, (mut x, mut y): (usize, usize)) -> Option<(usize, usize)> {
        // Get current line
        let line = self.get(y)?;
        let line = trim_line_ending(&line);

        // Check if we're at start of buffer
        if y == 0 && x == 0 {
            return None;
        }

        let line_len = Self::char_len(line);

        // If we're at the end of line, move back one
        if x >= line_len {
            x = line_len.saturating_sub(1);
        }

        // Move one character backward
        if x == 0 {
            // Move to end of previous line
            if y == 0 {
                return None;
            }
            y -= 1;
            let prev_line = self.get(y)?;
            let prev_line = trim_line_ending(&prev_line);
            if prev_line.is_empty() {
                return Some((0, y));
            }
            x = Self::char_len(prev_line) - 1;
        } else {
            x -= 1;
        }

        let current_line = self.get(y)?;
        let current_line = trim_line_ending(&current_line);

        // Get the type of character we landed on
        let start_type = Self::char_type_at(current_line, x)?;

        // Skip whitespace backward
        if start_type == CharType::Whitespace {
            x = match Self::last_non_whitespace_at_or_before(current_line, x) {
                Some(x) => x,
                None => {
                    if y == 0 {
                        return None;
                    }
                    y -= 1;
                    let prev_line = self.get(y)?;
                    let prev_line = trim_line_ending(&prev_line);
                    if prev_line.is_empty() {
                        return Some((0, y));
                    }
                    Self::last_non_whitespace_at_or_before(
                        prev_line,
                        Self::char_len(prev_line).saturating_sub(1),
                    )?
                }
            };

            // If we hit start of line while skipping whitespace, go to previous line
            if x == 0 && Self::char_type_at(current_line, 0)? == CharType::Whitespace {
                if y == 0 {
                    return None;
                }
                y -= 1;
                let prev_line = self.get(y)?;
                let prev_line = trim_line_ending(&prev_line);
                if prev_line.is_empty() {
                    return Some((0, y));
                }
                x = Self::last_non_whitespace_at_or_before(
                    prev_line,
                    Self::char_len(prev_line).saturating_sub(1),
                )?;
            }
        }

        let current_line = self.get(y)?;
        let current_line = trim_line_ending(&current_line);
        let current_type = Self::char_type_at(current_line, x)?;

        // Move backward to start of current word/symbol
        x = Self::word_start_at_or_before(current_line, x, current_type);

        // If we're at start of line, check previous line
        if x == 0 && y > 0 {
            let prev_line = self.get(y - 1)?;
            if trim_line_ending(&prev_line).is_empty() {
                return Some((0, y - 1));
            }
        }

        Some((x, y))
    }

    /// Finds the next occurrence of a search query
    pub fn find_next(&self, query: &str, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_end((x, y))?;

        loop {
            if y > self.len() {
                return None;
            }

            let line = self.get(y)?;
            let suffix = crate::unicode_utils::char_suffix(&line, x);
            if let Some(pos) = suffix.find(query) {
                let prefix_chars = suffix[..pos].chars().count();
                return Some((prefix_chars + x, y));
            }

            x = 0;
            y += 1;
        }
    }

    /// Finds the previous occurrence of a search query
    pub fn find_prev(&self, query: &str, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = (x, y);

        loop {
            if y > self.len() {
                return None;
            }

            let line = self.get(y)?;
            let prefix = crate::unicode_utils::char_prefix(&line, x.min(line.chars().count()));
            if let Some(pos) = prefix.rfind(query) {
                return Some((prefix[..pos].chars().count(), y));
            }

            if y == 0 {
                return None;
            }

            y -= 1;
            x = self.get(y)?.chars().count();
        }
    }

    /// Deletes the word at the current position
    pub fn delete_word(&mut self, (x, y): (usize, usize)) -> Option<String> {
        let start = (x, y);
        let end = self.find_next_word((x, y))?;

        let start_char = self.xy_to_char_idx(start.0, start.1);
        let end_char = self.xy_to_char_idx(end.0, end.1);

        // Get the text before removing (need to use byte indices for slice)
        let result = self
            .content
            .get_slice(start_char..end_char)
            .map(|s| s.to_string());

        self.content.remove(start_char..end_char);
        self.mark_changed();

        result
    }

    /// Returns whether the buffer has unsaved changes
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn refresh_dirty_from_history(&mut self) {
        self.dirty = self.undo_history.is_dirty();
    }

    pub fn mark_saved(&mut self) {
        self.undo_history.mark_saved();
        self.refresh_dirty_from_history();
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

        // Handle line endings - Ropey includes newlines in char count.
        let mut line_chars_without_newline = line_chars;
        if line_chars_without_newline > 0 && line.char(line_chars_without_newline - 1) == '\n' {
            line_chars_without_newline -= 1;
        }
        if line_chars_without_newline > 0 && line.char(line_chars_without_newline - 1) == '\r' {
            line_chars_without_newline -= 1;
        }

        // Clamp x to valid range
        let x = x.min(line_chars_without_newline);

        line_start_char + x
    }

    /// Get the display width of a line
    pub fn line_display_width(&self, y: usize) -> usize {
        if let Some(line) = self.get(y) {
            display_width(trim_line_ending(&line))
        } else {
            0
        }
    }

    /// Convert a display column to a character index
    pub fn column_to_char_index(&self, column: usize, y: usize) -> usize {
        if let Some(line) = self.get(y) {
            let line = trim_line_ending(&line);
            column_to_char(line, column)
        } else {
            0
        }
    }

    /// Convert a character index to a display column
    pub fn char_index_to_column(&self, char_idx: usize, y: usize) -> usize {
        if let Some(line) = self.get(y) {
            let line = trim_line_ending(&line);
            char_to_column(line, char_idx)
        } else {
            0
        }
    }

    fn get_char_type(c: char) -> CharType {
        if c.is_whitespace() {
            CharType::Whitespace
        } else if c.is_alphanumeric() || c == '_' {
            CharType::Word
        } else if c.is_ascii_punctuation() {
            CharType::Punctuation
        } else {
            CharType::Symbol
        }
    }

    fn char_len(line: &str) -> usize {
        line.chars().count()
    }

    fn char_types(line: &str) -> impl Iterator<Item = (usize, CharType)> + '_ {
        line.chars()
            .enumerate()
            .map(|(i, c)| (i, Self::get_char_type(c)))
    }

    fn char_type_at(line: &str, x: usize) -> Option<CharType> {
        line.chars().nth(x).map(Self::get_char_type)
    }

    fn first_non_whitespace_char(line: &str) -> Option<usize> {
        Self::char_types(line)
            .find(|(_, kind)| *kind != CharType::Whitespace)
            .map(|(i, _)| i)
    }

    fn last_non_whitespace_at_or_before(line: &str, x: usize) -> Option<usize> {
        let mut index = Self::char_len(line);
        for c in line.chars().rev() {
            index = index.saturating_sub(1);
            if index > x {
                continue;
            }
            if Self::get_char_type(c) != CharType::Whitespace {
                return Some(index);
            }
        }
        None
    }

    fn word_start_at_or_before(line: &str, x: usize, target_type: CharType) -> usize {
        let mut start = x;
        let mut index = Self::char_len(line);
        for c in line.chars().rev() {
            index = index.saturating_sub(1);
            if index > x {
                continue;
            }
            if Self::get_char_type(c) != target_type {
                break;
            }
            start = index;
        }
        start
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharType {
    Whitespace,
    Word,
    Punctuation,
    Symbol,
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("red-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn test_home_dir() -> PathBuf {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("HOME or USERPROFILE should be set for tests")
    }

    #[tokio::test]
    async fn load_or_create_expands_home_paths() {
        let home = test_home_dir();
        let dir_name = format!(".red-load-home-{}", uuid::Uuid::new_v4());
        let dir = home.join(&dir_name);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        fs::write(&file, "theme = \"kanso\"\n").unwrap();

        let buffer = Buffer::load_or_create(Some(format!("~/{dir_name}/config.toml")))
            .await
            .unwrap();

        assert_eq!(buffer.contents(), "theme = \"kanso\"\n");
        assert_eq!(buffer.file, Some(file.to_string_lossy().into_owned()));

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_or_create_reads_through_symlinked_directory() {
        let root = unique_temp_dir("symlink-open");
        let real_dir = root.join("real");
        let link_dir = root.join("link");
        fs::create_dir_all(&real_dir).unwrap();
        symlink(&real_dir, &link_dir).unwrap();
        fs::write(real_dir.join("config.toml"), "theme = \"latte\"\n").unwrap();

        let link_path = link_dir.join("config.toml");
        let buffer = Buffer::load_or_create(Some(link_path.to_string_lossy().into_owned()))
            .await
            .unwrap();

        assert_eq!(buffer.contents(), "theme = \"latte\"\n");
        assert_eq!(buffer.file, Some(link_path.to_string_lossy().into_owned()));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_through_symlinked_directory() {
        let root = unique_temp_dir("symlink-save");
        let real_dir = root.join("real");
        let link_dir = root.join("link");
        fs::create_dir_all(&real_dir).unwrap();
        symlink(&real_dir, &link_dir).unwrap();

        let link_path = link_dir.join("config.toml");
        let mut buffer = Buffer::new(
            Some(link_path.to_string_lossy().into_owned()),
            "theme = \"zen\"\n".to_string(),
        );

        buffer.save().unwrap();

        assert_eq!(
            fs::read_to_string(real_dir.join("config.toml")).unwrap(),
            "theme = \"zen\"\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn load_and_save_preserves_empty_file() {
        let root = unique_temp_dir("empty-file");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("empty.txt");
        fs::write(&file, "").unwrap();

        let mut buffer = Buffer::load_or_create(Some(file.to_string_lossy().into_owned()))
            .await
            .unwrap();

        assert_eq!(buffer.contents(), "");

        buffer.save().unwrap();

        assert_eq!(fs::read(&file).unwrap(), b"");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_or_create_errors_for_broken_symlink() {
        let root = unique_temp_dir("broken-symlink");
        fs::create_dir_all(&root).unwrap();
        let link_path = root.join("config.toml");
        symlink(root.join("missing.toml"), &link_path).unwrap();

        let err = Buffer::load_or_create(Some(link_path.to_string_lossy().into_owned()))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("not found"));

        fs::remove_file(link_path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

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
    fn test_find_next_word_matches_nvim_delimiter_boundaries() {
        let buffer = Buffer::new(None, "foo:bar baz".to_string());

        assert_eq!(buffer.find_next_word((0, 0)), Some((3, 0))); // foo -> :
        assert_eq!(buffer.find_next_word((1, 0)), Some((3, 0))); // oo -> :
        assert_eq!(buffer.find_next_word((2, 0)), Some((3, 0))); // final o -> :
        assert_eq!(buffer.find_next_word((3, 0)), Some((4, 0))); // : -> bar
        assert_eq!(buffer.find_next_word((4, 0)), Some((8, 0))); // bar -> baz
        assert_eq!(buffer.find_next_word((7, 0)), Some((8, 0))); // space -> baz
    }

    #[test]
    fn test_find_next_word_matches_nvim_generic_delimiters() {
        let buffer = Buffer::new(None, "Option<Result<T, E>> rest".to_string());

        assert_eq!(buffer.find_next_word((0, 0)), Some((6, 0))); // Option -> <
        assert_eq!(buffer.find_next_word((5, 0)), Some((6, 0))); // final n -> <
        assert_eq!(buffer.find_next_word((6, 0)), Some((7, 0))); // < -> Result
        assert_eq!(buffer.find_next_word((12, 0)), Some((13, 0))); // final t -> <
        assert_eq!(buffer.find_next_word((17, 0)), Some((18, 0))); // E -> >>
        assert_eq!(buffer.find_next_word((18, 0)), Some((21, 0))); // >> -> rest
        assert_eq!(buffer.find_next_word((19, 0)), Some((21, 0))); // final > -> rest
    }

    #[test]
    fn test_find_next_word_moves_from_prefix_punctuation_to_keyword() {
        let buffer = Buffer::new(None, "&Config::path".to_string());

        assert_eq!(buffer.find_next_word((0, 0)), Some((1, 0))); // & -> Config
        assert_eq!(buffer.find_next_word((6, 0)), Some((7, 0))); // final g -> ::
        assert_eq!(buffer.find_next_word((7, 0)), Some((9, 0))); // :: -> path
    }

    #[test]
    fn test_find_next_word_treats_digits_as_keyword_chars() {
        let buffer = Buffer::new(None, "value123 next".to_string());

        assert_eq!(buffer.find_next_word((0, 0)), Some((9, 0)));
        assert_eq!(buffer.find_next_word((4, 0)), Some((9, 0)));
        assert_eq!(buffer.find_next_word((7, 0)), Some((9, 0)));
    }

    #[test]
    fn test_find_next_word_moves_to_eof_like_nvim() {
        let buffer = Buffer::new(None, "final".to_string());

        assert_eq!(buffer.find_next_word((0, 0)), Some((4, 0)));
        assert_eq!(buffer.find_next_word((3, 0)), Some((4, 0)));
        assert_eq!(buffer.find_next_word((4, 0)), None);
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
    fn test_find_prev_search_skips_current_match_start() {
        let buffer = Buffer::new(None, "alpha beta alpha gamma alpha".to_string());

        assert_eq!(buffer.find_prev("alpha", (23, 0)), Some((11, 0)));
        assert_eq!(buffer.find_prev("alpha", (11, 0)), Some((0, 0)));
    }

    #[test]
    fn test_file_end() {
        let buffer = Buffer::new(None, "a\nb\nc".to_string());
        assert_eq!(buffer.get(3), None);
    }

    #[test]
    fn crlf_line_endings_do_not_count_toward_display_positions() {
        let buffer = Buffer::new(None, "abc\r\ndef\r\n".to_string());

        assert_eq!(buffer.line_display_width(0), 3);
        assert_eq!(buffer.column_to_char_index(3, 0), 3);
        assert_eq!(buffer.char_index_to_column(3, 0), 3);
        assert_eq!(buffer.position_to_char_idx(TextPosition::new(0, 99)), 3);
    }

    #[test]
    fn revision_advances_only_when_content_changes() {
        let mut buffer = Buffer::new(None, "abc".to_string());
        let initial_revision = buffer.revision();

        buffer.insert(1, 0, 'x');
        assert_eq!(buffer.revision(), initial_revision + 1);

        let changed_revision = buffer.revision();
        buffer.remove(99, 0);
        assert_eq!(buffer.revision(), changed_revision);

        buffer.remove(1, 0);
        assert_eq!(buffer.revision(), changed_revision + 1);
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
