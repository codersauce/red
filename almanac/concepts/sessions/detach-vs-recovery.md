---
title: "Detach Versus Recovery"
summary: "Detach keeps a live Unix editor owner running, while recovery restores persisted snapshots after owner loss or restart."
topics: [concepts, sessions, detach, recovery]
sources:
  - id: detach-doc
    type: file
    path: docs/DETACH.md
  - id: recovery-doc
    type: file
    path: docs/SESSION_RECOVERY.md
  - id: headless
    type: file
    path: src/headless/mod.rs
  - id: session
    type: file
    path: src/session.rs
  - id: main
    type: file
    path: src/main.rs
---

Detach and recovery solve different Red session failures. Detach is a live Unix owner model: the editor, unsaved buffers, LSP servers, plugins, and running Codex app-server process can continue after the attached terminal or SSH connection disappears [@detach-doc]. Recovery is a persisted snapshot model: `red --resume` loads the newest valid snapshot with dirty buffers or the newest clean snapshot when no work is pending, restores editor state in memory, and can reconnect structured agent conversations through a replacement app-server when the snapshot has a persisted Codex thread binding [@recovery-doc].

For the implementation reading map across both mechanisms, start with [Sessions architecture](../../architecture/sessions) before choosing the detach or recovery path.

## Live Detach

Detach starts a persistent owner and then attaches the current terminal to it. The documented commands are `red --detach=refactor ...`, `red --attach refactor`, and `red --stop refactor`; `red --detach` without a value uses the `default` session, and only one TUI may attach to a session at a time [@detach-doc]. In the implementation, `start_detached_owner` launches the current executable with the hidden `--core-session` flag, starts it in a new Unix session with `setsid`, waits for the socket, token, and PID files, and then calls the attach path [@main].

The transport is local IPC, not persistence. `src/headless/mod.rs` defines protocol version 3 over Unix sockets with terminal-independent input events, render deltas, reconnect tokens, detach and stop messages, frame and paste limits, heartbeat timing, and one attached interactive client [@headless]. The detach documentation also states that sessions are local to the current OS user and use a private Unix socket and reconnect token rather than a TCP port [@detach-doc]. For operations, read [Detach and reattach](../../guides/sessions/detach-reattach) and [Detachable editor core](../../architecture/sessions/detachable-editor-core).

## Crash Recovery

Recovery is written to disk under the configuration directory's `sessions/<owner>/latest.json` namespace, with separate namespaces for each editor and named detached owner [@recovery-doc]. `SessionSnapshot` is the durable schema: it stores working directory, buffers, window layout, registers, jumps, marks, undo history, plugin extensions, optional agent transcript text, optional structured agent conversation state, and a legacy `agent_workspace` field that is accepted for backward-compatible loading but skipped when new snapshots are written [@session].

Visual-selection recovery uses both marks and dedicated selection metadata. The snapshot stores special marks plus buffer-indexed last visual selections, so a recovered editor can restore `gv` shape and direction without treating the selection as live process state [@session].

The entrypoint loads recovery only for `--resume`. It calls `SessionStore::load_latest_with_store`, changes to the snapshot working directory when present, reconstructs buffers from the snapshot, restores editor session state, reports disk divergence, and continues using the store that supplied the resumed snapshot [@main]. The detailed workflow belongs in [Crash recovery snapshots](../../architecture/sessions/crash-recovery-snapshots) and [Resume after crash](../../guides/sessions/resume-after-crash).

## The Boundary

The important distinction is whether the owner process still exists. A dropped terminal connection leaves the detached owner running, so reattach can reconnect to the same in-memory editor and running agent process [@detach-doc]. If the owner crashes or the machine restarts, only the last crash-safe snapshot remains. New agent snapshots can store `agent_conversation` with a Codex thread binding, so recovery restores that conversation into editor state and waits for a replacement app-server to rejoin and reconcile the thread when possible [@recovery-doc] [@session]. Legacy flat transcripts and conversations whose Codex thread is missing become archived context instead of pretending the old process survived [@recovery-doc]. [Detachable Core Boundary](../../decisions/sessions/detachable-core-boundary) records the decision behind that owner/client split.

The storage layer reinforces that boundary. `SessionStore::load_latest_with_store` ranks recoverable dirty-buffer snapshots ahead of newer clean snapshots, falls back from invalid `latest.json` to `previous.json`, rejects future schema versions, and writes by syncing a temporary file before rotating generations [@session]. That makes recovery reliable for editor state, but it is not a substitute for the live IPC guarantees of detach.
