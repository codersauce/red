//! Explicit conversions among UTF-8 bytes, Unicode scalars, graphemes, and terminal columns.
//!
//! Red intentionally uses different coordinate systems at different boundaries:
//! strings and parser spans use bytes, Ropey edits use scalar indices, the visible
//! cursor uses grapheme indices, and layout uses display columns. Tab-aware helpers also
//! require the starting column or configured tab width where expansion depends on
//! context.
//!
//! Conversion functions clamp or return the nearest representable boundary as described
//! by each API. Passing a value from the wrong coordinate system may work for ASCII and
//! fail only for combining marks, CJK text, or emoji, so callers should never rely on
//! coincident numeric values.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// The side on which hidden terminal text is replaced by an overflow marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationSide {
    /// Preserve the end of the text and place the marker on the left.
    Left,
    /// Preserve the beginning of the text and place the marker on the right.
    Right,
}

/// Calculate the display width of a string in terminal columns
pub fn display_width(s: &str) -> usize {
    s.width()
}

/// Returns the byte boundary before trailing whitespace and the preceding word.
/// Words use the prompt editor's existing whitespace-delimited semantics, and
/// the result always falls on an extended-grapheme boundary.
pub(crate) fn previous_word_start(text: &str) -> usize {
    let mut start = text.len();
    let mut seen_word = false;
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        let whitespace = grapheme.chars().all(char::is_whitespace);
        if seen_word && whitespace {
            break;
        }
        seen_word |= !whitespace;
        start = index;
    }
    start
}

/// Removes the last whitespace-delimited word without splitting a grapheme.
pub(crate) fn delete_last_word(text: &mut String) {
    text.truncate(previous_word_start(text));
}

/// Calculate terminal display width while expanding tabs to the next tab stop.
pub fn display_width_with_tabs(s: &str, tab_width: usize) -> usize {
    display_width_with_tabs_from_column(s, 0, tab_width)
}

pub fn is_printable_ascii(s: &str) -> bool {
    const HIGH_BITS: u64 = 0x8080_8080_8080_8080;
    const SPACE_BYTES: u64 = 0x2020_2020_2020_2020;
    const ONE_BYTES: u64 = 0x0101_0101_0101_0101;

    let mut chunks = s.as_bytes().chunks_exact(std::mem::size_of::<u64>());
    if chunks.any(|chunk| {
        let bytes: [u8; std::mem::size_of::<u64>()] = chunk
            .try_into()
            .expect("chunks_exact always produces full machine words");
        let word = u64::from_ne_bytes(bytes);
        word & HIGH_BITS != 0
            || word.wrapping_sub(SPACE_BYTES) & !word & HIGH_BITS != 0
            || word.wrapping_add(ONE_BYTES) & HIGH_BITS != 0
    }) {
        return false;
    }

    chunks
        .remainder()
        .iter()
        .all(|byte| (0x20..=0x7E).contains(byte))
}

/// Calculate terminal display width from an existing display column.
pub fn display_width_with_tabs_from_column(
    s: &str,
    start_column: usize,
    tab_width: usize,
) -> usize {
    if is_printable_ascii(s) {
        return s.len();
    }
    let tab_width = tab_width.max(1);
    let mut column = start_column;
    for grapheme in s.graphemes(true) {
        if grapheme == "\t" {
            column += tab_width - (column % tab_width);
        } else if grapheme.chars().all(char::is_control) {
            // Terminal control sequences do not occupy display cells.
        } else {
            column += display_width(grapheme);
        }
    }
    column - start_column
}

/// Remove one trailing line ending from a rope line while preserving other
/// trailing whitespace. Handles both LF and CRLF files.
pub fn trim_line_ending(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .or_else(|| line.strip_suffix('\r'))
        .unwrap_or(line)
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
    if is_printable_ascii(line) {
        return target_column.min(line.len());
    }
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
    if line.is_ascii() {
        return char_idx.min(line.len());
    }
    line.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(line.len())
}

/// Convert a grapheme cluster index to a byte offset.
pub fn grapheme_to_byte(line: &str, grapheme_idx: usize) -> usize {
    if line.is_ascii() {
        return grapheme_idx.min(line.len());
    }
    line.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(line.len())
}

/// Convert a byte offset to a grapheme cluster index.
pub fn byte_to_grapheme(line: &str, byte_offset: usize) -> usize {
    let byte_offset = byte_offset.min(line.len());
    if line.is_ascii() {
        return byte_offset;
    }
    line[..byte_offset].graphemes(true).count()
}

