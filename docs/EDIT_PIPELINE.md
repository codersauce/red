# Input, action, and edit pipeline

This document defines the production path for user input and buffer mutation. Features
such as macros, dot-repeat, marks, agent proposals, attribution, LSP synchronization, and
undo must extend these seams rather than create a second executor.

## Production flow

```text
crossterm Event
  -> Editor::handle_event
  -> key resolution / pending operator / count state
  -> KeyAction
  -> Editor::handle_key_action
  -> Editor::execute / execute_with_tracking
  -> begin_transaction
  -> replace_range
  -> Buffer::replace_range_raw + UndoHistory::record_replace
  -> commit_transaction
  -> notify_change / dispatcher fallback
  -> LSP didChange + buffer:changed plugin event
  -> render and editor state events
```

`EditorTestExt::test_execute_action` invokes the production action dispatcher.
`Editor::test_execute_event` starts at the production input boundary and must be used for
tests whose behavior depends on counts, pending keys, operators, or mode transitions.

## Three different replay records

The roadmap requires three related but non-interchangeable representations:

- **Input events** are normalized key events replayed through `handle_event`. Macros use
  these because counts, pending keys, mappings, and mode transitions must run normally.
- **Semantic changes** describe one completed Vim change, including count, register,
  operator/motion or text object, inserted text, and mode transitions. Dot-repeat uses
  these because repeating a change is not the same as replaying arbitrary navigation.
- **Recorded edits** are character-coordinate replacements inside `EditTransaction`.
  Undo, attribution, anchors, persistence, and agent proposal application use these.

An input event may produce no action. One action may produce several recorded edits. One
semantic change may span several actions and one transaction, particularly insert mode.

## Mutation invariants

1. Production editor content changes call `Editor::replace_range`.
2. `replace_range` requires an active undo transaction. A missed transaction is a
   programming error rather than an untracked mutation.
3. The raw `Buffer::replace_range_raw` method is reserved for the recorded-edit seam and
   undo/redo replay. Raw replacement must not be called by a new action handler.
4. A successful replacement advances `Buffer::revision`, records the old and new text,
   and eventually emits the new revision to LSP and plugins.
5. Eager notifications remain valid where UI ordering needs them. At the end of every
   production action, the dispatcher compares the buffer revision with the last
   successfully notified revision and flushes any missing notification.
6. Visual-block replay may defer notifications while nested rows are applied, but the
   outer replay emits one notification for the final revision.
7. Undo and redo use the recorded transaction, restore a cursor snapshot, refresh dirty
   state, and notify external consumers.

## Coordinates

Recorded `TextPosition` values use line plus Unicode scalar-value index. The visible
editor cursor uses a grapheme index, terminal layout uses display columns, and LSP uses
UTF-16 code units. See [unicode-handling.md](unicode-handling.md) for the conversion
contract.

## Adding an editing action

1. Resolve input through the existing key pipeline; do not call the action handler from a
   custom event path.
2. Begin or join the transaction that represents the user's logical undo step.
3. Convert cursor/selection coordinates to `TextPosition` explicitly.
4. Apply every content replacement through `Editor::replace_range`.
5. Commit at the logical boundary. Insert sessions may deliberately keep a transaction
   open until leaving insert mode.
6. Render as needed. Notification is safe to request eagerly; the dispatcher fallback
   guarantees the final revision is not omitted.
7. Test the production action path. Also test the production event path when mappings,
   counts, modes, or pending-key state matter.

## Required invariant coverage

- Mutating without a transaction fails immediately.
- A committed edit is undoable and redoable as one logical operation.
- The externally notified revision equals the buffer's latest revision and duplicate
  flushes are idempotent.
- Cursor positions remain valid after action, undo, and redo.
- Event-driven operator/count tests pass through normal key resolution rather than
  constructing their resulting edit action directly.
