---
title: "Followed Editing"
summary: "Followed editing is Red's full-agent mutation path: Codex can only change files through Red-owned dynamic tools that check revisions, apply an attributed editor transaction, and save through the editor."
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

Followed editing is Red's current safety boundary for full Codex agent writes. The Codex process runs with a read-only sandbox and denied native approvals, but Red exposes dynamic tools whose mutating calls are executed by the editor owner task, not by Codex writing the workspace directly [@workflow] [@codex]. Mutating tools apply as `EditOrigin::Agent` transactions and save through the editor. Tool calls are serialized but run without deliberate delays by default; users can enable target-revealing playback with `agent.follow_tool_calls` [@workflow] [@editor].

## Tool Entry Points

The full agent receives `list_files`, `search_files`, `read_file`, `write_file`, `create_directory`, `get_editor_state`, `open_file`, `select_text`, `apply_edits`, `add_annotations`, `dismiss_annotations`, and `run_editor_action` [@workflow]. `list_files` and `read_file` are paged and report continuation or truncation metadata. `read_file` returns current editor-visible contents and revision, including unsaved buffer contents, and `write_file`, `apply_edits`, and `add_annotations` require that revision [@codex] [@tools].

The tool host is a bounded channel from the Codex worker into the editor loop. `EditorToolHost` packages each read, write, navigation, selection, or editor action as an `EditorToolRequest`, waits for the editor owner to answer, and times out if the dispatcher stalls [@tools]. The Codex worker rejects tool calls for unknown sessions, inactive turns, cancelled turns, commit-message sessions, and oversized arguments before it forwards a request [@codex].

## Serialized And Optional Follow Playback

The editor serializes tool execution through `service_background`. By default, it dispatches the next tool immediately. With `agent.follow_tool_calls = true`, it first resolves and opens the target when relevant, moves the cursor to the first affected range for `apply_edits`, renders, and uses the configured dwell period before dispatch [@workflow] [@editor].

Path resolution stays fail-closed. Agent tool paths must be non-empty, remain under the active workspace root after lexical normalization, avoid symlink components, and avoid ignored workspace paths. Secret-like filenames require the explicit `agent.allow_sensitive_paths` grant [@editor]. `list_files` and `search_files` apply the same policy, avoid symlink-following workspace walks, and use bounded safe reads for content search on Unix [@codex] [@workflow].

## Mutation And Saving

`write_file` replaces a complete file, while `apply_edits` computes a complete replacement by applying up to 128 non-overlapping UTF-16 edits to the current buffer contents [@tools] [@editor]. Both paths call `apply_agent_contents`, which opens or creates the target buffer, checks the expected revision against the current buffer revision, rejects NUL bytes, starts an agent-origin transaction with the active session and turn id, replaces the buffer contents, commits the transaction, notifies change consumers, renders, and saves through `save_current_agent_buffer` [@editor].

The save step is part of the full-agent contract. On Unix, Red writes through the secure workspace writer, marks the buffer saved on success, and emits `file:saved`; on other platforms it uses the buffer save path [@editor]. The tool result reports whether the edit was applied, whether the save succeeded, the new revision, and any persistence or notification error [@editor].

## Inline Assist Boundary

Inline assist is intentionally narrower. `Space i` starts an ephemeral Codex thread with bounded read-only project tools and one result-submission call; Codex cannot choose a mutation target or call the full editor-tool surface [@workflow] [@codex]. Red verifies the active buffer, window, revision, and original target text before applying a replacement as an agent-origin transaction. Exact-scope foreground edits auto-apply by default, while background, stale, and expanded-scope results wait for review. Inline edits remain unsaved and undoable [@workflow] [@editor].

Use [Agent-attributed edits](../../concepts/agent-attributed-edits) for the user-facing edit model and [Inspect agent history](../../guides/agent/inspect-agent-history) for the operational path after full-agent or inline edits have entered undo history.
