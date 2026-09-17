---
title: "Inspect Agent History"
summary: "Use `:AgentHistory` to review and safely revert agent-attributed edit transactions after Codex changes have entered Red's undo history."
topics: [guides, agent, agent-edits, history, operations, safety]
sources:
  - id: agent-plugin
    type: file
    path: plugins/agent.hk
  - id: editor
    type: file
    path: src/editor.rs
  - id: undo
    type: file
    path: src/undo.rs
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
---

# Inspect Agent History

Use this guide after inline assist or the full Agent workflow has changed a buffer and you need to inspect what Codex did. Full-agent edits are followed, revision-checked, applied as agent-origin transactions, and saved through Red, while inline replacements are agent-origin transactions that remain unsaved until the user saves [@workflow] [@editor]. `:AgentHistory` is the operational view for those transactions.

## Open The History Workspace

Run:

```text
:AgentHistory
```

The bundled agent plugin registers `AgentHistory` with the alias `transactions`, opens a workspace titled `Attributed edit history`, requests `EditHistory`, and renders up to 500 transaction rows [@agent-plugin]. Rows whose origin kind is `agent` show the originating session and turn id on the right side of the workspace [@agent-plugin].

## Inspect A Transaction

Move through the rows with the workspace navigation keys. When the selected row changes, the plugin fills the detail pane with the old and new text for each recorded edit in that transaction [@agent-plugin]. Treat this view as a transaction-level audit trail for what entered editor history.

## Revert Safely

Press `r` on a history row to request `RevertTransaction` for that transaction id [@agent-plugin]. The command is intentionally routed through Red rather than through plugin-side buffer mutation, so reversion remains part of the editor-owned transaction and undo model [@editor]. After the request, the plugin refreshes the history workspace [@agent-plugin].

Selective revert is checked rather than blind replay. `UndoHistory::prepare_revert` only prepares inverse edits when the target transaction is on the current undo branch, no later edit overlaps the transaction's post-image, and the current buffer still matches the text that transaction wrote [@undo]. If one of those checks fails, the editor reports `revert conflict: ...` instead of changing the buffer [@editor].

If a user reports that an agent edit cannot be reverted from history, inspect the editor transaction path and undo history first. Use [Undo tree](../../concepts/editor/undo-tree) for the branch and selective-revert model, then confirm whether later edits changed the same range or whether the user is trying to revert a transaction from another undo branch [@undo].

For the surrounding model, read [Agent-attributed edits](../../concepts/agent-attributed-edits), [Agent Architecture](../../architecture/agent), and [Followed Editing](../../architecture/agent/followed-editing).
