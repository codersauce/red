---
title: "Dynamic Tools And Editor Tools"
summary: "Red exposes a strict Codex dynamic-tool surface that separates workspace search from editor-owned reads, navigation, UTF-16 edits, saves, and safe actions."
topics: [architecture, agent, codex, editor, unicode, agent-edits]
sources:
  - id: codex
    type: file
    path: src/codex/mod.rs
  - id: tools
    type: file
    path: src/agent_tools.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
---

Dynamic tools and editor tools are Red's app-server capability layer for Codex. The Codex worker publishes four workspace dynamic tools directly, extends them with eight strict editor-tool schemas, and routes editor-aware reads, writes, annotations, selections, and safe actions through the editor owner task [@codex] [@tools]. This layer is where the [Codex App-Server Workflow](codex-app-server-workflow) becomes editor-aware: Codex can list, search, read, open, select, annotate, run allow-listed editor actions, and request edits, but each operation is bounded, schema-checked, session-scoped, and mediated by Red [@workflow] [@editor].

## Tool Surface

The app-server worker publishes `list_files`, `search_files`, `read_file`, and `write_file` itself, then appends editor schemas for `get_editor_state`, `open_file`, `select_text`, `apply_edits`, `run_editor_action`, `create_directory`, `add_annotations`, and `dismiss_annotations` [@codex] [@tools]. `list_files` walks without following links, respects ignore and sensitive-path policy, sorts results, and returns pages plus truncation metadata within Red's entry and time bounds [@codex]. On Unix, `search_files` reads through descriptor-relative, no-follow, nonblocking filesystem operations and reports bounded-scan truncation; platforms without that safe read boundary must use `read_file` [@codex] [@workflow].

`read_file` and `write_file` are editor-tool-host operations. `read_file` opens the safe workspace file through Red when needed and returns a bounded editor-visible page, current revision, existence, line range, and continuation metadata [@editor]. `write_file` requires that revision, replaces the complete buffer through an agent-origin editor transaction, and saves through Red [@tools] [@editor].

The workflow documentation describes the same twelve-tool contract and its expected behavior, including bounded file listing, bounded search, Red-mediated reads, revision-checked writes, directory creation, editor state snapshots, file opening, UTF-16 selections, revision-checked edits, source annotations, and allow-listed navigation or LSP actions [@workflow].

## Strict Editor Schemas

Editor tools use strongly parsed Rust enums and JSON schemas with `additionalProperties: false` [@tools]. `EditorToolCall::parse` rejects non-object arguments, rejects attempts to override the tool name, and deserializes with `deny_unknown_fields`, so unknown fields and unregistered action names fail before they reach editor logic [@tools]. Tests assert that schemas are strict, `apply_edits` is capped at 128 edits, and actions such as `quit` are rejected [@tools].

The editor-tool set is intentionally narrow. `get_editor_state` returns a bounded active-editor snapshot, including the current source annotation; `open_file` opens a workspace file at a UTF-16 line and character in the current, horizontal, or vertical target; `select_text` opens a workspace file and creates a character, line, or block selection; `apply_edits` applies up to 128 atomic revision-checked UTF-16 replacements and saves the file; `add_annotations` adds up to 16 revision-checked line-anchored cards without changing source; `dismiss_annotations` hides cards by stable ID; and `run_editor_action` maps only to safe navigation or LSP actions such as go to definition, hover, diagnostics refresh, signature help, jump history, buffer switching, and annotation traversal [@tools] [@editor]. It cannot invoke arbitrary commands, shell, quit, or unrelated text mutations [@tools].

`add_annotations` returns a canonical `red://annotation/<uuid>` destination for
each card. Markdown rendered in an Agent transcript recognizes that destination
as a typed internal link rather than a file or URL. Activating it resolves only
a visible Agent-owned annotation, switches to the owning buffer, selects the
tracked source anchor, and opens the shared inline-comment card. Missing or
dismissed IDs produce a quiet unavailable message without any fallback to
filesystem or external-link behavior [@editor].

## UTF-16 Editor Boundary

Codex editor tools use zero-based UTF-16 positions because they share coordinates with LSP-facing editor surfaces [@tools]. `EditorPosition` stores a line and UTF-16 code-unit character, and `EditorTextEdit` stores half-open UTF-16 replacements with UTF-8 replacement text [@tools]. The conversion helper rejects out-of-bounds lines, out-of-bounds characters, and positions that split a UTF-16 surrogate pair [@tools].

The editor applies that boundary consistently. `open_file` passes `LocationColumnEncoding::Utf16` into `OpenLocation`; `select_text` converts UTF-16 start and end positions to byte offsets for validation and then to grapheme positions for the visible selection; `apply_edits` checks the expected visible revision, applies the UTF-16 edit list to current contents, and routes the replacement through the agent edit transaction path [@editor]. This is the agent-specific instance of Red's broader [Editor Coordinate Systems](../../concepts/editor/coordinate-systems) and [Text Mutation Boundary](../editor/text-mutation-boundary): external positions are converted at named boundaries before they can affect editor state.

## Followed Mutation And Activity

Mutating editor tools are serialized before they apply. When `agent.follow_tool_calls` is enabled, `prepare_agent_follow_step` resolves the target, opens the file, moves the cursor to the first edit range when available, renders, and delays execution so the user can see it. The default skips those deliberate playback pauses [@editor]. `apply_agent_contents` still checks the expected revision, starts an `EditOrigin::Agent` transaction, replaces the buffer, commits, notifies change consumers, renders, and saves [@editor].

The editor dispatcher checks more than schema validity before executing a tool. It requires an active session and workspace root, resolves paths through the workspace, rejects ignored paths, requires explicit consent for secret-like filenames, and rejects stale revisions before text changes apply [@editor]. Editor-tool requests travel over a bounded channel and time out if the dispatcher is backpressured or stops [@tools].

Tool calls also shape user-facing state. `EditorToolCall::activity_title` defines concise labels for operations such as opening a file, writing a file, or editing a path, while the editor event bridge forwards app-server activity updates as `agent:activity` [@tools] [@editor]. Use [Followed Editing](followed-editing) for the write-and-save path built on top of this tool layer.