/// Convert a grapheme cluster index to Ropey's character index.
pub fn grapheme_to_char(line: &str, grapheme_idx: usize) -> usize {
    byte_to_char(line, grapheme_to_byte(line, grapheme_idx))
}

/// Return the character-index range occupied by one grapheme cluster.
pub fn grapheme_char_range(line: &str, grapheme_idx: usize) -> Option<(usize, usize)> {
    let mut start = 0;
    for (index, grapheme) in line.graphemes(true).enumerate() {
        let end = start + grapheme.chars().count();
        if index == grapheme_idx {
            return Some((start, end));
        }
        start = end;
    }
    None
}

/// Convert Ropey's character index to a grapheme cluster index.
pub fn char_to_grapheme(line: &str, char_idx: usize) -> usize {
    byte_to_grapheme(line, char_to_byte(line, char_idx))
}

/// Slice a string by character indices.
pub fn char_slice(line: &str, start: usize, end: usize) -> &str {
    let start_byte = char_to_byte(line, start);
    let end_byte = char_to_byte(line, end);
    &line[start_byte..end_byte]
}

/// Return the prefix before a character index.
pub fn char_prefix(line: &str, end: usize) -> &str {
    char_slice(line, 0, end)
}

/// Return the suffix starting at a character index.
pub fn char_suffix(line: &str, start: usize) -> &str {
    let start_byte = char_to_byte(line, start);
    &line[start_byte..]
}

/// Truncate a string to at most `max_chars` Unicode scalar values.
pub fn truncate_chars(line: &str, max_chars: usize) -> &str {
    char_prefix(line, max_chars)
}

/// Truncate a string to at most `max_width` terminal display columns.
pub fn truncate_display_width(s: &str, max_width: usize) -> String {
    let mut width = 0;

    for (start, grapheme) in s.grapheme_indices(true) {
        let grapheme_width = display_width(grapheme);
        if width + grapheme_width > max_width {
            return s[..start].to_owned();
        }
        width += grapheme_width;
    }

    s.to_owned()
}

/// Return the longest complete-grapheme suffix that fits `max_width` cells.
pub fn truncate_display_width_from_end(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    let mut start = text.len();
    let mut used = 0;
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        let next = used + display_width(grapheme);
        if next > max_width {
            break;
        }
        used = next;
        start = index;
    }

    text[start..].to_owned()
}

/// Clip at a complete grapheme and display a bounded, caller-selected marker.
///
/// Text that already fits is returned unchanged. The marker is itself clipped
/// to the available terminal cells so zero-width and narrow surfaces are safe.
pub fn truncate_display_width_with_marker(
    text: &str,
    max_width: usize,
    marker: &str,
    side: TruncationSide,
) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(text) <= max_width {
        return text.to_owned();
    }

    let marker = truncate_display_width(marker, max_width);
    let content_width = max_width.saturating_sub(display_width(&marker));
    match side {
        TruncationSide::Left => {
            let mut result = marker;
            result.push_str(&truncate_display_width_from_end(text, content_width));
            result
        }
        TruncationSide::Right => {
            let mut result = truncate_display_width(text, content_width);
            result.push_str(&marker);
            result
        }
    }
}

/// Pad or truncate a string so it occupies exactly `width` display columns.
pub fn fit_display_width(s: &str, width: usize) -> String {
    let mut result = truncate_display_width(s, width);
    result.extend(std::iter::repeat_n(
        ' ',
        width.saturating_sub(display_width(&result)),
    ));
    result
}

/// Convert a byte offset to a character index
pub fn byte_to_char(line: &str, byte_offset: usize) -> usize {
    let byte_offset = byte_offset.min(line.len());
    line[..byte_offset].chars().count()
}

/// Count the number of grapheme clusters in a string
pub fn grapheme_len(s: &str) -> usize {
    if s.is_ascii() {
        // CRLF is the only multi-byte extended grapheme possible in ASCII.
        return s.len() - s.matches("\r\n").count();
    }
    s.graphemes(true).count()
}

/// Get the nth grapheme cluster from a string
pub fn nth_grapheme(s: &str, n: usize) -> Option<&str> {
    s.graphemes(true).nth(n)
}

