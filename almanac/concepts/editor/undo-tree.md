---
title: "Undo Tree"
summary: "Red keeps a buffer-local branching undo history whose revisions drive dirty state and whose replay returns concrete edits for editor-side maintenance."
topics: [editor, undo, persistence]
sources:
  - id: undo
    type: file
    path: src/undo.rs
  - id: buffer
    type: file
    path: src/buffer.rs
  - id: editor-core
    type: file
    path: src/editor.rs
  - id: editing-tests
    type: file
    path: tests/editing.rs
---

Red's undo tree is a buffer-local branching history of committed edit transactions. Each transaction records the origin of the change, a human label, ordered replacement edits, cursor snapshots before and after the change, and revisions before and after the transaction [@undo]. Editing after undo creates a sibling branch instead of deleting alternate history, while dirty state is computed by comparing the selected history revision with the saved revision [@undo].

## Transactions

`UndoHistory` collects replacements inside one active transaction. `begin_transaction_with_origin` records the label, cursor snapshot, current revision, and origin, and nested begins keep the first active transaction rather than replacing its attribution [@undo]. `record_replace` ignores no-op replacements and records half-open `TextRange` values plus absolute Ropey character positions and old/new text for effective replacements [@undo].

The editor enforces the transaction boundary. `Editor::replace_range` asserts that a transaction is active, mutates the buffer through `replace_range_raw`, updates marks and the special `.` mark, then records the replacement in the buffer's undo history [@editor-core]. A regression test panics when `replace_range` is called without an active transaction, which makes the mutation boundary an enforced runtime invariant rather than a style preference [@editor-core].

## Branches And Replay

Committed transactions become nodes under the current node or under the virtual root. The node's parent keeps the branch relationship, and `branch_selection` records which sibling child a future redo should select [@undo]. `select_next_branch` and `select_previous_branch` adjust that sibling choice without applying edits [@undo].

Undo replays the current transaction backward, replacing each recorded new image with its old image in reverse edit order, then moves the current pointer to the parent revision and returns the transaction's before-cursor snapshot plus concrete applied edit ranges [@undo]. Redo applies the selected child transaction in forward order, advances the current pointer, and returns the after-cursor snapshot plus applied edit ranges [@undo]. The editor receives those applied edits, updates anchors, restores the cursor snapshot, refreshes dirty state, notifies external consumers, and renders [@editor-core].

## Revisions And Dirty State

Dirty state is revision-based. `UndoHistory::mark_saved` stores the current revision as the saved revision, and `is_dirty` compares the current revision with that saved revision [@undo]. `Buffer::mark_saved` delegates to the undo history and then refreshes the public dirty flag [@buffer]. Editor tests assert that undoing back to the clean revision clears dirty state, redo makes the buffer dirty again, saving moves the clean checkpoint forward, saving during insert stays clean after leaving insert mode, and undoing past the saved revision is dirty because the selected content no longer matches the saved revision [@editing-tests].

This model lets saves preserve history. A saved buffer can still undo to an older state, but that older selected revision is dirty because it differs from the saved checkpoint [@editing-tests].

## Coordinates And Limits

Undo transaction ranges use the scalar-coordinate boundary described in [Editor Coordinate Systems](coordinate-systems). `TextPosition::character` is explicitly a Unicode scalar index, while `CursorSnapshot::x` is a grapheme index used for editor cursor restoration [@undo]. This lets undo replay mutate the rope in canonical buffer coordinates while the editor restores the visible cursor in user-facing coordinates.

The history has a default maximum of 10,000 nodes. If the node count exceeds the cap, `prune_excess_nodes` retains the newest part of the active branch, discards alternate branches, compacts indexes, and preserves an undoable active path [@undo]. Tests cover capacity enforcement, serialization of compacted history, and undo/redo after compacting [@undo].

The undo tree also supports selective revert. `prepare_revert` can build inverse replacements for a transaction on the current branch, but it refuses to proceed if later edits overlap the transaction's post-image or if the current buffer text no longer matches that post-image [@undo]. That makes revert a checked operation rather than blind replay.

## Relation To Registers And Repeats

Undo records text mutations; registers, macros, and dot-repeat record command-level input or copied content. The [Registers, Clipboard, And Macros](../../reference/editor/registers-clipboard-and-macros) reference describes that separate surface. The systems meet at the transaction boundary: repeatable commands and macro playback still create normal undo transactions, and tests assert that counted operators, dot-repeat, macro replay, joins, visual changes, and block inserts remain undoable as coherent changes [@editing-tests].
