---
title: "Sessions Architecture"
summary: "Sessions architecture routes readers through Red's detachable live owner, crash-recovery snapshots, detach/recovery distinction, IPC contract, guides, and boundary decision."
topics: [architecture, sessions, detach, recovery, persistence]
sources:
  - id: main
    type: file
    path: src/main.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: session
    type: file
    path: src/session.rs
  - id: headless
    type: file
    path: src/headless/mod.rs
---

# Sessions Architecture

Red has two session mechanisms with different failure models. Detach keeps a live Unix owner process running while terminal clients disconnect and reconnect, and crash recovery writes editor-owned snapshots so `red --resume` can restore dirty buffers, layout, undo history, plugin storage, and agent conversation context after the owner is gone [@main] [@editor] [@session]. Use this hub to choose between the live-owner path, the snapshot path, and the references or guides that explain their contracts.

## Reading Order

Start with [Detach Versus Recovery](../../concepts/sessions/detach-vs-recovery) when deciding which session mechanism applies. The runtime entrypoint routes `--detach`, `--attach`, `--stop`, hidden `--core-session`, and `--resume` through separate startup branches, which keeps reconnecting to a live owner distinct from loading a persisted snapshot [@main].

Read [Detachable Editor Core](detachable-editor-core) for the live Unix owner model. `DetachedEditorCore` keeps the production editor in the owner process, converts protocol input back into editor events, services background work, persists snapshots when due, and serializes render deltas for clients [@editor].

Read [Crash Recovery Snapshots](crash-recovery-snapshots) for persisted recovery. `SessionSnapshot` stores schema version, generation, working directory, buffers, window layout, registers, jumps, marks, last visual selections, plugin extensions, optional agent transcript text, optional structured agent conversation, and legacy compatibility fields; `SessionStore` writes and loads `latest.json` and `previous.json` generations under owner namespaces [@session].

Use [Detach IPC Protocol](../../reference/sessions/detach-ipc-protocol) for exact message shapes, authentication, limits, render deltas, and errors. The headless protocol defines versioned client and server messages, local reconnect-token authentication, terminal-independent input events, heartbeat behavior, one interactive client, and stop control [@headless].

For operations, use [Detach And Reattach](../../guides/sessions/detach-reattach) when the owner is alive, and [Resume After Crash](../../guides/sessions/resume-after-crash) when the owner crashed or the machine restarted. For the architectural choice behind the owner/client split, read [Detachable Core Boundary](../../decisions/sessions/detachable-core-boundary).

## Boundaries To Preserve

Do not treat detach as snapshot recovery. Detach depends on a still-running owner and local IPC, while recovery reconstructs editor state from persisted JSON after process loss [@main] [@session] [@headless].

Do not move editor ownership into the terminal client. The detached core keeps `Editor` in the owner process and exposes only rendered rows, styled spans, cursor positions, and control responses to clients [@editor] [@headless].

Do not make recovery write restored text automatically. Recovery restores buffer contents into editor memory and uses divergence reporting to warn about external disk changes; saving remains an explicit user action [@session].