/// Move to the next grapheme cluster boundary
/// Returns the byte offset of the next grapheme boundary, or None if at the end
pub fn next_grapheme_boundary(s: &str, byte_offset: usize) -> Option<usize> {
    s.grapheme_indices(true)
        .map(|(start, grapheme)| start + grapheme.len())
        .find(|&end| byte_offset < end)
}

/// Move to the previous grapheme cluster boundary
/// Returns the byte offset of the previous grapheme, or None if at the beginning
pub fn prev_grapheme_boundary(s: &str, byte_offset: usize) -> Option<usize> {
    s.grapheme_indices(true)
        .map(|(start, _)| start)
        .take_while(|&start| start < byte_offset)
        .last()
}

/// Calculate the display column of a character at a given character index
pub fn char_to_column(line: &str, char_idx: usize) -> usize {
    if let Some(column) = printable_ascii_coordinate(line, char_idx, /*reverse*/ false) {
        return column;
    }
    line.chars().take(char_idx).map(char_display_width).sum()
}

/// Find the character index that contains the given display column
pub fn column_to_char(line: &str, target_column: usize) -> usize {
    if let Some(character) = printable_ascii_coordinate(line, target_column, /*reverse*/ true) {
        return character;
    }
    let mut current_column = 0;
    let mut char_count = 0;

    for (idx, ch) in line.chars().enumerate() {
        let char_width = char_display_width(ch);
        if current_column + char_width > target_column {
            // Target column is within this character
            return idx;
        }
        current_column += char_width;
        char_count = idx + 1;
    }

    // Return the character count if column is beyond the line
    char_count
}

/// Calculate the display column of a grapheme at a given grapheme index.
pub fn grapheme_to_column(line: &str, grapheme_idx: usize) -> usize {
    line.graphemes(true)
        .take(grapheme_idx)
        .map(display_width)
        .sum()
}

/// Calculate a grapheme's display column while expanding tabs.
pub fn grapheme_to_column_with_tabs(line: &str, grapheme_idx: usize, tab_width: usize) -> usize {
    if let Some(column) = printable_ascii_coordinate(line, grapheme_idx, /*reverse*/ false) {
        return column;
    }
    let tab_width = tab_width.max(1);
    let mut column = 0;
    for grapheme in line.graphemes(true).take(grapheme_idx) {
        column += if grapheme == "\t" {
            tab_width - (column % tab_width)
        } else {
            display_width(grapheme)
        };
    }
    column
}

/// Find the grapheme index that contains the given display column.
pub fn column_to_grapheme(line: &str, target_column: usize) -> usize {
    let mut current_column = 0;
    let mut grapheme_count = 0;

    for (idx, grapheme) in line.graphemes(true).enumerate() {
        let grapheme_width = display_width(grapheme);
        if current_column + grapheme_width > target_column {
            return idx;
        }
        current_column += grapheme_width;
        grapheme_count = idx + 1;
    }

    grapheme_count
}

/// Find the grapheme containing a display column while expanding tabs.
pub fn column_to_grapheme_with_tabs(line: &str, target_column: usize, tab_width: usize) -> usize {
    if let Some(grapheme) = printable_ascii_coordinate(line, target_column, /*reverse*/ true) {
        return grapheme;
    }
    let tab_width = tab_width.max(1);
    let mut current_column = 0;
    let mut grapheme_count = 0;

    for (idx, grapheme) in line.graphemes(true).enumerate() {
        let grapheme_width = if grapheme == "\t" {
            tab_width - (current_column % tab_width)
        } else {
            display_width(grapheme)
        };
        if current_column + grapheme_width > target_column {
            return idx;
        }
        current_column += grapheme_width;
        grapheme_count = idx + 1;
    }

    grapheme_count
}

