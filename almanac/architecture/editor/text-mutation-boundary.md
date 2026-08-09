---
title: "Text Mutation Boundary"
summary: "The text mutation boundary is the editor-controlled transaction path that turns every production text change into an undoable, attributed, notified, and renderable state update."
topics: [architecture, editor, buffers, undo, lsp, plugins]
sources:
  - id: editor
    type: file
    path: src/editor.rs
  - id: buffer
    type: file
    path: src/buffer.rs
  - id: undo
    type: file
    path: src/undo.rs
  - id: editing-tests
    type: file
    path: tests/editing.rs
---

The text mutation boundary is Red's canonical path for changing buffer contents. Production edits open an undo transaction, call `Editor::replace_range`, update anchors and special marks, record the replacement in buffer-local undo history, refresh dirty state at commit, notify LSP and plugins, and render through the event loop [@editor]. `Buffer::replace_range_raw` exists beneath this boundary, but its own documentation says it does not record undo history, update marks, refresh dirty state, or notify external consumers, so new production edit handlers must not call it directly [@buffer].

## Canonical Coordinates

Text edits enter the boundary as `TextRange` values. `TextPosition::character` is a Unicode scalar index within a logical line, not a UTF-8 byte offset, grapheme index, terminal column, or LSP UTF-16 code-unit offset [@undo]. The editor converts between cursor grapheme coordinates and canonical text coordinates before calling the boundary; for example, visual selections and comment operations convert grapheme positions on a line into scalar positions before building ranges [@editor]. See [Coordinate Systems](../../concepts/editor/coordinate-systems) for the broader model behind those conversions.

The coordinate rule matters because surrounding systems use different units. The editor stores cursor `x` as a grapheme index, rendering uses terminal display columns, syntax highlighting spans use bytes, and LSP positions use UTF-16 code units [@editor]. Centralizing text mutation at `TextRange` keeps those conversions explicit and prevents a plugin, LSP response, or command handler from mixing coordinate systems inside raw buffer mutation.

## Transaction Path

`begin_transaction` and `begin_transaction_with_origin` capture the current cursor snapshot and open a buffer-local undo transaction, optionally with an attributed origin such as user, plugin, agent, or LSP [@editor]. `replace_range` first reads the old text, returns early for no-op replacements, asserts that a transaction is active, computes the absolute character range, applies `Buffer::replace_range_raw`, updates marks, writes the special `.` mark, and records the old and new text in undo history [@editor]. `commit_transaction` delegates to `UndoHistory::commit_transaction` and refreshes the buffer dirty flag from history state [@editor].

`UndoHistory` makes this a logical edit boundary rather than a raw diff list. A transaction stores ordered replacements, cursor state before and after, attribution, and revisions; commit advances the history revision once for the complete logical change and preserves sibling branches when editing after an undo [@undo]. Empty transactions are discarded, and equal old/new replacement records are ignored [@undo]. The user-facing model is covered by [Undo Tree](../../concepts/editor/undo-tree).

## Subsystems Updated By A Change

The boundary updates more than text. Anchor maintenance transforms local, global, and special marks across the replacement, then recomputes fallback line/character positions from the buffer after the edit [@editor]. Dirty state is derived from undo history revisions, not from a separate ad hoc flag once the transaction commits [@editor]. Undo and redo temporarily take ownership of the buffer's undo history, replay raw replacements, refresh dirty state, update anchors, restore cursor snapshots, notify change consumers, and render [@editor].

Notification is explicit. `notify_change` opens the current file in LSP if needed, sends `did_change` with full current buffer contents when LSP is enabled, emits a `buffer:changed` plugin notification with buffer identity, file path, revision, line count, and cursor coordinates, and records the delivered revision in `LspCoordinator` [@editor]. `flush_change_notification` skips duplicate notifications when the current buffer revision has already been delivered [@editor].

## Entry Points

User actions call the boundary through editing helpers such as comment toggling, selection transformations, joins, deletes, inserts, and replacements [@editor]. Plugin text requests also route through the same path: `BufferInsert`, `BufferDelete`, and `BufferReplace` open plugin-labeled transactions, call `replace_range`, commit, notify change consumers, and request render [@editor]. Agent proposal acceptance uses an attributed transaction before applying the proposed replacement and notifying plugins about proposal application [@editor]; [Proposal Workspace](../agent/proposal-workspace) explains the staged state that reaches this boundary only after review.

Tests exercise this boundary from higher-level workflows rather than only from helper functions. Agent editor tools stage Unicode edits in a proposal workspace without touching the live buffer or disk, reject stale revisions and workspace escapes, and preserve focused conversation UI while navigating files [@editing-tests]. Those tests confirm that agent-side edit preparation is separate from direct buffer mutation and that accepted editor changes still need to enter the editor-owned mutation path.

## Raw Mutation Escape Hatch

`Buffer` owns Ropey storage, stable process-local `BufferId`, content revision, dirty flag, fallback cursor state, and buffer-local `UndoHistory` [@buffer]. Its raw mutators increment content revision and may set dirty state, but they do not publish the semantic effects required by the editor [@buffer]. The safe rule for future changes is narrow: raw buffer replacement belongs inside `Editor::replace_range` and controlled undo/redo replay; new user, plugin, LSP, or agent mutations should be modeled as transactions through this page's boundary.
