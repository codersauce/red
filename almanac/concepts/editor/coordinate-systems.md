---
title: "Editor Coordinate Systems"
summary: "Red keeps separate byte, scalar, grapheme, terminal-column, and UTF-16 coordinate systems at explicit editor boundaries."
topics: [editor, unicode, lsp]
sources:
  - id: editor-core
    type: file
    path: src/editor.rs
  - id: unicode-utils
    type: file
    path: src/unicode_utils.rs
  - id: lsp-edit
    type: file
    path: src/lsp/edit.rs
  - id: unicode-tests
    type: file
    path: tests/unicode.rs
---

Red's editor coordinate systems are deliberately separate. Editor cursor `x` values are grapheme indices, buffer and undo edit ranges are Unicode scalar indices, rendering and layout use terminal display columns, syntax spans use UTF-8 byte offsets, and LSP edit positions cross through UTF-16 code units before becoming buffer scalar ranges [@editor-core]. This split matters because ASCII text can make all of those numbers look interchangeable, while emoji, combining marks, tabs, CJK text, and CRLF line endings expose the boundaries [@unicode-utils].

## Coordinate Boundaries

The central editor module states the runtime rule: crossing from one coordinate system to another requires a helper that names the conversion [@editor-core]. The most common boundary is from visible cursor coordinates into buffer edit coordinates. The cursor stores `cx` as a grapheme index, while `TextPosition::character` and `TextRange` use Unicode scalar indices, so edit paths call helpers such as `grapheme_to_char_on_line`, `char_to_grapheme_on_line`, and `move_to_text_position` when a visible position becomes a buffer range or vice versa [@editor-core].

UTF-8 bytes appear where raw strings and external spans require them. The syntax highlighter tracks byte spans, and plugin location handling accepts byte-encoded columns; these are distinct from editor cursor positions and from Ropey character indices [@editor-core]. The utility module provides byte-to-character and grapheme-to-byte helpers so code can slice strings without splitting a multibyte character or a grapheme cluster [@unicode-utils].

LSP has its own boundary. `lsp_character_for_cursor` converts a grapheme cursor into an LSP character value by taking the graphemes before the cursor, expanding them into Rust `char`s, and summing each character's UTF-16 length [@editor-core]. Incoming LSP text edits use `text_edit_char_range`, which maps UTF-16 line positions to byte offsets, rejects positions that split a UTF-16 character, then returns absolute Unicode scalar ranges for buffer mutation [@lsp-edit].

## Graphemes And Scalars

Red treats user-visible horizontal cursor movement as grapheme movement. The editor's Unicode tests assert that a single emoji advances the cursor by one, CJK characters advance as individual cursor positions, and mixed ASCII, emoji, and CJK text produce grapheme positions rather than byte positions [@unicode-tests]. That is why visible selections and block operations often start with grapheme positions and convert to scalar ranges only at the edit boundary [@editor-core].

Scalars are the canonical mutation unit below the editor surface. `TextPosition::character` in the undo model is a zero-based Unicode scalar index, and `Buffer::replace_range_raw` removes and inserts through Ropey character positions derived from `TextRange` values [@editor-core]. This makes mutations independent of terminal cell width while still allowing cursor restoration through `CursorSnapshot`, whose `x` field remains a grapheme index [@editor-core].

## Terminal Columns

Terminal display columns model what the user sees, not where text should be edited. The Unicode helpers calculate display width with `unicode_width`, expand tabs from the current column, and provide conversions between graphemes and display columns with explicit tab width parameters [@unicode-utils]. Vertical cursor goals use those display columns so movement across tabs, wide glyphs, and short lines can preserve an intended screen column [@editor-core].

Display columns are also the input to [Display Layout](display-layout). Layout maps logical lines into screen rows by display width, while cursor-to-window conversion first turns a grapheme cursor into a display column and then asks the active `DisplayLayout` for the screen segment that contains it [@editor-core].

## Why The Split Matters

Most editor code can stay correct by knowing which boundary it is crossing. Rendering code should not feed terminal columns into `TextPosition`; LSP code should not treat protocol `character` values as Unicode scalar counts; and string slicing should not assume a grapheme index is a byte offset. The helper names are the guardrail: `grapheme_to_column_with_tabs`, `column_to_grapheme_with_tabs`, `grapheme_to_char`, `char_to_grapheme`, `grapheme_to_byte`, and `text_edit_char_range` make the conversion explicit [@unicode-utils].

The same boundary supports higher-level systems. [Undo Tree](undo-tree) records canonical scalar edit ranges while restoring grapheme cursor snapshots, and the [text mutation boundary](../../architecture/editor/text-mutation-boundary) explains how those edits are committed through the editor before marks, LSP, plugins, and dirty state are updated.
