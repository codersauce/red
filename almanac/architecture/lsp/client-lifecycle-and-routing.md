---
title: "LSP Client Lifecycle And Routing"
summary: "Red routes buffers to lazily started language-server processes by document selector and workspace root, then polls all live clients fairly while treating unsupported files as no-op targets."
topics: [architecture, lsp]
sources:
  - id: manager
    type: file
    path: src/lsp/manager.rs
  - id: config
    type: file
    path: src/config.rs
  - id: lazy-tests
    type: file
    path: tests/lsp_lazy.rs
---

Red's LSP manager is the routing layer between editor actions and language-server processes. It builds document selectors from configured servers without starting processes, resolves a file to a server, language id, URI, and workspace root when an operation needs LSP, and then lazily starts one client for each `(server, workspace root)` key [@manager]. Unsupported files are valid no-op targets for most LSP operations, while process and protocol failures remain errors that can be surfaced by the editor [@manager].

## Selector And Workspace Resolution

The configuration model has a global LSP switch, a format-on-save option, and named server definitions with command, arguments, environment, root markers, initialization options, workspace name, and document selectors [@config]. A server can use explicit `documents` entries or legacy `language_id` plus `file_extensions`; the `documents()` helper normalizes both forms into selector records [@config].

`LspManager::new` sorts configured servers by name and registers the first selector for each lowercased extension, so extension collisions are deterministic [@manager]. `resolve_document` returns no document when LSP is disabled, the extension is not selected, or the path and URI cannot be normalized [@manager]. When a selector matches, workspace discovery walks ancestors from the file's parent and returns the first directory containing any configured root marker; if no marker is found, it falls back to the current working directory or the file's starting directory [@manager].

## Lazy Process Lifecycle

The manager starts no server during construction [@manager]. Opening or changing a routed document calls `client_for_document`, which starts `RealLspClient`, sends `initialize`, and records the client only after successful startup and initialization [@manager]. If startup or initialization fails, the client key is added to `failed_clients`, and future requests for that key return `Ok(None)` instead of repeatedly trying the broken process [@manager].

Document identity is tracked separately from client lifetime. `did_open` skips duplicate opens for the same file, records the owning client key, and remembers the full `(server, workspace, URI)` document key so a reopened view can reuse the same managed lifecycle [@manager]. The editor-side lazy tests enforce that constructing an editor does not open inactive LSP buffers, activating a buffer opens it once, switching away and back does not reopen it, and deleting then reopening a buffer sends close followed by a fresh open [@lazy-tests].

## Request Routing And Polling

Most LSP methods route through `client_for_file`, which reuses the remembered client for an already opened file and otherwise resolves the file lazily [@manager]. URI-driven requests such as completion and diagnostics use `client_for_uri_mut`, which decodes the URI to a file path and finds the remembered or resolvable client [@manager]. Workspace-wide symbol requests use the first available client when no file-scoped source is provided [@manager].

Inbound polling is round-robin across sorted client keys. If the stored poll order no longer matches the live client map, it is rebuilt from the current keys; each successful poll advances the next starting index so one busy server does not permanently starve another [@manager]. Progress notifications are enriched with server name and workspace root, and server-initiated requests receive their managed client key as `source` so later responses and workspace edits can be sent back to the process that originated them [@manager].

## Relationship To Capabilities And Document Sync

The lifecycle layer depends on the advertised [LSP capabilities](../../concepts/lsp/capabilities) being truthful: it can route static requests and workspace edits, but it does not implement dynamic registration or broad client-side server-management features. It also sits above editor document synchronization; the tests show user-visible actions open the active buffer before hover, symbols, references, formatting, code actions, signature help, and rename requests [@lazy-tests].

