---
title: "Editor Event Loop"
summary: "The editor event loop is the single-owner runtime loop that serializes terminal input, plugin requests, LSP responses, agent events, rendering, session snapshots, and shutdown."
topics: [architecture, editor, plugins, lsp, agent, sessions]
sources:
  - id: editor
    type: file
    path: src/editor.rs
  - id: agent-manager
    type: file
    path: src/editor/agent_manager.rs
  - id: lsp-coordinator
    type: file
    path: src/editor/lsp_coordinator.rs
  - id: session-manager
    type: file
    path: src/editor/session_manager.rs
---

The editor event loop is Red's ownership boundary for interactive state. `Editor` owns buffers, windows, rendering caches, LSP state, plugin resources, session recovery, and Codex agent integration on one async task; other subsystems communicate by messages and are serviced between input batches rather than mutating editor state directly [@editor]. This shape is why text changes pass through the [text mutation boundary](text-mutation-boundary), why plugin work is drained through [plugin host requests](plugin-host-requests), and why the loop can keep detachable editor cores alive without moving state into a client.

## Single Owner

`Editor` is documented as the single-task owner of Red's application state, and its fields group the major runtime domains: `BufferManager`, `WindowManager`, `SessionManager`, `LspCoordinator`, `AgentManager`, plugin registry, render buffers, diagnostics, panels, overlays, and dialogs [@editor]. The sub-managers keep domain state local without changing ownership. `AgentManager` stores the Codex bridge, task handle, workspace root, tool request receiver, active sessions, turn timing, commit-message requests, and conversation snapshot [@agent-manager]. `LspCoordinator` tracks which document URIs have been opened and which buffer revisions have already been delivered to language servers [@lsp-coordinator]. `SessionManager` tracks the active session store, snapshot interval, in-flight writer, persisted generation, and warning state [@session-manager].

That ownership rule is practical. The editor loop can safely call into LSP, plugin, session, and agent subsystems because the final state mutation happens after a message returns to `Editor`. The same rule protects buffer edits: a plugin or background tool may request a change, but the editor applies it through the canonical transaction path described in [text mutation boundary](text-mutation-boundary).

## Startup And Input

Interactive startup enables raw terminal mode, mouse capture, focus events, bracketed paste, the alternate screen, and keyboard enhancement flags before plugin startup and rendering [@editor]. It initializes the Husk runtime, refreshes plugin snapshots, registers configured plugins, runs plugin initialization, emits `editor:ready`, restores any stored agent transcript, opens the current buffer for LSP, reconciles terminal size, and renders the first frame [@editor].

The main loop waits up to 10 ms for terminal input when no events are queued, then drains ready terminal events through `process_editor_event` [@editor]. That bounded wait is the heartbeat for non-terminal work: LSP responses, plugin timers, plugin process events, filesystem watches, Codex events, dialogs, panel animations, session snapshots, and plugin requests all receive service even while no key is pressed [@editor].

## Background Service Tick

`service_background` is the loop's multiplexing point. It follows and executes pending agent editor tool requests, then sends each result back on the tool response channel [@editor]. It then polls plugin timer callbacks, plugin process events, directory watcher changes, and plugin hot reloads before draining Codex bridge events into plugin notifications such as `agent:update`, `agent:completed`, and `agent:activity` [@editor]. The method also clears finished Codex tasks and emits `agent:session_lost` when the bridge has stopped and no pending events remain [@editor].

The LSP pump runs inside the same tick. `recv_response` is always called because it completes initialization and flushes queued document notifications, and any resulting `Action` is executed through the editor action executor [@editor]. Plugin host requests are then drained from `ACTION_DISPATCHER` up to `PLUGIN_REQUESTS_PER_TICK`, so plugin effects share the same serialized state path as user input and LSP actions [@editor]. The [plugin host requests](plugin-host-requests) page describes that request boundary in more detail.

## Rendering And Persistence

The loop coalesces most background work into a single render at the end of a tick. `service_background` sets `needs_render` or `needs_motion_render` as it handles LSP messages, plugin requests, panel changes, overlays, filesystem operations, and agent tool activity; it then calls either full render or a motion render once for the tick [@editor]. That keeps rendering downstream of state changes rather than letting every subsystem flush the terminal independently. The stages of frame construction are covered in [rendering pipeline](rendering-pipeline).

Session persistence is also loop-owned. During each loop iteration the editor asks whether a session snapshot should be persisted, and a changed warning state can trigger a render [@editor]. `SessionManager` determines whether a snapshot is due, tracks generation backoff, stores an in-flight writer, and records successful generations [@session-manager]. Detached editor cores use the same editor state and call `service_background` from their `tick`, `input`, `resize`, and `focus` methods, so timers, plugin processes, LSP messages, and agent events continue while no terminal client owns the UI [@editor].

## Shutdown

Shutdown is serialized through `shutdown_services`. The editor drops the agent bridge, waits for the Codex task if present, runs plugin `before_exit` with an editor-state snapshot, flushes pending plugin storage requests while dropping other plugin requests, persists a forced session snapshot, and deactivates plugins [@editor]. This final drain preserves the same ownership model as the live loop: storage writes may still be accepted, but late requests that would mutate editor state are logged and discarded because the editor is leaving its active runtime phase [@editor].
