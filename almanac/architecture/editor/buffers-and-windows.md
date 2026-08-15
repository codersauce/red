---
title: "Buffers And Windows"
summary: "Buffers and windows separate editable text identity from split-tree presentation while the editor synchronizes active cursor, viewport, jumplist, and plugin-visible window state."
topics: [architecture, editor, buffers, windows, sessions, plugins]
sources:
  - id: buffer
    type: file
    path: src/buffer.rs
  - id: window
    type: file
    path: src/window.rs
  - id: buffer-manager
    type: file
    path: src/editor/buffer_manager.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: session-store
    type: file
    path: src/session.rs
  - id: movement-tests
    type: file
    path: tests/movement.rs
---

Red separates editable text from the window tree that presents it. A `Buffer` owns Ropey text, a stable process-local `BufferId`, file association, dirty state, content revision, fallback cursor state, and undo history [@buffer]. A `Window` owns a stable `WindowId`, a buffer index, terminal bounds, viewport offsets, wrapping state, cursor position, jump list, and active flag [@window]. `Editor` coordinates both sides: buffers hold content identity, windows hold per-view presentation, and rendering plus plugin snapshots synchronize the active view with the state exposed to integrations [@editor].

## Buffer Identity And Selection

`BufferId` is process-local and stable even when the buffer's position in `Editor::buffers` changes [@buffer]. That distinction matters because many legacy editor APIs still refer to buffers by current index, while undo history, marks, LSP revision tracking, and durable snapshots need an identity that is not invalidated by closing a neighboring buffer [@editor]. The buffer itself tracks a monotonic content revision that render caches, LSP delivery, and plugin payloads can compare against [@buffer].

`BufferManager` owns the open buffer vector and active buffer index. It adds buffers by making them active, can append without changing selection, removes buffers while clamping the active index, and replaces the full buffer set by resetting selection to the first buffer [@buffer-manager]. That small boundary keeps tab selection rules localized while `Editor` remains responsible for cross-cutting effects such as LSP open/close, rendering, marks, and session state.

## Split Tree And Window Identity

`WindowManager` stores windows in a recursive `Split` tree. Leaves are windows, and internal nodes are horizontal or vertical splits with ratios; layout recursively assigns terminal positions and sizes while reserving one row or column for separators [@window]. Tree-order window indexes are transient navigation handles, but each `Window` has a stable `WindowId` that survives sibling insertion and removal and is the identity exposed to plugin resources such as window bars [@window].

Window operations mutate the split tree and then recompute layout. Horizontal and vertical splits insert a new window for a buffer index and make the new leaf active; close removes a leaf and collapses its parent when only one child remains; resize adjusts a split ratio; balance resets split ratios; maximize biases ancestor ratios toward the active window; and `only_window` collapses the tree to the active leaf [@window]. Tests in `window.rs` verify stable IDs after tree reordering, non-reuse after close, snapshot round-trips, and nested split resizing [@window].

## Synchronized Cursor And View State

The editor keeps an active-window view in its own fields while windows retain per-window state. When switching buffers, `set_current_buffer` saves the outgoing buffer's viewport and cursor fallback, changes the active buffer index, restores the selected buffer's fallback position, resets horizontal viewport fields, updates the gutter-derived visual x offset, and requests diagnostics [@editor]. During rendering, `render` fixes cursor bounds, checks viewport bounds, and calls `sync_to_window` before drawing windows [@editor].

Per-window state is also serialized. `SplitSnapshot` records each leaf's saved buffer index, viewport top, horizontal offset, wrapped-line skip column, wrap mode, cursor grapheme index, cursor row, and legacy x offset [@window]. `WindowManagerSnapshot` stores the split topology and active tree-order index; restore remaps saved buffer indexes to current indexes, rebuilds fresh stable window IDs, recomputes layout for current terminal size, and activates a valid window [@window].

## Window-Local Jumplists

Jumplists are window state, not editor-global state. Each `Window` owns a `JumpList`, and editor actions resolve backward or forward traversal through the active window's list [@window] [@editor]. Split tests assert that a new split copies the source window's list and then traverses independently, which matches the window-local ownership model instead of sharing a global history [@movement-tests].

Session snapshots store per-window jumplists in split-tree order as `window_jumps` [@session-store]. Restore rebuilds each jump entry against current buffer IDs, drops entries that no longer map to an open buffer, and falls back from legacy global `jumps`/`jump_index` fields when `window_jumps` is absent [@editor]. Snapshot construction still writes legacy active-window jump fields alongside `window_jumps`, so older readers have a compatibility path while current recovery preserves each window's list [@editor] [@session-store].

## Plugin And Render Visibility

The editor publishes window state through plugin snapshots. `plugin_windows_payload` emits each window's stable ID, active flag, buffer index, file/name, buffer revision, terminal bounds, content bounds, viewport, and cursor position, including an LSP UTF-16 cursor character derived from the window's grapheme cursor [@editor]. `refresh_plugin_snapshots` can update viewport, windows, and editor-info snapshots independently, and editor event notifications refresh the snapshot groups whose cursor, viewport, layout, or buffer selection changed [@editor].

Rendering consumes the same window state. The [rendering pipeline](rendering-pipeline) draws every leaf by current tree-order index, but plugin-owned window bars use stable `WindowId` so a split or close does not silently retarget a resource to a different leaf [@editor]. The event loop page explains why those updates are serialized through the [event loop](event-loop) before plugins or detached clients observe them.
