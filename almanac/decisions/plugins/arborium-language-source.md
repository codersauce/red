---
title: "Arborium Language Pack Source"
summary: "Red uses Arborium as build-time grammar and query material through the language-pack source repository, while keeping per-language packages and LSP tooling separate."
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
  - id: packs-readme
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/README.md
  - id: packs-contributing
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/CONTRIBUTING.md
  - id: validate-workflow
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/.github/workflows/validate.yml
  - id: arborium-importer
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/scripts/arborium.py
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

Red uses Arborium as a build-time source for Tree-sitter grammars and highlight queries, not as a single runtime language pack. Arborium is a batteries-included Tree-sitter grammar collection, and its development guide describes per-language grammar crates with committed grammar sources, query files, and metadata such as upstream repository, commit, license, tier, aliases, and scanner flags [@arborium-readme] [@arborium-develop]. The importer lives in the separate `codersauce/red-language-packs` source repository, where only languages with explicit Red metadata overlays are generated into independent packs [@packs-readme] [@arborium-importer]. Red's catalog and package code then consume those packages as separate install, update, enable, remove, and native-grammar approval units rather than as one shared aggregate artifact [@catalog-code] [@package-code].

## Status

This decision is active through the language-pack source repository. That repository documents Arborium as a pinned, digest-verified build-time grammar and query source, keeps Red-owned metadata under `arborium/languages/`, and runs `scripts/arborium.py inventory --check` plus `scripts/arborium.py sync --check` in its validation workflow [@packs-readme] [@packs-contributing] [@validate-workflow]. The Red repository still does not contain the importer; its runtime boundary is the curated catalog and installed package manager, which retain catalog provenance in `PluginInstallSource::Catalog` and revalidate target artifacts before installation [@catalog-code] [@package-code].

## Decision

Use a pinned Arborium release or commit as upstream grammar and query material for official packs. The language-pack repository owns the overlay that Arborium does not define for this editor: exact filenames and extensions, aliases, comments, indentation, Red package descriptions, catalog metadata, LSP selectors and commands, injected-language dependencies, sample coverage, and release policy [@packs-readme] [@packs-contributing]. The published source already covers separate packs such as Go, Swift, HTML, Svelte, Vue, Python, Zig, and other reviewed languages rather than waiting for a first broad-catalog implementation [@packs-readme].

Do not link every Arborium grammar into one shared Red library and do not publish one catalog package that contains every language. A shared library would make one native-code approval cover unrelated grammars, while Red's existing approval model records package-provided grammar bytes by exact SHA-256 before the highlighter opens them in process [@package-code] [@highlighter-code]. The existing [Official Language Pack Distribution](language-pack-distribution) decision remains the boundary: a shared repository or importer is source organization, not the installable unit.

Keep LSP binaries on a separate lifecycle from grammar packages. Red language definitions can declare local LSP settings, and config loading turns those declarations into named server launch configs, but the catalog's current requirement model only records command names, purposes, and optionality [@config-code] [@catalog-code]. That is enough to tell users what a pack expects, not enough to manage server versions, licenses, target artifacts, checksums, updates, or removal.

## Consequences

The importer must preserve Arborium's injection model before web-oriented packs are promoted. Arborium injection queries can name static injected languages with Tree-sitter query properties such as `#set! injection.language "javascript"` [@arborium-develop]. Red's current highlighter reads that static property first, falls back to dynamic `@injection.language` captures, requires `@injection.content`, and degrades without loading another grammar when the injected language is unavailable [@highlighter-code]. That means imported HTML, Svelte, Vue, Markdown variants, or similar languages should keep static injection properties in their generated query overlays rather than rewriting every injection into a dynamic capture.

Licensing stays a catalog gate rather than an Arborium default. Arborium's README says permissively licensed grammars are enabled by default, and its metadata tracks grammar licenses [@arborium-readme] [@arborium-develop]. The language-pack importer rejects licenses outside the allowlist, immature quality tiers, unpinned upstream sources, missing reviewed metadata, and required highlight-capture regressions before generated packs can become official or curated catalog entries [@packs-readme] [@arborium-importer].

The managed-tool catalog should be a separate project when Red is ready for it. Language servers have incompatible distribution shapes: rust-analyzer publishes prebuilt binaries for major platforms, `gopls` is tied to Go toolchain versions and workspace modes, and SourceKit-LSP is included with Swift toolchains and Xcode [@rust-analyzer-doc] [@gopls-doc] [@sourcekit-lsp-doc]. A future tool catalog can resolve an LSP by explicit user configuration, then a Red-managed installation, then a compatible executable on `PATH`, and finally a missing-tool state with exact install instructions [@arborium-evaluation]. That resolver should not be hidden inside a grammar package artifact.

The practical rollout path is now metadata-first. Adding another high-quality Arborium language should require a reviewed Red metadata file plus catalog release work, because the source repository scaffolds the manifest, catalog metadata, provenance, example, query overlay, and documentation from that overlay without changing another pack [@packs-readme] [@packs-contributing]. Red-side changes are still needed when the editor's package schema, catalog validation, host API requirement, formatter contract, or highlighter behavior changes [@catalog-code] [@package-code] [@highlighter-code].

Follow [Syntax Services](../../architecture/editor/syntax-services) for runtime highlighter effects, [Plugin Lifecycle And Reload](../../architecture/plugins/lifecycle-and-reload) for package activation and quarantine, and [Release Red](../../guides/releases/release-red) when catalog changes become release work.
