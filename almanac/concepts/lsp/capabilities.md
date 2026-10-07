---
title: "LSP Capabilities"
summary: "Red advertises a mostly conservative LSP capability set, with known gaps where advertised or routed features do not yet have editor consumers."
topics: [concepts, lsp]
sources:
  - id: capabilities
    type: file
    path: src/lsp/capabilities.rs
  - id: lsp-types
    type: file
    path: src/lsp/types.rs
  - id: lsp-trait
    type: file
    path: src/lsp/mod.rs
  - id: manager
    type: file
    path: src/lsp/manager.rs
  - id: client
    type: file
    path: src/lsp/client.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: fidget
    type: file
    path: plugins/fidget.hk
  - id: vscode-fixture
    type: file
    path: src/lsp/fixtures/vscode-capabilities.json
---

LSP capabilities are Red's contract with language servers during `initialize`: they say which protocol shapes the editor is prepared to receive, route, and apply. Red builds this contract in code instead of copying a broad editor profile, and the module warns that a new advertised capability must be paired with protocol and editor-path coverage [@capabilities]. The result is intentionally narrower than the captured Visual Studio Code capability fixture, especially around dynamic registration, file watching, refresh support, change annotation handling, and window requests [@vscode-fixture]. The contract still has known gaps where Red advertises, parses, or routes features that do not yet reach a user-visible editor consumer [@capabilities] [@lsp-trait] [@editor].

## Conservative Advertisement

Red advertises UTF-16 position encoding, static registration across text-document features, completion context support, snippets, commit characters, hover and signature documentation formats, code actions, formatting, on-type formatting, rename, diagnostics, and work-done progress [@capabilities]. It also parses the server's `documentOnTypeFormattingProvider` options, including the first trigger character and optional additional trigger characters [@lsp-types]. These values match the surrounding [transport](../../architecture/lsp/transport) and editor paths: requests use JSON-RPC correlation, server responses are routed back by method, progress notifications are enriched with server identity before reaching the editor, and edits are converted before mutation [@capabilities] [@manager].

The advertised omissions are as important as the positive features. Dynamic registration is disabled throughout the capability tree, save lifecycle flags are disabled, `window/showDocument` is not supported, diagnostics refresh is disabled, and code-action resolve support is omitted [@capabilities]. This prevents servers from assuming Red can handle extra runtime registration flows or deferred resolution paths that are not implemented.

Work-done progress is a display path, not a server-management path. `LspManager` enriches progress notifications with the originating server and workspace, `editor.rs` emits them to plugins as `lsp:progress`, and the bundled `fidget` plugin renders the grouped bottom overlay from those events [@manager] [@editor] [@fidget].

## Known Capability Gaps

The location-navigation path is narrower than the advertised LSP feature set.
Red advertises definition, declaration, type-definition, and implementation
client capabilities with static registration and `link_support(false)`, but the
editor has a single `GoToDefinition` action that sends
`textDocument/definition`, stores the request id in
`pending_definition_requests`, defers jump-list traversal while that set is
non-empty, and jumps to only the first returned location [@capabilities]
[@editor]. Adding declaration, type-definition, implementation, or multi-result
navigation is therefore an editor action, response-routing, stale-response, and
picker problem, not just a new client request method. [LSP Navigation And
Document Highlight](../../architecture/lsp/navigation-and-highlights) records
the current definition path and the planned generalization.

`textDocument/documentHighlight` is currently an advertised and routable request
without a user-visible editor consumer. The capability builder advertises
document-highlight support, and the client and manager can send
`textDocument/documentHighlight` for a file and position, but the editor does
not request, store, stale-check, or render document-highlight ranges
[@capabilities] [@client] [@manager] [@editor]. This is a contract gap: either
the advertisement should be removed, or the editor needs the idle trigger,
revision/cursor guards, theme colors, dirty-row invalidation, and rendering path
that make server highlights visible and safe.

The same pattern applies to several lower-priority feature families. Document
links, document colors, folding ranges, call-hierarchy preparation, and full
semantic tokens are advertised in the client capability tree and have trait,
client, and manager request paths, but they do not have built-in editor
consumers comparable to completion, diagnostics, formatting, rename, references,
document symbols, or plugin inlay hints [@capabilities] [@lsp-trait] [@client]
[@manager] [@editor]. Code lens, selection range, linked editing, type
hierarchy, and inline values are advertised or represented in capability/type
structures, but they have no complete high-level request path; code lens is
still a commented TODO at the LSP trait boundary [@capabilities] [@lsp-types]
[@lsp-trait].

## Workspace Edit Boundary

Red advertises `workspace.applyEdit` and a workspace edit capability that varies by platform [@capabilities]. Unix builds advertise document changes and transactional failure handling; Linux, Android, and Apple targets additionally advertise create, rename, and delete resource operations, while other Unix targets omit rename because no atomic no-replace rename path is available [@capabilities]. Non-Unix builds advertise text-only transactional failure handling and do not advertise document-change resource operations [@capabilities].

That capability shape reflects the [workspace edits](../../architecture/lsp/workspace-edits) architecture. Red does not honor change annotations, so both code-action and rename capabilities set `honorsChangeAnnotations` to false, and workspace edits that require confirmation are rejected rather than silently applied [@capabilities].

## Contrast With VS Code

The `vscode-capabilities.json` fixture records a much larger client profile that includes dynamic registration, configuration notifications, workspace file operations, code-lens and semantic-token refresh, change annotation grouping, and multiple other refresh or resolution hooks [@vscode-fixture]. Red keeps that fixture as reference material, not as the advertised runtime contract. The code-generated contract is the source of truth for what Red tells servers today [@capabilities].
