use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// Calculate the display width of a string in terminal columns
pub fn display_width(s: &str) -> usize {
    s.width()
}

/// Calculate the display width of a single character
pub fn char_display_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Convert a byte offset to a display column position
/// Returns the column number (0-based) where the character at the given byte offset would appear
pub fn byte_to_column(line: &str, byte_offset: usize) -> usize {
    let byte_offset = byte_offset.min(line.len());
    let prefix = &line[..byte_offset];
    display_width(prefix)
}

/// Convert a display column position to a byte offset
/// Returns the byte offset of the character that contains the given column
pub fn column_to_byte(line: &str, target_column: usize) -> usize {
    let mut current_column = 0;

    for (idx, ch) in line.char_indices() {
        let char_width = char_display_width(ch);
        if current_column + char_width > target_column {
            // Target column is within this character
            return idx;
        }
        current_column += char_width;
    }

    // Target column is at or beyond the end of the string
    line.len()
}

/// Convert a character index to a byte offset
pub fn char_to_byte(line: &str, char_idx: usize) -> usize {
    line.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(line.len())
}

/// Convert a byte offset to a character index
pub fn byte_to_char(line: &str, byte_offset: usize) -> usize {
    let byte_offset = byte_offset.min(line.len());
    line[..byte_offset].chars().count()
}

/// Count the number of grapheme clusters in a string
pub fn grapheme_len(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Get the nth grapheme cluster from a string
pub fn nth_grapheme(s: &str, n: usize) -> Option<&str> {
    s.graphemes(true).nth(n)
}

/// Move to the next grapheme cluster boundary
/// Returns the byte offset of the next grapheme boundary, or None if at the end
pub fn next_grapheme_boundary(s: &str, byte_offset: usize) -> Option<usize> {
    let graphemes: Vec<(usize, &str)> = s.grapheme_indices(true).collect();

    // Find which grapheme contains our byte offset
    for i in 0..graphemes.len() {
        let (start, _grapheme) = graphemes[i];
        let end = if i + 1 < graphemes.len() {
            graphemes[i + 1].0
        } else {
            s.len()
        };

        if byte_offset >= start && byte_offset < end {
            // We're inside this grapheme, return its end
            return Some(end);
        }
    }

    // We're at or past the end
    None
}

/// Move to the previous grapheme cluster boundary
/// Returns the byte offset of the previous grapheme, or None if at the beginning
pub fn prev_grapheme_boundary(s: &str, byte_offset: usize) -> Option<usize> {
    let graphemes: Vec<(usize, &str)> = s.grapheme_indices(true).collect();

    // Find the grapheme that contains or is after our position
    for i in (0..graphemes.len()).rev() {
        if graphemes[i].0 < byte_offset {
            return Some(graphemes[i].0);
        }
    }

    // We're at the beginning
    None
}

/// Calculate the display column of a character at a given character index
pub fn char_to_column(line: &str, char_idx: usize) -> usize {
    line.chars().take(char_idx).map(char_display_width).sum()
}

/// Find the character index that contains the given display column
pub fn column_to_char(line: &str, target_column: usize) -> usize {
    let mut current_column = 0;

    for (idx, ch) in line.chars().enumerate() {
        let char_width = char_display_width(ch);
        if current_column + char_width > target_column {
            // Target column is within this character
            return idx;
        }
        current_column += char_width;
    }

    // Return the character count if column is beyond the line
    line.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("你好"), 4); // CJK characters are 2 columns each
        assert_eq!(display_width("👋"), 2); // Emoji is 2 columns
        assert_eq!(display_width("café"), 4); // Combining character
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn test_char_display_width() {
        assert_eq!(char_display_width('a'), 1);
        assert_eq!(char_display_width('中'), 2);
        assert_eq!(char_display_width('👋'), 2);
        assert_eq!(char_display_width('\t'), 0); // Tab has no intrinsic width
    }

    #[test]
    fn test_byte_to_column() {
        let line = "hello世界";
        assert_eq!(byte_to_column(line, 0), 0);
        assert_eq!(byte_to_column(line, 5), 5); // After "hello"
        assert_eq!(byte_to_column(line, 8), 7); // After first CJK char (3 bytes)
        assert_eq!(byte_to_column(line, 11), 9); // End of string
    }

    #[test]
    fn test_column_to_byte() {
        let line = "hello世界";
        assert_eq!(column_to_byte(line, 0), 0);
        assert_eq!(column_to_byte(line, 5), 5);
        assert_eq!(column_to_byte(line, 6), 5); // Middle of CJK char rounds to start
        assert_eq!(column_to_byte(line, 7), 8); // Start of second CJK char
        assert_eq!(column_to_byte(line, 9), 11); // End of string
        assert_eq!(column_to_byte(line, 20), 11); // Beyond end
    }

    #[test]
    fn test_grapheme_operations() {
        // Test with combining characters
        let s = "e\u{0301}"; // é as e + combining acute
        assert_eq!(grapheme_len(s), 1);
        assert_eq!(nth_grapheme(s, 0), Some("e\u{0301}"));

        // Test with emoji
        let s = "👨‍👩‍👧‍👦"; // Family emoji with ZWJ
        assert_eq!(grapheme_len(s), 1);
        assert_eq!(display_width(s), 2);
    }

    #[test]
    fn test_grapheme_boundaries() {
        let s = "a👋b";
        assert_eq!(next_grapheme_boundary(s, 0), Some(1));
        assert_eq!(next_grapheme_boundary(s, 1), Some(5)); // Skip emoji bytes
        assert_eq!(next_grapheme_boundary(s, 5), Some(6));
        assert_eq!(next_grapheme_boundary(s, 6), None);

        assert_eq!(prev_grapheme_boundary(s, 6), Some(5));
        assert_eq!(prev_grapheme_boundary(s, 5), Some(1));
        assert_eq!(prev_grapheme_boundary(s, 1), Some(0));
        assert_eq!(prev_grapheme_boundary(s, 0), None);
    }

    #[test]
    fn test_char_column_conversions() {
        let line = "hello世界";
        assert_eq!(char_to_column(line, 0), 0);
        assert_eq!(char_to_column(line, 5), 5);
        assert_eq!(char_to_column(line, 6), 7); // After first CJK char
        assert_eq!(char_to_column(line, 7), 9); // After second CJK char

        assert_eq!(column_to_char(line, 0), 0);
        assert_eq!(column_to_char(line, 5), 5);
        assert_eq!(column_to_char(line, 6), 5); // Middle of CJK char
        assert_eq!(column_to_char(line, 7), 6);
        assert_eq!(column_to_char(line, 9), 7);
    }
}
