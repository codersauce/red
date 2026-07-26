---
title: "LSP Capabilities"
summary: "Red advertises a conservative LSP capability set that matches its transport, editor routing, completion, diagnostics, and workspace-edit safety boundaries."
topics: [concepts, lsp]
sources:
  - id: capabilities
    type: file
    path: src/lsp/capabilities.rs
  - id: vscode-fixture
    type: file
    path: src/lsp/fixtures/vscode-capabilities.json
---

LSP capabilities are Red's contract with language servers during `initialize`: they say which protocol shapes the editor is prepared to receive, route, and apply. Red builds this contract in code instead of copying a broad editor profile, and the module warns that a new advertised capability must be paired with protocol and editor-path coverage [@capabilities]. The result is intentionally narrower than the captured Visual Studio Code capability fixture, especially around dynamic registration, file watching, refresh support, change annotation handling, and window requests [@vscode-fixture].

## Conservative Advertisement

Red advertises UTF-16 position encoding, static registration across text-document features, completion context support, snippets, commit characters, hover and signature documentation formats, code actions, formatting, rename, folding ranges, semantic tokens, inlay hints, and diagnostics [@capabilities]. These values match the surrounding [transport](../../architecture/lsp/transport) and editor paths: requests use JSON-RPC correlation, server responses are routed back by method, and edits are converted before mutation [@capabilities].

The advertised omissions are as important as the positive features. Dynamic registration is disabled throughout the capability tree, save lifecycle flags are disabled, `window/showDocument` and work-done progress are not supported, diagnostics refresh is disabled, and completion/code-action resolve support is omitted [@capabilities]. This prevents servers from assuming Red can handle extra runtime registration flows or deferred resolution paths that are not implemented.

## Workspace Edit Boundary

Red advertises `workspace.applyEdit` and a workspace edit capability that varies by platform [@capabilities]. Unix builds advertise document changes and transactional failure handling; Linux, Android, and Apple targets additionally advertise create, rename, and delete resource operations, while other Unix targets omit rename because no atomic no-replace rename path is available [@capabilities]. Non-Unix builds advertise text-only transactional failure handling and do not advertise document-change resource operations [@capabilities].

That capability shape reflects the [workspace edits](../../architecture/lsp/workspace-edits) architecture. Red does not honor change annotations, so both code-action and rename capabilities set `honorsChangeAnnotations` to false, and workspace edits that require confirmation are rejected rather than silently applied [@capabilities].

## Contrast With VS Code

The `vscode-capabilities.json` fixture records a much larger client profile that includes dynamic registration, configuration notifications, workspace file operations, code-lens and semantic-token refresh, change annotation grouping, and multiple other refresh or resolution hooks [@vscode-fixture]. Red keeps that fixture as reference material, not as the advertised runtime contract. The code-generated contract is the source of truth for what Red tells servers today [@capabilities].

