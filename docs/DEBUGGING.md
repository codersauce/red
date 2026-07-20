# Debugging Red

Start with the narrowest owner of the failed invariant. Red deliberately keeps editor mutation on the core task, so background process output is evidence delivered to that owner rather than proof that visible editor state changed.

## Startup, configuration, and runtime assets

Run `red --check-config` before debugging downstream behavior. It prints every recoverable configuration diagnostic with its source path and fallback; malformed whole-file input uses the safe profile rather than partially trusting unknown values. `src/config.rs` owns layering and diagnostics, while `src/main.rs` adds runtime validation for themes and log paths.

Run `red --runtime-files` when a plugin or theme is absent or unexpectedly old. Resolution order is user configuration, `RED_RUNTIME`, then embedded assets. The listing marks shadowed sources, so a user copy or development runtime can be identified without guessing which bytes were loaded.

The default log is `/tmp/red.log`. A configured `log_file` replaces that destination. Failure to open the configured path becomes a configuration diagnostic and disables logging rather than preventing startup.

## Input, editing, undo, and rendering

Trace a content change through `Editor::process_editor_event`, key resolution, `Editor::execute`, transaction start, `Editor::replace_range`, transaction commit, and `Editor::notify_change`. `Editor::replace_range` asserts that a transaction is active; `Buffer::replace_range_raw` is the lower seam used by the editor and undo replay and must not be a new feature entry point.

When text is correct but the cursor or paint is wrong, identify the coordinate system before inspecting arithmetic. Editor cursor `x` is a grapheme index, `TextPosition::character` is a Unicode scalar index, rendering uses terminal columns, tree-sitter spans use UTF-8 bytes, and LSP positions use UTF-16 code units. `src/unicode_utils.rs` owns the named conversions and `src/editor/display_layout.rs` owns wrapped-row mapping.

Run with `RED_PERF=summary` for aggregate event, render, plugin, detach, and session timings. Use `RED_PERF=trace` only for a short reproduction because per-sample logging perturbs the measured path and may produce a large log. If a visible non-text update is missing, inspect whether the feature requested a render or advanced the editor render generation; a buffer revision alone covers only text-derived caches.

## Language servers

Search the log for `[lsp]`. `src/lsp/manager.rs` owns document-to-client routing, while `src/lsp/client.rs` owns process stdio, initialization, request IDs, pending queues, document versions, diagnostic debounce, and shutdown. An incoming response is correlated with the original request before the editor interprets it.

For a failed edit, inspect conversion and preparation before filesystem application. `src/lsp/edit.rs` validates URIs, UTF-16 boundaries, and overlap. `src/lsp/workspace_edit.rs` validates workspace confinement, revisions, versions, resource-operation support, total size, protected paths, and rollback. Failure is intentionally closed; retrying with a looser path is not a supported recovery technique.

Tests that launch a real configured language server are opt-in through `RED_RUN_REAL_LSP_TESTS`. Default integration coverage uses mock servers so protocol ordering and failure paths remain deterministic.

## Husk plugins

Run `red --self-check` to parse themes, resolve bundled assets, seed production-equivalent snapshots, and activate every bundled plugin without entering the terminal. A single invalid plugin is quarantined and reported with its stage; it should not prevent unrelated plugins from activating.

Search the log for `[PLUGIN:HUSK]` and the plugin name. `src/plugin/registry.rs` owns status, dependency ordering, quarantine, command routing, and hot reload. `src/plugin/runtime.rs` owns the Husk VM, host translation, callback instruction budget, snapshots, and staged reload effects. A reload error leaves the previous plugin active, so inspect `PluginStatus` rather than assuming the edited source is live.

For missing UI, identify the stable resource ID and owning manager: panels, workspaces, overlays, gutter signs, decorations, and window bars replace namespaced or ID-addressed state. A plugin refresh using a different ID creates a second lifecycle instead of updating the first.

