---
title: "Arborium Language Pack Source"
summary: "Arborium can seed Red's language-pack supply chain, but Red should import it as build-time grammar and query material while keeping per-language packages and LSP tooling separate."
topics: [decisions, plugins, syntax, lsp, release]
sources:
  - id: arborium-readme
    type: web
    url: https://github.com/bearcove/arborium
  - id: arborium-develop
    type: web
    url: https://github.com/bearcove/arborium/blob/main/DEVELOP.md
  - id: catalog-code
    type: file
    path: src/plugin/catalog.rs
  - id: package-code
    type: file
    path: src/plugin/package.rs
  - id: config-code
    type: file
    path: src/config.rs
  - id: highlighter-code
    type: file
    path: src/highlighter.rs
  - id: rust-analyzer-doc
    type: web
    url: https://rust-analyzer.github.io/
  - id: gopls-doc
    type: web
    url: https://go.dev/gopls/
  - id: sourcekit-lsp-doc
    type: web
    url: https://github.com/swiftlang/sourcekit-lsp
  - id: arborium-evaluation
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/03/rollout-2026-08-03T22-35-21-019fca69-3a66-7590-b9c3-65e02dde412d.jsonl
---

# Arborium Language Pack Source

Red should use Arborium as a build-time source for Tree-sitter grammars and highlight queries, not as a single runtime language pack. Arborium is a batteries-included Tree-sitter grammar collection, and its development guide describes per-language grammar crates with committed grammar sources, query files, and metadata such as upstream repository, commit, license, tier, aliases, and scanner flags [@arborium-readme] [@arborium-develop]. Red's catalog and package code already make the package the install, update, enable, remove, and native-grammar approval unit, so an Arborium importer must emit independent Red packages rather than one shared aggregate artifact [@catalog-code] [@package-code].

## Status

This is a planned supply-chain decision. The August 2026 evaluation verified that Arborium's Go grammar could be generated, compiled as a native grammar, loaded by Red, and pass Red's configuration validation, but Red does not yet contain an Arborium importer [@arborium-evaluation]. The current catalog implementation can already publish target-specific language-pack archives and retain their catalog source in `PluginInstallSource::Catalog`, so the missing work is the importer, metadata overlay, compatibility fixes, and release automation [@catalog-code] [@package-code].

## Decision

Use a pinned Arborium release or commit as upstream grammar and query material for official packs. Red owns the overlay that Arborium does not define for this editor: exact filenames and extensions, aliases, comments, indentation, Red package descriptions, catalog metadata, LSP selectors and commands, injected-language dependencies, sample coverage, and release policy [@config-code] [@catalog-code] [@arborium-evaluation]. The first implementation slice should convert Go and Swift before generating a broad catalog, because those packs exercise both a normal external language server and a toolchain-provided language server without requiring managed LSP downloads [@arborium-evaluation].

Do not link every Arborium grammar into one shared Red library and do not publish one catalog package that contains every language. A shared library would make one native-code approval cover unrelated grammars, while Red's existing approval model records package-provided grammar bytes by exact SHA-256 before the highlighter opens them in process [@package-code] [@highlighter-code]. The existing [Official Language Pack Distribution](language-pack-distribution) decision remains the boundary: a shared repository or importer is source organization, not the installable unit.

Keep LSP binaries on a separate lifecycle from grammar packages. Red language definitions can declare local LSP settings, and config loading turns those declarations into named server launch configs, but the catalog's current requirement model only records command names, purposes, and optionality [@config-code] [@catalog-code]. That is enough to tell users what a pack expects, not enough to manage server versions, licenses, target artifacts, checksums, updates, or removal.

## Consequences

The importer must preserve Arborium's injection model before web-oriented packs are promoted. Arborium injection queries can name static injected languages with Tree-sitter query properties such as `#set! injection.language "javascript"` [@arborium-develop]. Red's current highlighter reads that static property first, falls back to dynamic `@injection.language` captures, requires `@injection.content`, and degrades without loading another grammar when the injected language is unavailable [@highlighter-code]. That means imported HTML, Svelte, Vue, Markdown variants, or similar languages should keep static injection properties in their generated query overlays rather than rewriting every injection into a dynamic capture.

Licensing stays a catalog gate rather than an Arborium default. Arborium's README says permissively licensed grammars are enabled by default, and its metadata tracks grammar licenses [@arborium-readme] [@arborium-develop]. Red's importer should still decide which generated packs become official or curated catalog entries, because package license, native grammar code, target artifacts, and Red sample coverage all affect whether a pack is safe to present as reviewed [@catalog-code].

The managed-tool catalog should be a separate project when Red is ready for it. Language servers have incompatible distribution shapes: rust-analyzer publishes prebuilt binaries for major platforms, `gopls` is tied to Go toolchain versions and workspace modes, and SourceKit-LSP is included with Swift toolchains and Xcode [@rust-analyzer-doc] [@gopls-doc] [@sourcekit-lsp-doc]. A future tool catalog can resolve an LSP by explicit user configuration, then a Red-managed installation, then a compatible executable on `PATH`, and finally a missing-tool state with exact install instructions [@arborium-evaluation]. That resolver should not be hidden inside a grammar package artifact.

The practical rollout is narrow. Build the Arborium importer, convert Go and Swift, compare highlighting against existing packs, preserve static injection-property queries in generated overlays, and publish only packages that pass licensing, ABI, query, detection, sample-highlighting, and target build checks [@arborium-evaluation] [@highlighter-code]. After that foundation exists, adding another high-quality Arborium language should usually require a small Red metadata file plus catalog release work, not a hand-built repository per grammar.

Follow [Syntax Services](../../architecture/editor/syntax-services) for runtime highlighter effects, [Plugin Lifecycle And Reload](../../architecture/plugins/lifecycle-and-reload) for package activation and quarantine, and [Release Red](../../guides/releases/release-red) when catalog changes become release work.
