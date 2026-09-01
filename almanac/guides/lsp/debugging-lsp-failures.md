---
title: "Debugging LSP Failures"
summary: "This guide shows how to diagnose Red LSP failures by separating startup and routing problems from transport, diagnostics, completion, and fail-closed workspace edit handling."
topics: [guides, lsp, debugging, workspace-edits]
sources:
  - id: debugging
    type: file
    path: docs/DEBUGGING.md
  - id: manager
    type: file
    path: src/lsp/manager.rs
  - id: client
    type: file
    path: src/lsp/client.rs
  - id: edit
    type: file
    path: src/lsp/edit.rs
  - id: workspace-edit
    type: file
    path: src/lsp/workspace_edit.rs
  - id: go-lsp-session
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/31/rollout-2026-08-31T18-03-49-01a05a10-8f88-7f70-9850-f35913a6ac1b.jsonl
---

Use this guide when language-server behavior is missing, delayed, stale, or rejected in Red. The fastest path is to identify which LSP boundary owns the failed invariant: startup and routing, JSON-RPC transport, document synchronization, diagnostics, completion, or workspace edit preparation [@debugging] [@client] [@workspace-edit]. Red intentionally keeps editor mutation outside the LSP background tasks, so server output is evidence for the editor to interpret rather than proof that visible buffers changed [@debugging].

## Confirm Configuration And Logs First

Start with `red --check-config` if the failure could be caused by a disabled LSP setting, an invalid server command, or an unexpected fallback. The debugging guide recommends checking configuration before downstream behavior because recoverable diagnostics explain the source path and fallback behavior [@debugging]. For exact LSP config fields and default servers, use [LSP Configuration](../../reference/lsp/configuration).

Then search the log for `[lsp]`. The debugging guide identifies `src/lsp/manager.rs` as the owner of document-to-client routing and `src/lsp/client.rs` as the owner of process stdio, initialization, request IDs, pending queues, document versions, diagnostic debounce, and shutdown [@debugging]. Use [LSP Client Lifecycle And Routing](../../architecture/lsp/client-lifecycle-and-routing) when the symptom is "no server starts for this file"; use [LSP Transport](../../architecture/lsp/transport) when a started server does not answer correctly.

## Startup And Transport Failures

If the server never becomes usable, inspect the process and initialization path before looking at editor UI. `RealLspClient::start` spawns the configured command directly with its args and environment, opens stdin/stdout/stderr, and uses bounded channels between reader, writer, and editor-side polling [@client]. Requests and notifications sent before successful initialization are queued only within the documented message and byte budgets; exceeding that queue marks initialization as failed [@client].

A missing server executable is a process-start problem. Because the command is launched directly rather than through a shell, check `red --check-config`, the configured `command`, `command -v` in the same launch environment, the live process tree, and `[lsp] failed to start client ... os error 2` log entries before debugging completion or diagnostics UI [@client] [@go-lsp-session]. When `LspManager` catches a startup or initialization error it inserts the `(server, workspace)` key into `failed_clients`; later requests for that key return no client, and status reports `failed` until Red restarts or an LSP reconfiguration clears the affected key [@manager]. The 2026-08-31 Go diagnosis followed this pattern: the installed Go pack configured a bare `gopls` command, no `gopls` child existed, and installing the binary required restarting the active Red process before the Go workspace could retry [@go-lsp-session].

Transport failures usually show up as one of these owners:

| Symptom | First place to inspect |
| --- | --- |
| No response after a request | pending response correlation and request timeout in `RealLspClient::poll` [@client] |
| Invalid server output | bounded frame parsing for `Content-Length`, frame bytes, header bytes, and UTF-8 body parsing [@client] |
| Server logs look noisy | stderr tail handling; only fatal, panic, and thread-panic-looking lines are surfaced as server errors [@client] |
| Request before initialization fails later | pending message queue limits and failed pending request reporting [@client] |

Responses with an `id` and no `method` are matched as client request responses, while server-to-client requests also have ids but must not complete Red's pending client requests [@client]. This distinction matters when a server sends workspace prompts or configuration requests while a Red request is outstanding.

## Diagnostics And Completion Failures

Diagnostics are deliberately debounced. The client waits 250 ms after the last document change before requesting diagnostics because typing can produce one `didChange` per keystroke [@client]. If diagnostics appear stale, check whether the server advertises diagnostic support, whether the document URI is normalized consistently, and whether the pending diagnostic entry has been flushed by polling.

Completion failures need a different path. Completion requests depend on the transport request context and are later interpreted by editor UI code, so stale or replaced completion state is usually not the same issue as a failed server process. Start with the request id in `RealLspClient`, then move to [LSP Completion](../../architecture/lsp/completion) if the response arrived but UI filtering, snippet handling, or atomic edit application behaved incorrectly.

## Workspace Edit Rejections

For failed edits, inspect conversion and preparation before assuming the server or editor is wrong. Red parses LSP workspace edits into ordered operations, rejects ambiguous `changes` plus `documentChanges`, rejects change annotations that require confirmation, validates file URIs, and preserves ordered create, document, rename, and delete operations [@edit].

Text edits are converted without applying them first. The conversion checks UTF-16 positions against real scalar boundaries, rejects invalid line or character positions, rejects ranges that end before they start, and rejects overlapping edits [@edit]. This is why a server edit can be rejected even when the text looks close in a UTF-8 editor view; LSP positions are UTF-16, and Red refuses split-surrogate boundaries.

Multi-document edits go through a fail-closed preparation layer. `prepare_workspace_edit` validates the complete operation list before editor buffers or filesystem resources mutate; it checks operation count, workspace confinement, missing and non-UTF-8 files, expected revisions, LSP document versions, protected paths, and total content size [@workspace-edit]. Resource operations record snapshots and attempt rollback if a later operation fails, but rollback is refused if a target changed concurrently [@workspace-edit]. See [LSP Workspace Edits](../../architecture/lsp/workspace-edits) for the full architecture.

## Recovery Rules

Do not loosen path checks or write proposed workspace-edit results directly to disk as a workaround. The debugging guide explicitly describes workspace edit failure as intentionally closed and says retrying with a looser path is not a supported recovery technique [@debugging]. If a rejection is wrong, fix the conversion, preparation, or editor-owned application path that produced the rejection.

Use deterministic tests when changing LSP behavior. The debugging guide notes that tests which launch real configured language servers are opt-in through `RED_RUN_REAL_LSP_TESTS`; default integration coverage uses mock servers so protocol ordering and failure paths remain deterministic [@debugging]. That means most transport, routing, and workspace-edit fixes should be reproducible without depending on a user's installed server.
