---
title: "Detachable Core Boundary"
summary: "Red keeps detachable sessions alive by making the editor core the long-lived owner and the terminal UI a replaceable local client."
topics: [decisions, sessions, detach, architecture]
sources:
  - id: adr-0002
    type: file
    path: docs/adr/0002-detachable-core.md
  - id: main
    type: file
    path: src/main.rs
  - id: headless
    type: file
    path: src/headless/mod.rs
  - id: editor
    type: file
    path: src/editor.rs
---

Red's detachable-session decision is to put mutable editor ownership in a long-lived headless owner process and keep the terminal UI as a replaceable local client. ADR 0002 accepts that boundary for Linux and macOS: the owner keeps the production editor, runtime services, LSP, persistence, and Codex bridge, while the client owns terminal mode, input collection, and painting [@adr-0002]. Current startup code follows that split by spawning an internal `--core-session` process for `red --detach`, attaching the current terminal over local IPC, and routing later `--attach` and `--stop` requests to the existing owner [@main].

## Status

The decision is accepted and implemented for Linux and macOS [@adr-0002]. The implemented command path still rejects detachable sessions on non-Unix builds with the user-facing fallback to `--resume`, which keeps detach separate from crash recovery [@main].

## Context

Detach exists to survive the terminal lifecycle without turning crash recovery into a live-session transport. ADR 0002 names the problem directly: a dropped SSH terminal must not drop buffers, language servers, plugin processes, file watchers, or an active Codex task [@adr-0002]. That requirement makes the terminal an unreliable place to own editor state. It also makes simple snapshot recovery insufficient, because recovery can reload saved session generations after a process dies but cannot keep in-memory services and active agent work running through a client disconnect [@adr-0002].

The choice also had to avoid a second editor implementation. ADR 0002 states that `DetachedEditorCore` is not a parallel editor and that both interactive paths must use the same editor event and background-service boundary [@adr-0002]. The current `DetachedEditorCore` constructor takes a real `Editor`, keeps its LSP, recovery, and Codex state, initializes a plugin runtime, opens the current buffer with LSP, renders into a terminal-independent buffer, and then exposes logical rows, styles, and cursor positions to the IPC layer [@editor].

## Decision

Red treats the headless owner as the authoritative session. `red --detach[=SESSION]` starts a child owner through the hidden `--core-session` entrypoint with null standard streams and a new Unix process session, waits for the owner to publish its socket, token, and PID files, and then attaches the current terminal as a client [@main]. The owner binds a local Unix-domain socket under Red's run directory, creates owner-private rendezvous files, rejects stale live sessions, and removes the rendezvous files when the bound session drops [@headless].

The terminal client is intentionally thin. In `attach_session`, Red enables raw mode, bracketed paste, focus change, mouse capture, and the alternate screen, then sends normalized key, paste, mouse, resize, focus, heartbeat, detach, and stop messages to the owner [@main]. The client paints only the `RenderDelta` rows and authoritative cursor returned by the owner, so terminal escape sequences and mutable editor state do not cross from the client into the core [@main] [@headless].

The owner processes the real editor. `serve_editor_session` accepts one attached client at a time, keeps a background tick running every 10 ms, clears pending paste state when an attachment ends, and shuts down only when the editor stops or an authenticated stop request is accepted [@headless]. `DetachedEditorCore` converts IPC input back into editor events, calls `Editor::process_editor_event`, services background work, persists session snapshots when needed, and computes changed styled rows and cursor updates from the render buffer [@editor]. Future readers should use [Detachable Editor Core](../../architecture/sessions/detachable-editor-core) for the implementation flow and [Detach IPC Protocol](../../reference/sessions/detach-ipc-protocol) for the exact message contract.

## Consequences

Dropping a client is not the same as quitting the editor. The `Detach` protocol message closes the attachment while leaving the owner alive, and the production test simulates a disappeared terminal by dropping the first client, reconnecting a second client, and verifying that previously applied editor content remains in the owner [@headless]. A normal quit or authenticated stop request still terminates the owner, so detach preserves live state only while the owner process continues to run [@headless].

The owner/client boundary narrows what future detach work may change without a new decision. A client may improve terminal input or painting, but it must not become a second mutable editor, LSP manager, plugin host, persistence writer, or Codex owner [@adr-0002]. Multiple simultaneous clients, remote transport, and TCP attach were rejected as non-goals for this decision; adding them would require a new owner model or a new protocol version rather than a small client-side extension [@adr-0002].

The boundary also keeps [Detach vs Recovery](../../concepts/sessions/detach-vs-recovery) explicit. Detach depends on a live owner and uses local IPC to reconnect a terminal; recovery uses persisted session snapshots after an owner process is gone [@adr-0002]. That distinction is why the Unix detach path can reject Windows while still telling Windows users to use `--resume` instead of promising a transport it does not implement [@main].
