---
title: "Dynamic Tools And Editor Tools"
summary: "Red exposes a strict Codex dynamic-tool surface that separates workspace search and proposal writes from editor-owned navigation and UTF-16 edit staging."
topics: [architecture, agent, codex, editor, unicode, reviewable-edits]
sources:
  - id: codex
    type: file
    path: src/codex/mod.rs
  - id: tools
    type: file
    path: src/agent_tools.rs
  - id: workspace
    type: file
    path: src/agent_workspace.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
---

Dynamic tools and editor tools are Red's app-server capability layer for Codex. The Codex worker publishes four workspace dynamic tools directly, extends them with five strict editor-tool schemas, and routes all mutating operations into the [Proposal Workspace](proposal-workspace) rather than into the visible buffer or filesystem [@codex] [@tools]. This layer is where the [Codex App-Server Workflow](codex-app-server-workflow) becomes editor-aware: Codex can list, search, read, open, select, run allow-listed editor actions, and stage edits, but each operation is bounded, schema-checked, session-scoped, and mediated by Red [@workflow] [@editor].

## Tool Surface

The app-server worker publishes `list_files`, `search_files`, `read_file`, and `write_file` itself, then appends editor schemas for `get_editor_state`, `open_file`, `select_text`, `apply_edits`, and `run_editor_action` [@codex] [@tools]. `list_files` walks the workspace without following links, respects ignore files, sorts results, and stops at Red's file, entry, and time bounds [@codex]. On Unix, `search_files` reads through descriptor-relative, no-follow, nonblocking filesystem operations; the workflow states that content search is unavailable on platforms without that safe read boundary, so Codex must use `read_file` instead [@codex] [@workflow].

`read_file` and `write_file` are proposal-workspace operations. `read_file` asks `ProposalToolHost` to return the session's authoritative contents, which are staged proposal contents if present, otherwise visible editor contents or a safely read disk base [@workspace]. `write_file` replaces only the session's in-memory proposed contents and returns an empty success object, so a complete-file rewrite is still a proposal rather than a save [@workspace].

The workflow documentation describes the same nine-tool contract and its expected behavior, including bounded file listing, bounded search, Red-mediated reads, proposal writes, editor state snapshots, file opening, UTF-16 selections, revision-checked edits, and allow-listed navigation or LSP actions [@workflow].

## Strict Editor Schemas

Editor tools use strongly parsed Rust enums and JSON schemas with `additionalProperties: false` [@tools]. `EditorToolCall::parse` rejects non-object arguments, rejects attempts to override the tool name, and deserializes with `deny_unknown_fields`, so unknown fields and unregistered action names fail before they reach editor logic [@tools]. Tests assert that schemas are strict, `apply_edits` is capped at 128 edits, and actions such as `quit` are rejected [@tools].

The editor-tool set is intentionally narrow. `get_editor_state` returns a bounded active-editor snapshot; `open_file` opens a workspace file at a UTF-16 line and character in the current, horizontal, or vertical target; `select_text` opens a workspace file and creates a character, line, or block selection; `apply_edits` stages text replacements as a proposal; and `run_editor_action` maps only to safe navigation or LSP actions such as go to definition, hover, diagnostics refresh, signature help, jump history, and buffer switching [@tools] [@editor]. It cannot invoke arbitrary commands, shell, save, quit, or live text mutations [@tools].

## UTF-16 Editor Boundary

Codex editor tools use zero-based UTF-16 positions because they share coordinates with LSP-facing editor surfaces [@tools]. `EditorPosition` stores a line and UTF-16 code-unit character, and `EditorTextEdit` stores half-open UTF-16 replacements with UTF-8 replacement text [@tools]. The conversion helper rejects out-of-bounds lines, out-of-bounds characters, and positions that split a UTF-16 surrogate pair [@tools].

The editor applies that boundary consistently. `open_file` passes `LocationColumnEncoding::Utf16` into `OpenLocation`; `select_text` converts UTF-16 start and end positions to byte offsets for validation and then to grapheme positions for the visible selection; `apply_edits` calls the proposal workspace with the expected visible revision and UTF-16 edit list [@editor]. This is the agent-specific instance of Red's broader [Editor Coordinate Systems](../../concepts/editor/coordinate-systems) and [Text Mutation Boundary](../editor/text-mutation-boundary): external positions are converted at named boundaries before they can affect editor state.

## Proposal Staging And Activity

Mutating editor tools stage proposal state. `ProposalWorkspace::apply_editor_edits` normalizes the path, checks that both the proposal base revision and the current visible revision match the tool's `expected_revision`, applies the bounded UTF-16 edits to the proposed text, writes the new proposed contents, and returns review hunks against current contents [@workspace]. Complete-file writes use the same proposal state through `write_file`, with a content-size bound and no visible-buffer or disk mutation [@workspace].

The editor dispatcher checks more than schema validity before executing a tool. It requires an active session, requires an active proposal workspace, syncs visible buffers, resolves relative or absolute tool paths through the workspace, rejects sensitive filenames, and rejects ignored workspace paths [@editor]. Editor-tool requests travel over a bounded channel and time out if the dispatcher is backpressured or stops [@workspace].

Tool calls also shape user-facing state. `EditorToolCall::activity_title` defines concise labels for operations such as opening a file or proposing edits, while the editor event bridge forwards app-server activity updates as `agent:activity` and separately publishes proposal changes as `agent:proposals_changed` [@tools] [@editor]. This keeps review UI updates separate from assistant transcript text.
