---
title: "Agent-Attributed Edits"
summary: "Agent-attributed edits are Red's model for keeping Codex mutations inside editor-owned transactions, revision checks, visible following, and undo history instead of allowing native Codex workspace writes."
topics: [concepts, agent, agent-edits, safety]
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

Agent-attributed edits are Red's current safety model for Codex-assisted changes. Codex does not receive a shell or native patch tool in the full Agent workflow; it runs in a read-only sandbox with native approvals denied and must use Red's dynamic tools to read, navigate, edit, and save [@workflow] [@codex]. Red then turns accepted tool output into ordinary editor transactions with `EditOrigin::Agent`, so undo history and agent history can identify the session and turn that changed the buffer [@editor].

## Why Attribution Is The Boundary

The important boundary is editor ownership. Full-agent `write_file` and `apply_edits` calls require the visible revision returned by `read_file`, execute on the editor owner task, and save through Red after the transaction is committed [@workflow] [@tools] [@editor]. That keeps unsaved buffer contents authoritative for Codex reads while still requiring every mutation to pass through the same transaction, notification, rendering, and persistence paths as other production edits [@editor].

Red makes the work visible before it happens. For file tools, the editor opens the target path, moves the cursor to the relevant UTF-16 range when available, renders the file, and waits before executing the operation [@workflow] [@editor]. The resulting edit is immediately represented in the editor's undo and history machinery.

## Full Agent Versus Inline Assist

The full Agent workflow starts from `Space A` or `:Agent` and uses the nine-tool Codex surface documented in the workflow [@workflow]. Successful full-agent edits are revision-checked, applied as agent-origin transactions, and saved to disk through Red [@workflow] [@editor].

Inline assist starts from `Space i` and has a smaller contract. Codex receives one immutable target range plus bounded surrounding context and can only call `submit_replacement`; Red then verifies that the active buffer, window, revision, and original target text still match before applying the replacement [@workflow] [@codex] [@editor]. Inline replacements are agent-attributed but deliberately unsaved, giving the user local keep, undo, refine, and promote controls [@workflow] [@editor].

## What To Read Next

Use [Followed editing](../architecture/agent/followed-editing) for the full-agent mutation path, [Dynamic tools and editor tools](../architecture/agent/dynamic-tools-and-editor-tools) for the strict tool schemas, and [Inspect agent history](../guides/agent/inspect-agent-history) when you need to review or revert agent-origin transactions.
