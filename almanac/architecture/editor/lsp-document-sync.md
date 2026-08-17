---
title: "LSP Document Sync"
summary: "LSP document sync is the editor-side contract that lazily opens file buffers, sends changes after canonical mutations, tracks diagnostics by URI, and keeps server document identity aligned with buffer file changes."
topics: [architecture, editor, lsp]
sources:
  - id: coordinator
    type: file
    path: src/editor/lsp_coordinator.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: edit-batch
    type: file
    path: src/editor/edit_batch.rs
  - id: client
    type: file
    path: src/lsp/client.rs
  - id: lazy-tests
    type: file
    path: tests/lsp_lazy.rs
---

LSP document sync in Red is owned by the editor, not by the LSP manager. The manager routes requests to server processes, while `LspCoordinator` records which document URIs the editor has told servers are open and which buffer revisions have already been delivered [@coordinator]. The editor opens a file buffer only when an LSP operation needs it, publishes canonical edits or a full-content fallback after the editor mutation boundary, stores diagnostics by normalized URI, and closes or reopens documents when buffer identity changes [@editor]. This layer connects user actions from the [event loop](event-loop) to the process routing described by [LSP Client Lifecycle And Routing](../lsp/client-lifecycle-and-routing).

## Coordinator State

`LspCoordinator` tracks opened URI strings, the latest delivered revision per `BufferId`, and ordered pending canonical edits with their pre-edit Rope snapshot and revision chain. It also caches whether a revision uses line endings supported by the direct UTF-16 conversion [@coordinator]. `with_buffers` seeds revision state from existing buffers but starts with no opened documents, so constructing an editor does not imply `didOpen` [@coordinator].

The coordinator is deliberately small. It can mark a URI opened or closed, clear all opened documents, record or seed a notified revision, test whether a revision is already notified, and forget a closed buffer [@coordinator]. That gives the editor enough memory to suppress duplicate opens, skip redundant flushes, and discard per-buffer revision state when a buffer is deleted [@coordinator].

## Lazy Open

The editor opens LSP documents through `ensure_current_buffer_lsp_opened` and `ensure_buffer_lsp_opened`. A buffer without a file or URI is ignored, an already-open URI is ignored, and a new file-backed URI sends `did_open` with the buffer's current contents before marking the URI opened [@editor]. Tests enforce the behavior: editor construction does not open an inactive LSP buffer, activating a current LSP buffer opens it once, switching away and back does not reopen it, and hover opens the active buffer before sending the hover request [@lazy-tests].

Most LSP-facing actions call the lazy-open helper before issuing their request. The tests cover formatting, code actions, signature help, rename, document symbols, workspace symbols, references, and split-opened files as operations that open the active target before they use LSP [@lazy-tests]. This lets the editor keep startup cheap while still making request-time document contents available to language servers.

## Change Delivery

`notify_change` is the editor-side sync point after a content mutation. Local edit batches queue changed buffers by stable identity; nested dot, macro, counted, and block replay share the outer publication boundary. Unknown or external actions flush first, and mode transitions remain observable to plugins [@edit-batch]. Publication ensures the target buffer is open, sends its document change, emits `buffer:changed`, and records the delivered revision without switching the active window [@editor].

Canonical replacements retain ordered UTF-16 ranges and cheap before/after Rope snapshots. Adjacent same-line insertions can merge. An incremental server receives those ranges only when its cached preimage matches exactly; full-sync servers, revision gaps such as undo, lazy-open mismatches, and unsupported line separators retain the full-text fallback [@coordinator] [@client]. Document versions and debounced diagnostics still advance through the client [@client].

`flush_change_notification` reads the active buffer revision and returns early if the coordinator already recorded that exact revision [@editor]. Otherwise it calls `notify_change`, which keeps delayed or grouped changes from leaving the LSP and plugin observers behind the current buffer state [@editor]. This follows the same canonical mutation boundary described by [Text Mutation Boundary](text-mutation-boundary): direct buffer helpers can change text, but production actions must pass through the editor path that opens transactions and notifies observers.

## Diagnostics And URI Identity

Diagnostics are stored under normalized file URIs. `add_diagnostics` rejects messages with no URI, normalizes URI aliases through a file path when possible, converts the path back to a canonical file URI, and stores the diagnostic vector before refreshing the UI [@editor]. `handle_lsp_message` accepts both pull diagnostics responses and publish diagnostics notifications when `show_diagnostics` is enabled [@editor].

Buffer identity changes keep the diagnostic map and server state aligned. `sync_lsp_document_identity` compares the previous URI with the buffer's current URI, sends `did_close` for the old URI when it had been opened, moves any old diagnostics to the new URI, and then ensures the new buffer identity is opened [@editor]. Tests cover `Save As` closing the old LSP document and opening the new identity, and they cover diagnostics URI aliases normalizing to the current buffer URI [@lazy-tests].

## Close And Workspace Edit Interactions

Deleting a buffer closes its LSP document only when no other open buffer still has the same URI. The editor checks for another buffer with the same URI, sends `did_close` only if the coordinator marked the URI closed, removes diagnostics for that URI, and forgets the removed buffer id [@editor]. That rule matters in split and multi-buffer workflows because closing one view must not close the server document while another buffer still represents it.

Workspace edits and rename responses depend on revision snapshots captured at request time. The editor rejects stale formatting, code-action, rename, and completion responses when the pending buffer id, revision, or URI no longer matches the current buffer state [@editor]. It also validates workspace-edit origins against the originating workspace before applying operations, which links document sync to the broader [workspace edits](../lsp/workspace-edits) contract [@editor].
