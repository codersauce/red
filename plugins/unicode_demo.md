# Unicode Demo Plugin

This plugin demonstrates proper handling of multi-byte Unicode characters in the Red editor.

## Features

- **Character vs Display Column**: Shows the difference between character positions and display columns
- **Wide Character Support**: Properly handles CJK characters that take 2 display columns
- **Complex Emoji**: Handles emoji with Zero-Width Joiners (ZWJ) like 👨‍👩‍👧‍👦
- **Combining Characters**: Works with combining marks like in "café"

## Commands

### `unicode:demo`
Inserts a sample line with various Unicode characters and demonstrates cursor movement through them, showing how character positions differ from display columns.

### `unicode:cursor-info`
Shows detailed information about the cursor position including:
- Current character position (index)
- Current display column
- The character at cursor with its Unicode code point
- Whether it's a wide character

### `unicode:insert-samples`
Opens a picker with various Unicode samples you can insert:
- Basic emoji
- CJK text
- Complex emoji (ZWJ sequences)
- Country flags
- Mixed scripts
- Math symbols
- Text with combining marks

## Understanding Coordinate Systems

The Red editor uses three different coordinate systems:

1. **Byte Offsets**: Raw position in UTF-8 encoded string (rarely used by plugins)
2. **Character Indices**: Position by Unicode scalar values (what plugins use for text operations)
3. **Display Columns**: Visual position in terminal (what plugins use for alignment)

### Example

Consider the text: `Hello 👋 世界`

| Text | H | e | l | l | o | ␣ | 👋 | ␣ | 世 | 界 |
|------|---|---|---|---|---|---|-----|---|-----|-----|
| Char Index | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
| Display Col | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 8 | 9 | 11 |

Notice how:
- The emoji 👋 is 1 character but takes 2 display columns
- Each CJK character (世, 界) is 1 character but takes 2 display columns

## API Usage Examples

```javascript
// Insert text (uses character indices)
red.insertText(0, 0, "Hello 👋");

// Move cursor by character position
red.setCursorPosition(7, 0); // After the emoji

// Move cursor by display column
red.setCursorDisplayColumn(8, 0); // Same position, but using display column

// Get current positions
const charPos = await red.getCursorPosition(); // { x: 7, y: 0 }
const displayCol = await red.getCursorDisplayColumn(); // 8
```

## Best Practices for Plugin Developers

1. **Use character indices** for text manipulation (`insertText`, `deleteText`, `replaceText`)
2. **Use display columns** for visual alignment and column-based operations
3. **Test your plugin** with various Unicode content:
   - Emoji: 😀👋👨‍👩‍👧‍👦
   - CJK: 你好世界
   - RTL: مرحبا (Arabic)
   - Combining: café, naïve
4. **Be aware** that one grapheme cluster (what users perceive as one character) might be multiple Unicode code points

## Installation

Add to your `~/.config/red/config.toml`:

```toml
[plugins]
unicode_demo = "~/.config/red/plugins/unicode_demo.js"
```

Then reload Red or run the `:reload-plugins` command.