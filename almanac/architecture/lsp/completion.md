---
title: "LSP Completion"
summary: "Red completion combines automatic and manual insert-mode requests, open-buffer fallback items, UTF-16-aware filtering, active-session stale guards, terminal UI pass-through, atomic edit application, snippet cleanup, and optional follow-up LSP commands."
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

LSP completion in Red spans the [transport](transport), editor, and terminal UI. The editor issues completion only from insert mode, records a buffer id, revision, URI, cursor, and replacement-range snapshot, sends UTF-16 line and character positions to the language server, and later accepts a response only when the active completion session is still current [@editor]. Completion also has an editor-owned fallback source: matching words from open buffers become completion items with LSP-shaped replacement edits, so completion can still work when no language server is available [@editor]. The completion UI owns display state and filtering, while accepting an item returns to the editor for validated buffer mutation and optional command execution [@completion-ui] [@editor].

## Request Context

Manual completion sends `textDocument/completion` with trigger kind `1`, while trigger-character completion sends trigger kind `2` plus the character that caused the request [@editor]. Automatic completion is scheduled after ordinary keyword characters when `[completion].auto_trigger` is enabled and the prefix is at least `[completion].min_prefix_length`; the background service fires the request only if insert mode, buffer id, revision, and cursor still match the scheduled snapshot [@editor]. Before an LSP request, the editor opens the current buffer through LSP if needed, converts the cursor to an LSP position, and stores the request id in `pending_lsp_edit_requests` only when the client returns a positive id [@editor]. Tests enforce invoked completion, trigger-character completion, automatic buffer-word fallback, and UTF-16 cursor positions after emoji text [@completion-tests] [@editor].

Buffer-word completions scan the active buffer first, then other open buffers, deduplicate candidates case-insensitively, cap the scan and result count, and generate `Text` completion items whose text edit replaces the current identifier prefix [@editor]. `merge_completion_items` keeps LSP candidates ahead of buffer-word candidates and drops buffer duplicates when the language server returned the same label [@editor].

## Response Guards And Filtering

When a completion response arrives, the editor removes the pending snapshot and rejects the response if the original request no longer matches the active buffer id, revision, and URI [@editor]. It also ignores responses whose request URI no longer matches the current buffer, ignores empty LSP item lists unless buffer items exist, and only shows completions while still in insert mode [@editor]. A null LSP result can still show buffer-word completions, because `request_completion` keeps the request-time buffer candidates in `pending_buffer_completions` and `show_completion_items` can render those items without server candidates [@editor].

Red filters a response against the identifier prefix around the original request position. `completion_filter_for_response` reads the original request position, converts its UTF-16 character offset back to a character index on the current line, walks backward over keyword characters, compares that range with the current cursor position, and returns the active identifier prefix as the UI filter [@editor]. Editor tests cover plain, UTF-16, and "identifier before request position" filter cases, including a broad Python response where `xb` must beat unrelated `BaseException` items [@editor].

The active-session snapshot can advance during ordinary typed characters and backspace while the completion popup remains open [@editor] [@completion-ui]. `CompletionUI` allows those events to pass through; after the editor applies the text change, `refresh_completion_snapshot_after_passthrough` updates the completion revision and current replacement range only when the document identity is unchanged and the replacement still begins at the original prefix start [@editor] [@completion-ui]. This lets acceptance rebase safe main and additional text edits after continued typing, while unrelated buffer changes still fail the stale snapshot check before mutation [@editor].

## Terminal UI State

`CompletionUI` stores all response items, the filtered item indexes, selected row, scroll offset, visible bounds, commit characters, and theme-derived styles [@completion-ui]. Showing the menu collects unique commit characters from all items, sorts items by `preselect`, `sortText` or label, and then label, chooses the first preselected item when present, clamps width and row count to terminal bounds, and positions the popup above the cursor when there is not enough room below [@completion-ui]. The renderer derives a display label by trimming leading whitespace and bullet glyphs from the item label while keeping the original completion item as the insertion payload, so server-provided presentation markers do not shift the label column or change the accepted edit [@completion-ui]. The fixture response shows the kind of server payload this path handles: incomplete lists with labels, kinds, sort and filter text, preselected items, text edits, and additional text edits [@completion-fixture].

Filtering scores prefix matches ahead of contains matches against `filterText` when present, otherwise against the label; `sortText` and `insertText` do not make an item match a typed prefix [@completion-ui]. Refiltering resets selection and scroll to the top of the filtered list, recomputes the visible height from the current match count, and leaves zero-match popups non-rendering but still active until a key action closes them [@completion-ui]. The UI renders labels, details, documentation previews, icons, selected-row styling, and scroll indicators on the border rows, but it does not mutate a buffer itself [@completion-ui].

Completion key handling preserves modal editing behavior when filtering removes every visible candidate. `Enter` applies the selected item and closes the dialog when a match is selected, but with no selected item it closes the dialog and inserts a newline in the same action [@completion-ui]. `Esc` always closes the completion dialog and enters Normal mode in one action, whether candidates are visible or filtered to zero [@completion-ui]. Tests cover both the Python call newline regression and the invisible completion `Esc` regression through editor-level event handling [@editor].

## Atomic Application

Accepting an item first checks that the saved completion snapshot still matches the active buffer and current revision; stale items are rejected before mutation [@editor]. If the user typed within the active completion session after the popup opened, Red rebases the main text edit from the original range to the current range and also rebases safe additional edits; an edit that cannot be rebased fails before mutation [@editor]. The editor validates the main text edit and all additional text edits together with `apply_text_edits`, so invalid UTF-16 positions or overlapping edits leave the buffer unchanged [@editor]. That validation uses the same conversion rules described in [LSP Workspace Edits](workspace-edits), then enters the editor-owned [Text Mutation Boundary](../editor/text-mutation-boundary) for the actual buffer change. Tests cover one-undo-step application, UTF-16 and CRLF conversion, invalid split-surrogate edits, overlapping additional edits, continued typing, backspace, commit characters, LSP edit rebasing, stale-edit rejection, and bounded single-item rendering [@completion-tests] [@editor] [@completion-ui].

Application is one editor transaction. Red converts the rebased main LSP text edit or label/insert text into an editor range, strips basic snippet markers for snippet insert text, converts additional edits, sorts edits in descending position order, applies them, computes the final cursor position from the main edit, inserts any commit character, notifies LSP of the change, and commits the transaction [@editor]. When an item has no LSP text edit, the editor uses the active completion range instead of plain insertion, so accepting a label or `insertText` replaces the typed identifier prefix rather than appending after it [@editor]. If the completion item carries an LSP command, Red sends `workspace/executeCommand` after the edits have been applied [@editor]. Tests enforce snippet marker stripping and command execution after completion [@completion-tests].
