---
title: "LSP Completion"
summary: "Red completion combines UTF-16-aware requests, stale-response guards, filtered terminal UI, atomic edit application, snippet cleanup, and optional follow-up LSP commands."
topics: [architecture, lsp, completion]
sources:
  - id: completion-ui
    type: file
    path: src/ui/completion.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: completion-tests
    type: file
    path: tests/completion.rs
  - id: completion-fixture
    type: file
    path: src/fixtures/lsp-completion-response.json
---

LSP completion in Red spans the transport, editor, and terminal UI. The editor issues completion only from insert mode, records a buffer id, revision, and URI snapshot for the request, sends UTF-16 line and character positions to the language server, and later accepts a response only if the buffer identity and revision are still current [@editor]. The completion UI owns display state and filtering, while accepting an item returns to the editor for validated buffer mutation and optional command execution [@completion-ui] [@editor].

## Request Context

Manual completion sends `textDocument/completion` with trigger kind `1`, while trigger-character completion sends trigger kind `2` plus the character that caused the request [@editor]. Before either request, the editor opens the current buffer through LSP if needed, converts the cursor to an LSP position, and stores the request id in `pending_lsp_edit_requests` only when the client returns a positive id [@editor]. Tests enforce invoked completion, trigger-character completion, and UTF-16 cursor positions after emoji text [@completion-tests].

## Response Guards And Filtering

When a completion response arrives, the editor removes the pending snapshot and rejects the response if the buffer revision or URI changed [@editor]. It also ignores responses whose request URI no longer matches the current buffer, ignores null results, ignores empty item lists, and only shows completions while still in insert mode [@editor].

Red filters a response against text typed after the request position. `completion_filter_for_response` reads the original request position, converts its UTF-16 character offset back to a character index on the current line, compares it with the current cursor position, and returns the intervening text as the UI filter [@editor]. Editor tests cover both plain and UTF-16 filter conversion cases [@editor].

## Terminal UI State

`CompletionUI` stores all response items, the filtered item indexes, selected row, scroll offset, visible bounds, commit characters, and theme-derived styles [@completion-ui]. Showing the menu collects unique commit characters from all items, sorts items by `preselect` and then label, chooses the first preselected item when present, clamps width and row count to terminal bounds, and positions the popup above the cursor when there is not enough room below [@completion-ui]. The fixture response shows the kind of server payload this path handles: incomplete lists with labels, kinds, sort and filter text, preselected items, text edits, and additional text edits [@completion-fixture].

Filtering scores prefix matches ahead of contains matches across `filterText`, label, `sortText`, and `insertText`, then resets selection and scroll to the top of the filtered list [@completion-ui]. The UI renders labels, details, documentation previews, icons, and selected-row styling, but it does not mutate a buffer itself [@completion-ui].

## Atomic Application

Accepting an item first checks that the saved completion snapshot still matches an open buffer and revision; stale items are rejected before mutation [@editor]. The editor validates the main text edit and all additional text edits together with `apply_text_edits`, so invalid UTF-16 positions or overlapping edits leave the buffer unchanged [@editor]. Tests cover one-undo-step application, UTF-16 and CRLF conversion, invalid split-surrogate edits, and overlapping additional edits [@completion-tests].

Application is one editor transaction. Red converts the main LSP text edit or label/insert text into an editor range, strips basic snippet markers for snippet insert text, converts additional edits, sorts edits in descending position order, applies them, computes the final cursor position from the main edit, inserts any commit character, notifies LSP of the change, and commits the transaction [@editor]. If the completion item carries an LSP command, Red sends `workspace/executeCommand` after the edits have been applied [@editor]. Tests enforce snippet marker stripping and command execution after completion [@completion-tests].

