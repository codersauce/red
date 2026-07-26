---
title: "Husk Language Server"
summary: "The Husk language server combines editor-oriented analysis, locked dependency stubs, bounded JSON-RPC handling, and Red defaults for .hk and .husk files."
topics: [husk, lsp, architecture]
sources:
  - id: lsp-doc
    type: file
    path: docs/HUSK_LSP.md
  - id: analysis-code
    type: file
    path: crates/husk-analysis/src/lib.rs
  - id: lsp-server
    type: file
    path: crates/husk-lsp/src/server.rs
  - id: dependency-code
    type: file
    path: crates/husk-lsp/src/dependencies.rs
  - id: red-config
    type: file
    path: src/config.rs
---

The Husk language server is the first-party LSP implementation for `.hk` and `.husk` files. It uses `husk-analysis` for recovered syntax, semantic analysis, overlays, symbols, formatting, and UTF-16 position mapping, and `husk-lsp` for JSON-RPC transport, LSP request routing, dependency indexing, diagnostics, and editor protocol responses [@lsp-doc] [@analysis-code] [@lsp-server]. Red enables this server by default by launching the current Red executable with `husk lsp --stdio`, while standalone users can launch the same server through `husk lsp --stdio` or `husk-lsp` [@lsp-doc] [@red-config].

## Workspace Analysis

`Workspace::open` canonicalizes one root, stores a semantic profile, and immediately refreshes disk state [@analysis-code]. If a `Husk.toml` is discoverable, the workspace resolves the package, converts source module descriptors into declarations, and inserts each package module in deterministic package order [@analysis-code]. If package discovery or resolution fails, the workspace records a package diagnostic and falls back to loose-source loading, which scans `.hk` and `.husk` files while skipping `.git`, `.husk`, `target`, `vendor`, and `node_modules` directories [@analysis-code].

Open editor buffers replace disk text without writing it. `Workspace::update` requires the document path to remain inside the workspace root, enforces a 1 MiB document limit, preserves or infers the module path, and reanalyzes the in-memory text [@analysis-code]. This is why the server can diagnose unsaved revisions and still reload disk state on save or watched-file changes without losing open overlays [@analysis-code] [@lsp-server].

## LSP Surface

The server runs one session over standard input and output, reads JSON-RPC messages, dispatches requests and notifications, and writes responses without mixing protocol messages with diagnostics on stdout [@lsp-server]. Initialize chooses a workspace root, reads `semanticProfile`, `looseSemanticProfile`, and declaration sources from initialization options, opens the analysis workspace, indexes locked dependencies, registers external module declarations, and returns the server capabilities [@lsp-server].

The advertised capabilities include UTF-16 position encoding, incremental text sync, pull diagnostics, completion, hover, signature help, definition-family requests, references, document highlights, prepare-rename and rename, document and workspace symbols, full semantic tokens, inlay hints, folding ranges, selection ranges, code actions, formatting, range formatting, and call hierarchy [@lsp-server]. This matches the editor feature summary in the LSP documentation, which also notes that recovered parser syntax remains available while a document is incomplete [@lsp-doc].

## Dependency Stubs

Package extension dependencies are indexed read-only from verified package state. The dependency indexer discovers `Husk.toml`, parses `Husk.lock`, validates that the lock matches the manifest, chooses an installed bundle under `.husk/extensions/` or a vendored artifact fallback, validates the bundle digest, optionally validates an adapter-report digest, inspects the component without executing guest code, and turns the resulting descriptor into a Husk declaration stub [@dependency-code]. Stub paths are content-addressed under `.husk/lsp/<component-digest>/<module>.hk`, and writes reject symlinked cache directories or paths escaping the package root [@dependency-code].

Adapter report documentation is attached to matching generated declarations when the report is present and valid [@dependency-code]. Dependency stubs participate in navigation, hover, completion, and type checking, but rename treats external dependency symbols as read-only and refuses edits inside stubs [@lsp-server]. This preserves local editing features while keeping generated dependency surfaces derived from lock-proven component exports rather than from Cargo metadata at edit time.

## Red Defaults And Profiles

Red's default language-server configuration defines a `husk` server whose command is the current Red executable and whose arguments are `husk lsp --stdio` [@red-config]. It routes `.hk` and `.husk` documents to language ID `husk`, uses `Husk.toml` and `.git` as root markers, sets the workspace name to `husk`, and passes Red's trusted Husk declaration source plus `looseSemanticProfile = "legacyJavaScript"` in initialization options [@red-config]. Tests assert these defaults, including that the declaration source contains Red host declarations [@red-config].

The server itself chooses `SemanticProfile::Native` when a manifest is discoverable, unless initialization explicitly selects a profile; otherwise it uses the loose profile or its process default [@lsp-server]. This matches the documented Red integration: real packages use native Husk semantics, while loose Red plugin files can use legacy JavaScript compatibility and trusted Red declarations so `red::*`, `Json`, callback types, and editor host signatures participate in analysis [@lsp-doc]. For the surrounding LSP lifecycle, see [Client Lifecycle And Routing](../lsp/client-lifecycle-and-routing) and the [LSP Configuration](../../reference/lsp/configuration) reference.

## Bounds And Failure Modes

The LSP documentation states the main operational bounds: one process owns one workspace folder, at most 256 indexed Husk files, 1 MiB source files and overlays, 16 KiB JSON-RPC headers, 16 MiB messages, local `file:` URIs inside the workspace only, and fail-closed handling for symlinked package inputs, bundles, artifacts, and generated-stub targets [@lsp-doc]. The analysis crate enforces the 256-file workspace limit and 1 MiB document limit, and the dependency writer rejects symlinked stub directories [@analysis-code] [@dependency-code].

When dependency indexing cannot prove the extension state, it records diagnostics and lets local source analysis continue [@dependency-code]. The LSP documentation names these as dependency diagnostics and directs users to prepare exact offline state with `red husk install --locked --offline` after cloning a package [@lsp-doc].