For a process failure, inspect the requesting plugin's configured allowlist and direct argument vector. The process API does not invoke a shell, and unapproved environment variables are removed. Process events are polled by the editor; child exit alone does not update plugin UI until its event handler runs.

## Codex and reviewable proposals

Run `red --agent-check` for the installed executable and minimum-version report, or `red --agent-check --strict` when readiness should control an automated check. Authentication is verified when the app-server starts, not by the offline prerequisite report.

`src/codex/mod.rs` owns bounded JSONL transport, sessions, turns, cancellation, and dynamic-tool dispatch. `src/agent_tools.rs` validates editor tool shapes and UTF-16 edits. `src/agent_workspace.rs` owns session-scoped proposed contents and physical workspace confinement. Reads in one session see its staged proposal, but visible buffers and disk remain unchanged until the editor accepts a staged result through its transaction boundary.

When review reports a conflict, compare the proposal base revision and contents with the current visible buffer. Do not bypass the conflict by writing the proposed text directly: that would discard user work and lose agent attribution. Recovered proposals are archived and do not imply that a prior Codex thread or process is still live.

Structured `agent_proposal_notification_failed` records mean the attributed buffer change committed but a later notification, workspace sync, plugin event, or render failed. The editor reports that partial operational failure instead of rolling back an already accepted user decision.

## Recovery snapshots

Use `red --resume` only after the prior editor owner has stopped. Interactive owners do not currently hold an exclusive recovery lock.

`src/session.rs` owns schema validation, generation fallback, bounded and no-follow reads, atomic writes, and disk-divergence evidence. `Editor::persist_session_snapshot` freezes cheap Rope snapshots and delegates disk reads and writes to a worker thread. Search for structured `session_snapshot_failed` records and inspect the `session:snapshot` performance sample when persistence stalls.

Recovered dirty contents remain in memory and are never written to their backing files until an explicit save. A divergence report means Red could not prove that the snapshot's disk base still names the same unchanged regular file; keeping the recovered buffer dirty is the safe result.

## Detach and attach

`red --detach=name` starts the terminal-independent owner, `Ctrl-\` disconnects the current client, `red --attach name` reconnects, and `red --stop name` requests owner shutdown. A dropped SSH or terminal connection should leave the owner, buffers, LSP, plugins, timers, watchers, snapshots, and Codex process alive.

`src/headless/mod.rs` owns the versioned local protocol, authentication token, one-client lease, frame and terminal-size limits, heartbeat, timeouts, and row deltas. `DetachedEditorCore` in `src/editor.rs` owns the real editor and services background work while no client is attached.

With `RED_PERF=summary`, `detach:idle_tick` should rise while idle without matching serialization work. `detach:rendered_tick`, `detach:serialize_frame`, and `detach:changed_rows` should track actual visible updates. A full-frame delta is expected on connect or resize but suspicious during ordinary single-key input.

## Choosing the first code entry point

| Symptom | First owner |
| --- | --- |
| Configuration value ignored or replaced | `config::LoadedConfig` diagnostics |
| Wrong plugin or theme source | `assets::list_runtime_assets` |
| Key produces the wrong action | `Editor::process_editor_event` and configured `KeyAction` |
| Edit cannot undo as one unit | editor transaction start/commit and `UndoHistory` |
| Unicode cursor or selection drift | `unicode_utils`, display layout, then the boundary conversion |
| Stale syntax or frame output | buffer revision, render generation, layout/highlight cache key |
| LSP request never completes | `RealLspClient` pending request map and inbound correlation |
| Workspace edit rejected | LSP edit preparation and resource-operation validation |
| Plugin missing after reload | `PluginRegistry::statuses` and quarantine diagnostic |
| Plugin process has no output | process permission, process ID, and polled process events |
| Agent changed disk before review | proposal workspace boundary; treat as a safety defect |
| Snapshot not advancing | session worker result, generation tuple, and structured failure log |
| Attach paints stale content | headless revision handshake and detached-core render generation |
