---
title: "Rendering Pipeline"
summary: "The rendering pipeline turns editor, window, plugin, diagnostic, and dialog state into a width-aware terminal-cell frame and flushes only the changed cells or rows."
topics: [architecture, editor, rendering, windows, plugins]
sources:
  - id: rendering
    type: file
    path: src/editor/rendering.rs
  - id: render-buffer
    type: file
    path: src/editor/render_buffer.rs
  - id: picker
    type: file
    path: src/ui/picker.rs
  - id: display-layout
    type: file
    path: src/editor/display_layout.rs
  - id: splash
    type: file
    path: src/splash.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: rio-hidden-cursor-trail
    type: web
    url: https://github.com/raphamorim/rio/issues/1511
  - id: rio-effects-config
    type: web
    url: https://rioterm.com/pt-br/docs/config
  - id: rio-cursor-session
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/06/rollout-2026-08-06T13-38-01-019fd7f0-5a30-7af2-b391-9e17d6eee593.jsonl
  - id: ctrl-t-tab-session
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/31/rollout-2026-08-31T18-03-49-01a05a10-8f88-7f70-9850-f35913a6ac1b.jsonl
---

The rendering pipeline composes Red's logical editor state into an in-memory terminal-cell frame before writing to the terminal. `rendering.rs` draws windows, gutters, text, separators, panels, workspace views, dialogs, plugin render commands, overlays, diagnostics, search highlights, matching brackets, cursor styling, and frame diffs [@rendering]. `RenderBuffer` is the frame model: it stores grapheme text, width, continuation cells, and style for each terminal cell so wide graphemes and styled spans can be diffed without treating a row as a byte string [@render-buffer].

## Layout Before Paint

Window text rendering starts with [display layout](../../concepts/editor/display-layout). `DisplayLayout` maps buffer lines to visible screen-row segments using grapheme boundaries and terminal-cell widths, and it owns wrapping, horizontal offsets, continuation indentation, and cursor hit-testing for a specific viewport configuration [@display-layout]. Its cache key must include layout-affecting inputs such as buffer revision, viewport width, wrap mode, and indentation because reusing a layout across those changes would yield wrong cursor or hit-test positions [@display-layout].

The renderer asks for a layout per window, fills each visible content row, then walks graphemes from the segment's byte range while tracking display columns, tab expansion, visual offsets, and syntax highlight spans [@rendering]. This is the point where the editor's coordinate systems converge: buffer text is sliced by byte offsets from the layout, graphemes become terminal cells, tabs expand to spaces using indentation width, and styles come from a forward-only syntax style cursor [@rendering].

RenderBuffer cells must contain terminal-printable text, not raw control characters. `RenderBuffer::set_text` uses the printable-ASCII fast path only after `is_printable_ascii` rejects control bytes such as tabs, while `set_printable_ascii` skips that scan and writes bytes directly into cells [@render-buffer]. The diff flush later concatenates each changed cell's `text` and sends it to the terminal with `Print`, so any raw tab that enters a cell moves the terminal cursor independently of Red's computed cell positions [@rendering]. The window renderer expands buffer tabs before paint; picker preview paths are separate and must preserve that invariant when clipping source lines and overlaying syntax or match spans [@rendering] [@picker]. A 2026-08-31 `Ctrl-t` Go-symbol reproduction at 80x21 confirmed the failure mode: tab-indented preview lines emitted literal `0x09` bytes and corrupted the dialog, while the same fixture with spaces emitted no tab bytes and rendered cleanly [@ctrl-t-tab-session].

## Full Frame Construction

`Editor::render` is the full-frame entrypoint. It updates gutter width, applies panel layout, fixes cursor position, checks bounds, synchronizes editor fields back to the active window, renders each window, draws window separators, renders the startup splash if eligible, renders panels and global chrome, overlays modal workspaces and dialogs, drains plugin render commands, updates overlays, paints the cursor cell, diffs the frame, flushes changes, and advances `render_generation` [@rendering]. The [buffers and windows](buffers-and-windows) page explains the window state consumed by these steps.

