# Codex Integration Plan

This plan covers the first vertical slice for integrating Codex through
`codex app-server`. It intentionally proves the Plugin Window architecture and a
single Codex conversation loop before adding broader workflow polish.

## Goal

Open a right-side Codex Chat Window inside Red's normal split layout, attach
explicit editor context to a Composer draft, submit a turn to Codex App Server,
stream the assistant response into the Transcript, and restore the project-owned
Codex Thread after restarting Red.

## Decisions

- Codex UI is a Plugin Window, not a Plugin Panel or hidden text buffer.
- The bundled Codex plugin is TypeScript.
- Red owns Codex App Server lifecycle and protocol translation.
- Plugins use a narrow `red.codex` API, not raw app-server messages.
- Context References snapshot text at attach time.
- Large references render as Context Placeholders while full text is submitted
  through app-server `additionalContext`.
- A Codex Chat Window is pinned to one Workspace Root.

## Phase 1: Plugin Window Foundation

- Add a distinct Plugin Window leaf kind to the split tree.
- Preserve normal split behavior: focus, resize, close, navigation, and session
  layout placeholders.
- Keep Editor Windows buffer-backed and Plugin Windows plugin-backed.
- Add unavailable-plugin fallback behavior for restored Plugin Windows whose
  owner is missing.

Acceptance checks:

- A Plugin Window can open as a right vertical split beside the active Editor
  Window.
- Existing editor split tests still pass.
- Session restore can round-trip a Plugin Window placeholder without creating a
  synthetic buffer.

## Phase 2: Chat Render Model

- Add generic Plugin Window container state.
- Support first content kind: `chat`.
- Define semantic render data for:
  - title and status
  - Transcript blocks
  - Composer text, cursor, and selection
  - Context Placeholder spans
  - scroll state
  - key hints/actions
- Keep Rust responsible for terminal drawing, wrapping, clipping, focus borders,
  and theme mapping.

Acceptance checks:

- A plugin can update chat render state and trigger redraw.
- Transcript and Composer regions render separately.
- Long text wraps without corrupting adjacent editor windows.

## Phase 3: Input Routing

- Route key events to the focused Plugin Window.
- Let the plugin own local Plugin Input Mode.
- Implement Codex Chat Window modes:
  - transcript/navigation mode
  - composer editing mode
- Support transcript navigation with `j`/`k`, arrows, `Ctrl-f`, `Ctrl-b`,
  PageUp, and PageDown.
- Support Multiline Composer editing.
- Enter submits.
- Shift+Enter, Alt+Enter, and `Ctrl-j` insert newlines when detectable;
  `Ctrl-j` is the guaranteed fallback.
- Escape leaves Composer editing.
- `Ctrl-c` cancels an Active Codex Turn.

Acceptance checks:

- Focus can move between Editor Windows and the Codex Chat Window.
- Composer editing does not mutate editor buffers.
- Window navigation still works from transcript/navigation mode.

## Phase 4: Codex Host API

- Add Rust-owned Codex App Server client.
- Start and own local app-server by default using Unix socket transport.
- Allow explicit configured remote endpoint later.
- Translate app-server notifications into stable `red.codex` events.
- Implement the minimum host API:
  - open/start local app-server
  - start thread
  - resume thread by thread ID
  - send turn
  - stream assistant deltas
  - cancel active turn
  - report disconnected/error state
- On disconnect, attempt one automatic restart for owned local servers; do not
  queue submissions while disconnected.

Acceptance checks:

- A Composer submit creates or resumes a Codex Thread and starts a turn.
- Assistant output streams into the Transcript.
- Cancel stops an in-flight turn.
- App-server crash shows disconnected state and one restart attempt.

## Phase 5: Bundled TypeScript Codex Plugin

- Add a bundled TypeScript plugin for Codex.
- Register richer Plugin Commands:
  - `codex.open`
  - `codex.cancel`
  - `codex.attachCurrentLine`
  - `codex.attachCurrentFile`
  - `codex.attachSelection`
  - `codex.resume`
  - `codex.toggleFollowChanges`
- Open/focus a Codex Chat Window with `codex.open`.
- Attach context commands open/focus the window when needed.
- Store Composer draft, Context References, and active thread metadata in plugin
  state.
- Persist project-owned Codex Thread ID in plugin storage keyed by Workspace
  Root.

Acceptance checks:

- `codex.open` opens or focuses the right-side Codex Chat Window.
- `codex.attachCurrentLine` adds a visible Context Placeholder to the Composer.
- `codex.attachSelection` snapshots selected text and adds a placeholder.
- Restarting Red restores the specific plugin-owned Codex Thread for the
  Workspace Root when the plugin state says it should.

## Phase 6: Workspace Root And Context

- Resolve Workspace Root as Git root, falling back to Red's current directory.
- Pin each Codex Chat Window to its Workspace Root.
- Use Workspace Root as app-server `cwd`.
- Use Workspace Root as initial `runtimeWorkspaceRoots`.
- Warn before attaching context from a different Workspace Root.
- Submit Context References through app-server `additionalContext`, with
  placeholder spans in visible Composer text.

Acceptance checks:

- Thread list/resume calls use the expected `cwd`.
- Context from another root is not silently attached.
- Large context is displayed compactly but submitted in full.

## Deferred Work

- `codex.resume` picker for previous Codex Threads.
- Follow Changes toggle and changed-hunk preview behavior.
- Inline approval and user-input request blocks.
- Diagnostics, current file, open buffers, and git diff context commands.
- Dirty-buffer conflict UI for Codex-written files.
- Remote WebSocket endpoint support.
- Multiple simultaneous Codex Chat Windows for multiple Workspace Roots.
