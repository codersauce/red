# Unicode coordinates in Red

Red must distinguish four coordinate systems. They are not interchangeable, even when
they have the same value for ASCII text.

| Coordinate | Unit | Current use |
|------------|------|-------------|
| Byte offset | UTF-8 byte | Rust string slicing and parser spans |
| Character index | Unicode scalar value (`char`) | Ropey and `TextPosition::character` |
| Grapheme index | User-perceived character | Editor cursor movement and selections |
| Display column | Terminal cell | Rendering, wrapping, and horizontal alignment |

Language servers add a fifth boundary: LSP positions are normally UTF-16 code units.
Conversions at that boundary belong in the LSP layer, not in the buffer or renderer.

## Core ownership

- `src/unicode_utils.rs` owns conversions between UTF-8 bytes, scalar-value indices,
  grapheme indices, and terminal columns. It also owns tab-aware display-width helpers.
- `src/buffer.rs` stores text in Ropey and receives edit ranges as character indices.
- `src/undo.rs` records `TextPosition::character` in character-index coordinates.
- `src/editor.rs` stores the visible cursor `cx` as a grapheme index and converts it to a
  character index before constructing an edit range.
- `src/editor/display_layout.rs` converts grapheme positions to terminal columns for
  wrapping and screen-line movement.
- `src/editor/rendering.rs` renders grapheme clusters and expands tabs using the active
  tab width.

Use the helper that names both sides of a conversion. Do not slice a UTF-8 string using a
character, grapheme, or display-column value.

## Important helpers

The maintained conversion surface is in `src/unicode_utils.rs`:

```rust
display_width(text)
display_width_with_tabs(text, tab_width)
char_to_byte(text, character)
byte_to_char(text, byte_offset)
grapheme_to_char(text, grapheme)
char_to_grapheme(text, character)
grapheme_to_column_with_tabs(text, grapheme, tab_width)
column_to_grapheme_with_tabs(text, column, tab_width)
```

Prefer grapheme-aware helpers for user-visible cursor and selection behavior. Prefer
character indices for Ropey edits and persisted `TextPosition` values. Prefer display
columns only for terminal layout.

## Husk plugin boundary

Husk plugins use `red::execute` for fire-and-forget operations and `red::request` with a
callback for values. The current host boundary exposes these Unicode-related operations:

- `GetCursorPosition` and `SetCursorPosition` use the editor cursor's grapheme `x`.
- `BufferInsert`, `BufferDelete`, and `BufferReplace` use character-index `x` values.
- `GetCursorDisplayColumn` returns a terminal column.
- `SetCursorDisplayColumn` accepts a terminal column and resolves it to a grapheme.
- `GetTextDisplayWidth` returns terminal-cell width.
- `CharIndexToDisplayColumn` and `DisplayColumnToCharIndex` convert against a buffer line.

This asymmetry reflects the current implementation. Plugin code must not assume that a
cursor `x` can be passed directly to a buffer-edit operation for text containing a
multi-scalar grapheme such as a combining sequence or ZWJ emoji. Unifying and typing this
boundary is part of the canonical edit-boundary and typed-host-API work in
`PROJECT_PLAN.md`.

Example request:

```rust
fn activate() {
    red::add_command("MeasureCursor", measure_cursor);
}

fn measure_cursor() {
    red::request("GetCursorDisplayColumn", cursor_column_loaded);
}

fn cursor_column_loaded(result: Json, request_id: i32) {
    red::log("cursor display column", result.value.column);
}
```

The callback's second argument is the opaque request ID. See `PLUGIN_SYSTEM.md` and the
bundled `.hk` plugins for the current lifecycle and callback conventions.

## Testing

Relevant coverage currently lives in:

- `src/unicode_utils.rs` for conversion helpers;
- `src/editor/display_layout.rs` and `src/editor/rendering.rs` for layout and rendering;
- `tests/unicode.rs` for cursor, editing, visual-mode, combining-mark, CJK, and ZWJ cases;
- `tests/simple_unicode.rs` for focused end-to-end cases;
- plugin runtime tests in `src/plugin/runtime.rs` for host requests and callbacks.

When changing a coordinate boundary, include at least ASCII, CJK, combining-mark, emoji,
ZWJ, tab, empty-line, and end-of-line cases. Assertions should state which coordinate
system they use.

## Known limitations

- Bidirectional and vertical text layout are not implemented.
- Terminal and font support can affect the appearance of emoji and combining sequences.
- The plugin host API does not yet encode coordinate systems in distinct types.
- General multi-file LSP `WorkspaceEdit` handling still needs an explicit UTF-16-correct
  conversion and rollback policy.