The startup splash is render-only. `splash.rs` defines the splash model and states that it is drawn during the normal render pass and never touches buffer contents [@splash]. `render_splash` only draws it over a pristine single unnamed blank buffer when splash configuration and startup-file conditions allow it, and latches it off after the pristine state first fails [@rendering].

## Overlays, Dialogs, And Plugin Paint

Plugin paint enters the pipeline in two forms. Low-level `RenderCommand::BufferText` commands are drained after editor chrome and before overlay positioning, and each command writes styled text into the `RenderBuffer` [@rendering]. Higher-level plugin resources such as overlays, panels, text panels, workspaces, decorations, gutter signs, and window bars are materialized as editor state by plugin host requests, then rendered through normal pipeline stages [@rendering].

Decorations are placed per visible line segment. The renderer supports column, end-of-line, and right-aligned anchors, respects wrapped continuation rows, and can restrict decoration paint to blank or leading-whitespace cells so indentation guides do not overwrite content [@rendering]. Dialogs and keymap hints draw after workspace content and before plugin render commands or overlays, while overlay positions are updated from terminal size and cursor position before all dirty overlays render [@rendering].

## Diffing And Motion Fast Paths

The pipeline avoids repainting the whole terminal when it can. `render_buffer_changes` compares the new frame against `previous_render_buffer`, forces a full diff when dimensions or theme-derived colors require it, and commits only changed cells back to the previous frame after flushing [@rendering]. `RenderBuffer::diff` first detects changed rows and then emits changed cells, while `diff_row_snapshots` compares only explicitly snapshotted rows for cursor-motion fast paths [@render-buffer].

`render_motion_frame` redraws the active window and chrome when a full frame is unnecessary, and `render_cursor_motion_delta` snapshots the previous cursor row, new cursor row, matching-bracket rows, status row, and command row before re-rendering only those rows [@rendering]. That fast path is allowed only when terminal output is enabled, a synthetic block cursor is in use, no dialog, focused panel, visual selection, active search, visible overlay, or active diagnostics can affect unrelated rows [@rendering].

## Terminal Cursor Compatibility

Red's synthetic block cursor is a painted cell, not the native terminal cursor. The full render path updates terminal-cursor surface state, applies `render_cursor_cell`, diffs the frame, and then flushes changed cells [@rendering]. `render_diff` hides the native terminal cursor before writing changes, but it still moves that hidden cursor to each changed run's terminal position so text can be printed in place [@rendering]. After the flush, `draw_cursor_with_goal_refresh` keeps the native cursor hidden whenever `uses_synthetic_block_cursor` is true [@rendering].

That sequence can expose terminal-emulator cursor animation bugs. In Rio, issue #1511 reports that the cursor trail animates while the native cursor is hidden in TUI applications, and the trail should be disabled when the cursor is not visible [@rio-hidden-cursor-trail]. If Red shows phantom cursor bars or trails under Rio while the Red cursor itself stays logically correct, first disable Rio's trail effect with `[effects] trail-cursor = false`; Rio documents `trail-cursor` under the effects configuration table [@rio-effects-config]. The August 2026 cursor-debugging session matched that symptom to Red's hidden-native-cursor diff flushing rather than to a Red cursor-positioning failure [@rio-cursor-session].

## Detached Frames

Detached editor cores use the same render pipeline but turn off terminal escape output. `DetachedEditorCore::new` disables terminal output, initializes plugin runtime state, renders into a `RenderBuffer`, serializes text rows and styled spans, and records cursor position [@editor]. Later input, resize, focus, and idle ticks call the editor event loop and finish by serializing changed rows into render deltas, so detached clients paint frame results while the core process keeps owning editor state [@editor]. The [event loop](event-loop) page covers the background service path that keeps those detached frames current.
