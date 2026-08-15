---
title: "Crash Recovery Snapshots"
summary: "Red persists editor-owned session snapshots so a later `red --resume` can recover dirty buffers, layout, undo history, plugin storage, and agent conversation context without writing recovered text to disk."
topics: [sessions, recovery, architecture]
sources:
  - id: session-store
    type: file
    path: src/session.rs
  - id: session-manager
    type: file
    path: src/editor/session_manager.rs
  - id: editor-session
    type: file
    path: src/editor.rs
  - id: main-entry
    type: file
    path: src/main.rs
  - id: recovery-doc
    type: file
    path: docs/SESSION_RECOVERY.md
---

Crash recovery snapshots are Red's disk-backed answer to losing the editor owner process. The editor periodically writes a schema-v2 `SessionSnapshot` under an owner namespace in the user configuration directory, and `red --resume` chooses the newest useful snapshot, rebuilds editor state in memory, reports disk divergence, and continues saving into the same namespace [@session-store] [@main-entry]. This mechanism is separate from live detach: [detach](detachable-editor-core) keeps a running owner alive, while [recovery](../../concepts/sessions/detach-vs-recovery) restores the last durable state after the owner is gone.

## Snapshot Contents

`SessionSnapshot` is a durable schema rather than an editor cache. It records the schema version, generation, working directory, capture time, open buffers, active buffer index, window layout, registers, jumps, local marks, global marks, special marks, last visual selections, optional agent transcript text, optional structured agent conversation, a legacy `agent_workspace` compatibility field, an agent resumability flag, plugin extensions, and unknown legacy extensions [@session-store]. Each buffer entry stores the buffer index, canonical path when present, full in-memory contents, dirty bit, revision, cursor, viewport, undo tree, and the disk text observed at capture time [@session-store].

The editor builds the snapshot from live editor state. It synchronizes the window state, commits buffer undo transactions with the visible cursor, snapshots marks and last-visual-selection metadata through buffer IDs, captures agent transcript storage, captures the `AgentManager` conversation snapshot, and records plugin storage extensions [@editor-session]. On restore, Red reconstructs buffers first, then reapplies the window layout, registers, jumps, marks, last visual selections, agent conversation, plugin storage extensions, legacy plugin imports, and archived transcript warning state [@editor-session].

## Owner Namespaces

The snapshot root is `Config::path("sessions")`. Normal interactive editors receive an owner like `editor-<uuid>`, detached owners receive `detached-<session>`, and a resumed editor reuses the store that supplied the snapshot [@main-entry]. `SessionStore::for_owner` restricts owner names to a single safe path component and rejects a symlinked root before it joins the owner directory [@session-store].

`SessionStore::load_latest_with_store` scans the root store and every owner directory below the root. It ranks snapshots with dirty buffers ahead of clean snapshots, then uses capture time and generation to break ties [@session-store]. That ranking is why a dirty older editor can beat a newer clean exit when the user runs [Resume after crash](../../guides/sessions/resume-after-crash).

## Atomic Rotation

Each write updates the snapshot to the current schema version, increments the generation, serializes JSON, and rejects files larger than 256 MiB [@session-store]. The write path creates a unique temporary file with owner-only permissions, writes and syncs it, rotates `latest.json` to `previous.json` when the current latest generation is valid, renames the temporary file to `latest.json`, and syncs the directory [@session-store]. If a write fails after creating the temporary file, the temporary file is removed [@session-store].

The rotation logic is deliberately conservative. If `latest.json` is invalid or missing, writes validate `previous.json` before replacing the latest slot, and they refuse to write over an invalid latest when no last-known-good snapshot exists [@session-store]. Tests cover failure after temporary-file sync and after generation rotation, preserving the previous loadable generation in both cases [@session-store].

## Background Writes

`SessionManager` owns the editor's snapshot cadence. Its default interval is five seconds, it tracks the last persisted render generation, it holds an in-flight writer thread, and it stores the current recovery warning [@session-manager]. The interactive event loop services background work and then calls `persist_session_snapshot(false)`, while shutdown forces one final snapshot before plugin deactivation [@editor-session].

Snapshot writes avoid blocking input on large buffers. The editor captures cheap per-buffer content snapshots on the main thread, builds the durable snapshot without contents, and then fills buffer contents plus trusted disk bases in a worker thread before calling `SessionStore::write` [@editor-session]. If the writer fails or panics, the editor sets the warning text `Crash recovery is not being saved; check free space and permissions or reduce open-buffer size` [@editor-session].

## Disk Divergence

For file-backed buffers, recovery stores both the dirty in-memory text and the trusted disk base seen when the snapshot was written [@session-store]. The snapshot writer captures a file fingerprint before the worker reads disk contents, and the read verifies identity, size, and metadata before and after reading [@session-store]. The disk base is limited to 8 MiB, so oversized or unsafe files do not become trusted bases [@session-store].

On resume, `detect_disk_divergence` compares each stored disk base with the current file. Unreadable files are treated as divergences, and the returned unified diff is diagnostic only; the function does not change the recovered buffer or write the current file [@session-store]. The main entrypoint prints each divergence as `Recovered <path> with external disk changes:` before the editor runs [@main-entry].

## Resume Boundary

`red --resume` loads the chosen snapshot, changes to its saved working directory when present, reconstructs buffers from the snapshot, restores the editor state, reports divergences, and assigns the resumed store back to the editor [@main-entry]. Recovered dirty contents remain in memory; the recovery documentation states that Red never writes them to disk until the user explicitly saves [@recovery-doc].

Agent state follows the same boundary. The snapshot can restore structured conversation state and flat transcript text, but legacy transcript-only recovery is archived context and Red does not pretend that a Codex process survived a crash [@session-store] [@editor-session]. New snapshots derive `agent_session_resumable` from the presence of `agent_conversation`, while the legacy `agent_workspace` payload is accepted on load and not serialized back out [@session-store]. For the live-owner alternative, read [Detachable editor core](detachable-editor-core).
