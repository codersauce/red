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

## Owner Map

Use the first failing boundary to choose the page and source file. If no server starts for a file, inspect selectors, root markers, disabled LSP state, and the failed-client cache in the manager before looking at process IO [@manager]. If a server starts but messages do not correlate, parse, initialize, or shut down cleanly, inspect the transport client [@client]. If diagnostics, hovers, symbols, formatting, code actions, signature help, or rename happen against the wrong document identity, inspect the editor-side document sync path because it owns lazy open state, revision snapshots, URI identity, and close notifications [@editor-sync].

Completion failures usually cross two boundaries. The LSP request and response route through the client, but stale-response rejection, snippet handling, filtering, and accepted-item application live in the completion UI path and editor mutation path [@client] [@completion]. Multi-file rename, code action, formatting, or `workspace/applyEdit` failures should move to [Workspace Edits](workspace-edits), because Red parses and prepares the entire operation before any buffer or filesystem mutation occurs [@workspace-edit].

## Boundaries To Preserve

Do not let server output mutate editor state directly. The transport turns protocol traffic into typed inbound messages, and the editor remains the owner of diagnostics display, completion acceptance, buffer mutation, and follow-up document-sync notifications [@client] [@editor-sync] [@completion].

Do not collapse completion edits, workspace edits, and raw buffer replacement into one path. Completion applies a single accepted item through completion-specific stale guards and text-edit conversion, while workspace edits validate ordered multi-document and resource operations before routing prepared contents through editor-owned mutation [@completion] [@workspace-edit].

Keep advertised capabilities conservative. The capability model and configuration reference describe what Red promises to language servers; adding a capability without the matching manager, client, editor, and validation behavior creates a contract the rest of the LSP stack may not satisfy [@config].
