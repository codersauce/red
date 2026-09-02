---
title: "LSP Architecture"
summary: "LSP architecture routes readers through client lifecycle, transport, editor document sync, completion, workspace edits, capabilities, configuration, Husk LSP integration, and debugging."
topics: [architecture, lsp]
sources:
  - id: manager
    type: file
    path: src/lsp/manager.rs
  - id: client
    type: file
    path: src/lsp/client.rs
  - id: editor-sync
    type: file
    path: src/editor/lsp_coordinator.rs
  - id: workspace-edit
    type: file
    path: src/lsp/workspace_edit.rs
  - id: completion
    type: file
    path: src/ui/completion.rs
  - id: config
    type: file
    path: src/config.rs
---

# LSP Architecture

Red's LSP implementation is split between server selection, JSON-RPC transport, editor document synchronization, completion UI, and fail-closed edit application. The manager owns server routing, the client owns process transport, the editor coordinator syncs documents from editor state, workspace edits are prepared before editor-owned mutation, and completion has its own UI path [@manager] [@client] [@editor-sync] [@workspace-edit] [@completion]. Use this hub to move through that split without treating LSP as one monolithic subsystem.

## Reading Order

Start with [Client Lifecycle And Routing](client-lifecycle-and-routing) for document selectors, workspace roots, lazy process startup, failed-client handling, and polling. Then read [Transport](transport) for JSON-RPC framing, process IO, request correlation, diagnostics debounce, and shutdown.

Use [LSP Document Sync](../editor/lsp-document-sync) when a change touches lazy open, change notifications, diagnostics, path identity, or editor-side coordination. Use [Diagnostics UI](diagnostics-ui) for the editor-owned gutter, statusline, picker, and line-popup surfaces built from LSP diagnostics. Use [Completion](completion) for request context, stale-response guards, snippet handling, UI filtering, and completion edit application. Optional GitHub Copilot ghost-text suggestions live in [Copilot Inline Completion](../../guides/agent/copilot-completion), not in the LSP server lifecycle.

Read [Workspace Edits](workspace-edits) before changing rename, code action, resource operation, or multi-file edit behavior. It is the safety boundary that converts server edits into checked editor-owned changes.

[LSP Capabilities](../../concepts/lsp/capabilities) explains the advertised client capability model. [LSP Configuration](../../reference/lsp/configuration) is the exact lookup page for defaults and server fields, including Red's embedded Husk server definition [@config]. For Husk-specific server behavior, use [Husk Language Server](../husk/language-server). For diagnosis, use [Debugging LSP Failures](../../guides/lsp/debugging-lsp-failures).
