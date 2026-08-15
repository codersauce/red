---
title: "Detachable Editor Core"
summary: "Red's detachable editor core keeps the real editor alive in a Unix owner process while terminal clients connect, disconnect, and repaint over local IPC."
topics: [sessions, detach, architecture]
sources:
  - id: detach-doc
    type: file
    path: docs/DETACH.md
  - id: main-entry
    type: file
    path: src/main.rs
  - id: headless-ipc
    type: file
    path: src/headless/mod.rs
  - id: editor-core
    type: file
    path: src/editor.rs
  - id: detach-tests
    type: file
    path: tests/detach.rs
---

The detachable editor core is Red's live-session architecture for Unix terminals. `red --detach` starts a background owner process that contains the real editor, buffers, LSP client, plugin runtime, Codex task, recovery store, and render model; attached clients only send normalized terminal input and paint render deltas [@main-entry] [@editor-core]. This differs from [crash recovery](crash-recovery-snapshots): detach protects against terminal or SSH loss while the owner still runs, and recovery restores a persisted snapshot after the owner is gone [@detach-doc]. The accepted owner/client boundary is recorded in [Detachable Core Boundary](../../decisions/sessions/detachable-core-boundary).

## Owner And Client Split

The public detach path starts in `src/main.rs`. `red --detach=SESSION` spawns the current executable with the hidden `--core-session SESSION` flag, detaches it from the controlling terminal with `setsid`, forwards root/config/typecheck options and file arguments, waits up to five seconds for the owner socket, token, and PID file, and then attaches the current terminal [@main-entry]. `red --attach SESSION` connects to an existing owner, and `red --stop SESSION` asks an owner to shut down [@main-entry].

The owner binds a private Unix socket under the runtime directory, creates a reconnect token and PID file, and removes stale rendezvous files only after checking that they do not identify a live matching socket owner [@headless-ipc]. The detachable-session documentation states that sessions are local to the current OS user and use a private Unix socket and reconnect token rather than a TCP port [@detach-doc].

## Live Editor Ownership

`DetachedEditorCore` owns the production editor, not a simplified text model. Its constructor disables terminal output, creates a fresh Husk runtime, refreshes plugin snapshots, registers configured plugins, sends `editor:ready`, restores agent transcript notifications when present, opens the current buffer for LSP, renders the first frame, and keeps the editor, runtime, render buffer, rows, styled spans, cursor, stop flag, and pending paste buffer in the owner process [@editor-core].

Client input is converted back into Crossterm events and passed through the normal editor event path. After each input, resize, or focus event, the core services background work, persists a recovery snapshot when due, re-renders if the snapshot warning changed, and serializes only changed rows plus the cursor as a `RenderDelta` [@editor-core]. Large paste chunks are accumulated in the owner and applied only when the final chunk arrives, so one paste can remain one editor transaction [@editor-core].

## Background Ticks

Detach is useful because the owner keeps running when no client is attached. `serve_editor_session` accepts at most one interactive client, but it also runs a 10 ms background interval that calls `DetachedEditorCore::tick` while the session is detached or idle [@headless-ipc]. The core tick services plugin processes, LSP messages, timers, directory watches, and agent events, persists recovery snapshots when due, and emits a render delta only if the editor render generation changed [@editor-core].

Tests exercise that ownership boundary. The detach integration fixture starts a mock Codex app-server, edits through one client, drops that client, verifies the original Codex process is still alive, reconnects, and asserts that reattach did not restart the process [@detach-tests]. Other detached-core tests cover background agent events, plugin cursor requests, chunked paste, and resize notification through the native editor path [@editor-core].

## Reconnect And Stop Behavior

Only one TUI may attach to a detached session at a time [@detach-doc]. The owner tracks whether a client is attached; a second interactive connection receives a busy error, while an authenticated stop-control connection is still allowed to stop the owner [@headless-ipc]. When a connection ends, the owner clears pending paste state and marks the session available for another client [@headless-ipc].

An attached client enables raw mode, bracketed paste, focus change, mouse capture, an alternate screen, keyboard enhancement flags, and disabled line wrap before painting the initial render [@main-entry]. `Ctrl-\` or raw `Ctrl-4` sends `Detach`, which leaves the owner alive; `red --stop SESSION` uses a control connection and stops the owner after token authentication [@main-entry] [@headless-ipc]. For exact message shapes and timeouts, use the [Detach IPC protocol](../../reference/sessions/detach-ipc-protocol).

## Platform Boundary

Detach is implemented for Unix in this release. The code paths for start, attach, stop, and core-session mode bail on non-Unix platforms with the message that detach is available on Linux and macOS and that Windows users should use `--resume` [@main-entry]. The documentation states the same product boundary and says named-pipe support is deferred rather than replaced with an insecure transport [@detach-doc].
