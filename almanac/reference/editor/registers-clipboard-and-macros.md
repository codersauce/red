---
title: "Registers, Clipboard, And Macros"
summary: "Registers hold charwise, linewise, or blockwise text, the default register can sync with the system clipboard, and macros replay normalized key notation with deterministic limits."
topics: [editor, vim, clipboard]
sources:
  - id: clipboard
    type: file
    path: src/clipboard.rs
  - id: editor-core
    type: file
    path: src/editor.rs
  - id: editing-tests
    type: file
    path: tests/editing.rs
---

Red's register and macro surface follows Vim-style editing while staying inside the editor transaction boundary. Text registers store `Content` as charwise, linewise, or blockwise text; the default register is the double quote register and can synchronize with a replaceable clipboard provider [@editor-core]. Macros store editable key notation in registers, replay through queued events, and stop at fixed depth and event limits so recursive playback is deterministic [@editor-core].

## Text Register Kinds

`Content` has three constructors: `charwise`, `linewise`, and `blockwise` [@editor-core]. Deletes, yanks, visual selections, and paste plans preserve that kind, which changes how insertion works. Charwise paste inserts into a line around a grapheme-derived scalar position; linewise paste inserts whole lines before or after a target line; blockwise paste inserts each source line at a grapheme column, extending target lines with spaces if needed [@editor-core].

Visual paste plans also preserve register kind. Charwise sources replace the selected scalar range, linewise sources splice whole lines, and blockwise sources remove selected grapheme columns before inserting block lines [@editor-core]. Tests cover visual paste for charwise, linewise, and blockwise sources, and they verify that replacing a selected Unicode grapheme captures the whole grapheme into the default register [@editing-tests].

## Default Register And Clipboard

The default register is `'"'` [@editor-core]. `set_register` writes to the system clipboard only when the target register is the default register, and `write_system_clipboard` only runs when clipboard support is enabled and `sync_on_yank` is true [@editor-core]. Before normal or visual paste reads the default register, `refresh_default_register_from_system_clipboard` optionally imports system clipboard text when clipboard support is enabled and `sync_on_paste` is true [@editor-core].

The clipboard boundary is a trait. `ClipboardProvider` supports fallible `get_text` and `set_text`, `NativeClipboardProvider` delegates to `arboard`, `DisabledClipboardProvider` treats clipboard operations as unavailable no-ops, and `MemoryClipboardProvider` gives tests and embedded callers deterministic shared text [@clipboard]. Tests assert that yanking and deleting through the default register write the system clipboard, paste can read external clipboard text, and uppercase visual paste preserves the clipboard while replacing the selection [@editing-tests].

## Dot Repeat

Dot repeat uses `last_repeatable_change`, not the macro register store. The editor records an input recipe for the last completed content-changing command and finalizes it only when the change succeeds [@editor-core]. Tests assert that dot repeats direct changes at the current cursor, treats an insert session as one semantic change, preserves literal periods inserted by the user, recomputes operator motions at the new cursor, applies a count before dot as repeated playback, and does not replace the last repeatable change after a failed change [@editing-tests].

Dot repeat also respects the edit transaction boundary described by [Undo Tree](../../concepts/editor/undo-tree). Tests cover text objects, replace, indentation, open-line changes, linewise paste, visual block insert, counted replace, and join operations as repeatable changes that still undo as coherent transactions [@editing-tests].

## Macro Registers

Macro registers are limited to ASCII alphanumeric names, normalized to lowercase [@editor-core]. A macro recording stores the target register and a list of key events; register contents are stored as editable key notation, and `SetMacroRegister` can install that notation directly [@editor-core]. Tests verify that macros record normal-mode, insert-mode, and motion events, can record literal `q` input before normal-mode recording stops, can be inspected through register printing, and can be edited through `SetMacroRegister` [@editing-tests].

Playback has two deterministic limits. `MACRO_MAX_REPLAY_DEPTH` is 20 and `MACRO_MAX_REPLAY_EVENTS` is 10,000 [@editor-core]. Recursive playback reports a macro recursion limit instead of mutating the buffer indefinitely, and counted macro playback runs the target register repeatedly [@editing-tests].

## Boundaries

Registers and macros describe user-level editing state; they do not replace the canonical mutation path. Paste, delete, change, dot-repeat, and macro replay all reach content changes through editor actions that open transactions, convert visible grapheme positions into scalar ranges as needed, and call the editor's replacement boundary [@editor-core]. That is why this reference depends on both [Editor Coordinate Systems](../../concepts/editor/coordinate-systems) and [Undo Tree](../../concepts/editor/undo-tree).