fn printable_ascii_coordinate(line: &str, position: usize, reverse: bool) -> Option<usize> {
    let cursor = position.min(line.len());
    let end = if reverse {
        cursor.saturating_add(1).min(line.len())
    } else {
        cursor
    };
    let prefix = line.get(..end)?;
    is_printable_ascii(prefix).then_some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_backspace_preserves_graphemes_and_existing_word_boundaries() {
        for (input, expected) in [
            ("", ""),
            (" \t\n", ""),
            ("one two   ", "one "),
            ("one\nsecond", "one\n"),
            ("one\n", ""),
            ("one path/to/file.rs", "one "),
            ("one 👨‍👩‍👧e\u{301}\u{2003}", "one "),
            ("中文\u{3000}下一个", "中文\u{3000}"),
        ] {
            let mut text = input.to_string();
            delete_last_word(&mut text);
            assert_eq!(text, expected, "{input:?}");
        }
    }

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("你好"), 4); // CJK characters are 2 columns each
        assert_eq!(display_width("👋"), 2); // Emoji is 2 columns
        assert_eq!(display_width("café"), 4); // Combining character
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn test_tab_aware_columns() {
        let line = "\ta\t中";

        assert_eq!(display_width_with_tabs(line, 4), 10);
        assert_eq!(grapheme_to_column_with_tabs(line, 1, 4), 4);
        assert_eq!(grapheme_to_column_with_tabs(line, 3, 4), 8);
        assert_eq!(column_to_grapheme_with_tabs(line, 3, 4), 0);
        assert_eq!(column_to_grapheme_with_tabs(line, 4, 4), 1);
        assert_eq!(column_to_grapheme_with_tabs(line, 7, 4), 2);
        assert_eq!(column_to_grapheme_with_tabs(line, 8, 4), 3);
    }

    #[test]
    fn printable_ascii_fast_path_excludes_control_characters() {
        assert!(is_printable_ascii("plain ASCII ~"));
        assert!(!is_printable_ascii("line\r"));
        assert!(!is_printable_ascii("line\n"));
        assert!(!is_printable_ascii("left\tright"));
        assert_eq!(display_width_with_tabs("line\r", 4), 4);
        assert_eq!(display_width_with_tabs("left\tright", 4), 13);
    }

    #[test]
    fn printable_ascii_word_scanning_preserves_every_ascii_byte_and_alignment() {
        for byte in 0..=0x7f {
            for offset in 0..24 {
                let mut bytes = vec![b'A'; 25];
                bytes[offset] = byte;
                let text = std::str::from_utf8(&bytes).unwrap();
                assert_eq!(
                    is_printable_ascii(text),
                    (0x20..=0x7e).contains(&byte),
                    "byte={byte:#04x} offset={offset}"
                );
            }
        }
        for text in ["ordinary λ", "ordinary 👋", "ordinary 終", "e\u{301}"] {
            assert!(!is_printable_ascii(text), "{text:?}");
        }
    }

    #[test]
    fn printable_ascii_coordinates_preserve_unicode_controls_tabs_and_bounds() {
        for text in [
            "",
            "ordinary ASCII words",
            "\tleading tab",
            "prefix\tinside",
            "prefix\r\n",
            "prefix\0inside\u{7f}",
            "hello世界",
            "e\u{301}clair 👋 終",
            "👨‍👩‍👧 family 🇧🇷",
            "\u{301}leading combining",
        ] {
            for position in [0, 1, 2, 3, 5, 9, 20, 64, usize::MAX] {
                let expected_forward = text
                    .chars()
                    .take(position)
                    .map(char_display_width)
                    .sum::<usize>();
                let mut expected_reverse = 0;
                let mut column = 0;
                for (index, character) in text.chars().enumerate() {
                    let width = char_display_width(character);
                    if column + width > position {
                        expected_reverse = index;
                        break;
                    }
                    column += width;
                    expected_reverse = index + 1;
                }
                assert_eq!(
                    char_to_column(text, position),
                    expected_forward,
                    "scalar forward {text:?} position={position}"
                );
                assert_eq!(
                    column_to_char(text, position),
                    expected_reverse,
                    "scalar reverse {text:?} position={position}"
                );
                for tab_width in [0, 1, 2, 4, 8] {
                    let width = tab_width.max(1);
                    let mut expected_grapheme_forward = 0;
                    for grapheme in text.graphemes(true).take(position) {
                        expected_grapheme_forward += if grapheme == "\t" {
                            width - (expected_grapheme_forward % width)
                        } else {
                            display_width(grapheme)
                        };
                    }
                    let mut expected_grapheme_reverse = 0;
                    let mut display_column = 0;
                    for (index, grapheme) in text.graphemes(true).enumerate() {
                        let grapheme_width = if grapheme == "\t" {
                            width - (display_column % width)
                        } else {
                            display_width(grapheme)
                        };
                        if display_column + grapheme_width > position {
                            expected_grapheme_reverse = index;
                            break;
                        }
                        display_column += grapheme_width;
                        expected_grapheme_reverse = index + 1;
                    }
                    assert_eq!(
                        grapheme_to_column_with_tabs(text, position, tab_width),
                        expected_grapheme_forward,
                        "grapheme forward {text:?} position={position} tabs={tab_width}"
                    );
                    assert_eq!(
                        column_to_grapheme_with_tabs(text, position, tab_width),
                        expected_grapheme_reverse,
                        "grapheme reverse {text:?} position={position} tabs={tab_width}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_fit_display_width() {
        assert_eq!(fit_display_width("a👋b", 4), "a👋b");
        assert_eq!(fit_display_width("a👋b", 3), "a👋");
        assert_eq!(fit_display_width("a👋b", 5), "a👋b ");
        assert_eq!(fit_display_width("👨‍👩‍👧‍👦x", 2), "👨‍👩‍👧‍👦");
    }

    #[test]
    fn suffix_truncation_preserves_complete_graphemes() {
        assert_eq!(truncate_display_width_from_end("a👨‍👩‍👧‍👦世z", 4), "世z");
        assert_eq!(truncate_display_width_from_end("e\u{301}👋x", 3), "👋x");
        assert_eq!(truncate_display_width_from_end("visible", 0), "");
    }

    #[test]
    fn marker_truncation_preserves_configured_direction_and_width() {
        assert_eq!(
            truncate_display_width_with_marker("src/👨‍👩‍👧‍👦/function", 10, "…", TruncationSide::Left,),
            "…/function"
        );
        assert_eq!(
            truncate_display_width_with_marker("ab👋cd", 5, "…", TruncationSide::Right,),
            "ab👋…"
        );
        assert_eq!(
            truncate_display_width_with_marker("visible", 2, "...", TruncationSide::Right,),
            ".."
        );
        assert_eq!(
            truncate_display_width_with_marker("fit", 5, "…", TruncationSide::Right),
            "fit"
        );
        assert_eq!(
            truncate_display_width_with_marker("hidden", 0, "…", TruncationSide::Left),
            ""
        );
    }

    #[test]
    fn test_char_display_width() {
        assert_eq!(char_display_width('a'), 1);
        assert_eq!(char_display_width('中'), 2);
        assert_eq!(char_display_width('👋'), 2);
        assert_eq!(char_display_width('\t'), 0); // Tab has no intrinsic width
    }

    #[test]
    fn test_trim_line_ending_handles_lf_and_crlf() {
        assert_eq!(trim_line_ending("abc\n"), "abc");
        assert_eq!(trim_line_ending("abc\r\n"), "abc");
        assert_eq!(trim_line_ending("abc"), "abc");
        assert_eq!(trim_line_ending("abc  \r\n"), "abc  ");
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

        let line = "a👨‍👩‍👧‍👦e\u{0301}z";
        assert_eq!(grapheme_char_range(line, 0), Some((0, 1)));
        assert_eq!(grapheme_char_range(line, 1), Some((1, 8)));
        assert_eq!(grapheme_char_range(line, 2), Some((8, 10)));
        assert_eq!(grapheme_char_range(line, 3), Some((10, 11)));
        assert_eq!(grapheme_char_range(line, 4), None);
    }

    #[test]
    fn ascii_grapheme_count_preserves_crlf_and_unicode_cluster_boundaries() {
        for (text, expected) in [
            ("", 0),
            ("ordinary ASCII text\nnext line", 29),
            ("a\r\nb", 3),
            ("a\r\n\r\nb", 4),
            ("a\rb\nc", 5),
            ("\r\r\n\n", 3),
            ("e\u{0301}", 1),
            ("👨‍👩‍👧‍👦", 1),
            ("🇧🇷", 1),
            ("e\u{0301}\r\n👋", 3),
        ] {
            assert_eq!(grapheme_len(text), expected, "{text:?}");
        }
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

        let family = "👨‍👩‍👧‍👦";
        for byte_offset in 0..family.len() {
            assert_eq!(
                next_grapheme_boundary(family, byte_offset),
                Some(family.len())
            );
        }
        for byte_offset in 1..=family.len() {
            assert_eq!(prev_grapheme_boundary(family, byte_offset), Some(0));
        }
        assert_eq!(next_grapheme_boundary(family, family.len()), None);
        assert_eq!(prev_grapheme_boundary(family, family.len() + 1), Some(0));
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

    #[test]
    fn test_char_slicing() {
        let line = "a👋世界";
        assert_eq!(char_slice(line, 1, 3), "👋世");
        assert_eq!(char_prefix(line, 2), "a👋");
        assert_eq!(char_suffix(line, 3), "界");
        assert_eq!(truncate_chars(line, 3), "a👋世");
    }
}
