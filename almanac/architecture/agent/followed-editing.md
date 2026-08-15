---
title: "Followed Editing"
summary: "Followed editing is Red's full-agent mutation path: Codex can only change files through Red-owned dynamic tools that reveal the target, check revisions, apply an attributed editor transaction, and save through the editor."
topics: [architecture, agent, codex, agent-edits, safety]
sources:
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: codex
    type: file
    path: src/codex/mod.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: tools
    type: file
    path: src/agent_tools.rs
---

Followed editing is Red's current safety boundary for full Codex agent writes. The Codex process runs with a read-only sandbox and denied native approvals, but Red exposes dynamic tools whose mutating calls are executed by the editor owner task, not by Codex writing the workspace directly [@workflow] [@codex]. Before a mutating tool takes effect, Red opens the file, reveals the affected location, waits briefly, then applies the change as an `EditOrigin::Agent` transaction and saves through the editor [@workflow] [@editor].

## Tool Entry Points

The full agent receives `list_files`, `search_files`, `read_file`, `write_file`, `get_editor_state`, `open_file`, `select_text`, `apply_edits`, and `run_editor_action` [@workflow]. `read_file` returns the current editor-visible contents and revision for a workspace file, including unsaved buffer contents, and both `write_file` and `apply_edits` require that revision before mutating text [@codex] [@tools].

The tool host is a bounded channel from the Codex worker into the editor loop. `EditorToolHost` packages each read, write, navigation, selection, or editor action as an `EditorToolRequest`, waits for the editor owner to answer, and times out if the dispatcher stalls [@tools]. The Codex worker rejects tool calls for unknown sessions, inactive turns, cancelled turns, commit-message sessions, oversized arguments, and turns that exceed the tool-call limit before it forwards a request [@codex].

## Follow Before Apply

The editor serializes tool playback through `service_background`. It first prepares the follow step by resolving the workspace path, opening the target file when relevant, moving the cursor to the first affected range for `apply_edits`, rendering the view, and delaying edit tools for the configured dwell period [@editor]. Only after that delay does the editor dispatch the tool request, which keeps the user-facing buffer in sync with the file Codex is about to read or modify [@workflow] [@editor].

Path resolution stays fail-closed. Agent tool paths must be non-empty, remain under the active workspace root after lexical normalization, avoid symlink components, avoid sensitive filenames, and avoid ignored workspace paths [@editor]. `list_files` and `search_files` also avoid symlink-following workspace walks and use bounded safe reads for content search on Unix [@codex] [@workflow].

## Mutation And Saving

`write_file` replaces a complete file, while `apply_edits` computes a complete replacement by applying up to 128 non-overlapping UTF-16 edits to the current buffer contents [@tools] [@editor]. Both paths call `apply_agent_contents`, which opens or creates the target buffer, checks the expected revision against the current buffer revision, rejects NUL bytes, starts an agent-origin transaction with the active session and turn id, replaces the buffer contents, commits the transaction, notifies change consumers, renders, and saves through `save_current_agent_buffer` [@editor].

The save step is part of the full-agent contract. On Unix, Red writes through the secure workspace writer, marks the buffer saved on success, and emits `file:saved`; on other platforms it uses the buffer save path [@editor]. The tool result reports whether the edit was applied, whether the save succeeded, the new revision, and any persistence or notification error [@editor].

## Inline Assist Boundary

Inline assist is intentionally narrower. `Space i` starts an ephemeral Codex thread with only `submit_replacement`; Codex cannot choose a file, read additional files, or call the full editor-tool surface [@workflow] [@codex]. Red verifies the active buffer, window, revision, and original target text before applying the replacement as an agent-origin transaction, and the workflow explicitly leaves the inline result unsaved so the user can keep, undo, refine, or promote it to the full Agent workflow [@workflow] [@editor].

Use [Agent-attributed edits](../../concepts/agent-attributed-edits) for the user-facing edit model and [Inspect agent history](../../guides/agent/inspect-agent-history) for the operational path after full-agent or inline edits have entered undo history.
