# Unicode Handling in Red Editor

This guide explains how the Red editor handles Unicode and multi-byte characters, including implementation details for core developers and plugin authors.

## Overview

Red editor provides comprehensive Unicode support including:
- Proper rendering of wide characters (CJK, emoji)
- Grapheme cluster-aware cursor movement
- Correct display column calculations
- Plugin APIs for Unicode-aware text manipulation

## Three Coordinate Systems

Red uses three different coordinate systems for text positioning:

### 1. Byte Offsets
- Position in the UTF-8 encoded byte stream
- Used internally by the rope data structure
- Rarely exposed to plugins or users
- Example: "你好" is 6 bytes (3 bytes per character)

### 2. Character Indices
- Position by Unicode scalar values (Rust's `char`)
- Used by buffer operations and plugin APIs
- What `x` represents in cursor positions
- Example: "你好" is 2 characters

### 3. Display Columns
- Visual position in the terminal
- Accounts for character display width
- Used for rendering and visual alignment
- Example: "你好" takes 4 display columns (2 per character)

## Core Implementation

### Unicode Utilities Module (`src/unicode_utils.rs`)

The core Unicode handling is implemented in the `unicode_utils` module:

```rust
// Calculate display width of a string
pub fn display_width(s: &str) -> usize

// Convert between coordinate systems
pub fn char_to_column(line: &str, char_pos: usize) -> usize
pub fn column_to_char(line: &str, column: usize) -> usize
pub fn byte_to_char(s: &str, byte_pos: usize) -> usize
pub fn char_to_byte(s: &str, char_pos: usize) -> usize

// Grapheme cluster operations
pub fn grapheme_count(s: &str) -> usize
pub fn next_grapheme_boundary(s: &str, char_pos: usize) -> Option<usize>
pub fn prev_grapheme_boundary(s: &str, char_pos: usize) -> Option<usize>
```

### Cursor Movement

Cursor movement respects grapheme boundaries:

```rust
// In editor.rs
Action::MoveLeft => {
    if self.cx > 0 {
        let line = self.current_line_contents();
        if let Some(prev) = prev_grapheme_boundary(&line, self.cx) {
            self.cx = prev;
        }
    }
}
```

This ensures that multi-codepoint sequences (like 👨‍👩‍👧‍👦) move as single units.

### Rendering

The rendering system accounts for character display width:

```rust
// In editor/rendering.rs
for grapheme in line.graphemes(true) {
    let width = display_width(grapheme);
    if width == 0 {
        // Zero-width character (e.g., combining marks)
        continue;
    }
    // Render with proper spacing for wide characters
}
```

## Plugin API

### Text Manipulation

Plugin text operations use character indices:

```javascript
// Insert at character position 5
red.insertText(5, 0, "Hello");

// Delete 3 characters starting at position 10
red.deleteText(10, 0, 3);

// Replace 2 characters with new text
red.replaceText(8, 0, 2, "世界");
```

### Cursor Positioning

Plugins can work with both character positions and display columns:

```javascript
// Character-based positioning
red.setCursorPosition(7, 0);
const pos = await red.getCursorPosition(); // {x: 7, y: 0}

// Display column-based positioning
red.setCursorDisplayColumn(10, 0);
const col = await red.getCursorDisplayColumn(); // 10
```

### Unicode Helper Methods

New helper methods for Unicode handling:

```javascript
// Get display width of text
const width = await red.getTextDisplayWidth("你好"); // Returns 4

// Convert between character index and display column
const displayCol = await red.charIndexToDisplayColumn(5, 0);
const charIndex = await red.displayColumnToCharIndex(10, 0);
```

## Common Scenarios

### Working with Mixed-Width Text

When aligning text in columns, use display width calculations:

```javascript
async function alignText(red, text, targetWidth) {
    const width = await red.getTextDisplayWidth(text);
    const padding = targetWidth - width;
    return text + ' '.repeat(Math.max(0, padding));
}
```

### Finding Character Boundaries

When moving through text, respect grapheme boundaries:

```javascript
// Move cursor right by one visual character
const pos = await red.getCursorPosition();
red.execute('MoveRight'); // Handles grapheme boundaries
```

### Handling User Input

When processing user input with Unicode:

```javascript
red.on('buffer:changed', async (event) => {
    const line = await red.getBufferText(event.cursor.y, event.cursor.y + 1);
    const displayWidth = await red.getTextDisplayWidth(line);
    red.log(`Line ${event.cursor.y} is ${displayWidth} columns wide`);
});
```

## Best Practices

### For Core Development

1. **Always use grapheme boundaries** for cursor movement
2. **Test with complex Unicode** including:
   - ZWJ sequences: 👨‍👩‍👧‍👦
   - Combining marks: é (e + ́)
   - Wide characters: 你好
   - RTL text: مرحبا

3. **Preserve text integrity** - never split grapheme clusters
4. **Use unicode_utils functions** instead of implementing your own

### For Plugin Development

1. **Understand the coordinate systems**:
   - Use character indices for text manipulation
   - Use display columns for visual alignment

2. **Test with Unicode content**:
   ```javascript
   const testCases = [
       "Hello",      // ASCII
       "你好",       // CJK
       "👋🌍",      // Emoji
       "café",       // Combining chars
       "👨‍👩‍👧‍👦"    // ZWJ sequence
   ];
   ```

3. **Handle edge cases**:
   - Empty strings
   - Lines with only wide characters
   - Mixed-width content

4. **Use the helper methods**:
   ```javascript
   // Don't manually calculate display width
   const width = await red.getTextDisplayWidth(text);
   
   // Don't assume 1 char = 1 column
   const col = await red.charIndexToDisplayColumn(x, y);
   ```

## Testing

### Unit Tests

Test files for Unicode handling:
- `tests/unicode.rs` - Basic Unicode operations
- `tests/visual_unicode.rs` - Visual mode with Unicode
- `tests/plugin_unicode.rs` - Plugin API with Unicode

### Manual Testing

1. Create a file with diverse Unicode content
2. Test cursor movement through all characters
3. Test selection across grapheme boundaries
4. Test plugin operations on Unicode text

## Troubleshooting

### Common Issues

1. **Cursor jumps unexpectedly**
   - Check if you're mixing character indices and display columns
   - Ensure grapheme boundaries are respected

2. **Text alignment breaks**
   - Use `getTextDisplayWidth()` instead of string length
   - Account for zero-width characters

3. **Plugin operations fail on Unicode**
   - Verify you're using character indices, not byte offsets
   - Test with the `unicode:test-helpers` command

### Debug Commands

Use the Unicode demo plugin to debug issues:
```
:unicode:cursor-info     # Show current position details
:unicode:test-helpers    # Test coordinate conversions
```

## Performance Considerations

- Display width calculation is O(n) - cache results when possible
- Grapheme segmentation allocates memory - reuse iterators
- Coordinate conversions scan the string - minimize conversions

## Future Improvements

Potential enhancements for Unicode support:
- Bidirectional text (RTL) support
- Vertical text layout for CJK
- Unicode normalization options
- Configurable emoji presentation
- Performance optimizations for long lines

## References

- [Unicode Standard](https://unicode.org/)
- [Unicode Text Segmentation](https://unicode.org/reports/tr29/)
- [East Asian Width](https://unicode.org/reports/tr11/)
- [Rust unicode-width crate](https://docs.rs/unicode-width/)
- [Rust unicode-segmentation crate](https://docs.rs/unicode-segmentation/)