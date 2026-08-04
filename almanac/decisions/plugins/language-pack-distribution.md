---
title: "Official Language Pack Distribution"
summary: "Official language packs stay independent Red plugin packages even when authored together; catalog discovery must not bypass package lifecycle or native grammar trust."
topics: [decisions, plugins, syntax, release, safety]
sources:
  - id: package-code
    type: file
    path: src/plugin/package.rs
  - id: language-code
    type: file
    path: src/language.rs
  - id: highlighter
    type: file
    path: src/highlighter.rs
  - id: cli
    type: file
    path: src/cli.rs
  - id: main
    type: file
    path: src/main.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: language-doc
    type: file
    path: docs/LANGUAGES.md
  - id: language-pack-discussion
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/03/rollout-2026-08-03T22-35-21-019fca69-3a66-7590-b9c3-65e02dde412d.jsonl
---

# Official Language Pack Distribution

Red's future official language-pack catalog should treat each language pack as an independent external plugin package, not as one aggregate package containing every language. Current code already makes package identity the lifecycle unit: the install record stores one id, version, enabled state, source, package root, and install time; update, removal, data directories, and installed-package listing all operate through that package id [@package-code]. Language-only packages are allowed, and package language definitions merge into runtime configuration only when the user has not defined the same language id explicitly [@package-code] [@language-code]. The 2026-08-03 language-pack rollout discussion established the direction that a monorepo may be the authoring home and a catalog may be the discovery and vetting layer, while release, install/update, and trust remain per-package boundaries [@language-pack-discussion].

## Status

This is a planned distribution decision, not a fully implemented catalog. Red's public plugin install surface currently accepts either a local package path or a GitHub `owner/repository` source with an optional `@tag`, and `PluginInstallSource` records only `Path` and `GitHub` variants [@cli] [@main] [@package-code]. The user documentation still describes first-party language packs as standalone plugin repositories, so future catalog or monorepo implementation work must update docs and code together [@language-doc].

## Decision

Official language packs are authored and validated together when that improves shared tooling, but each pack stays a self-contained Red package with its own manifest, stable package id, version, release artifact, update stream, and enabled state. Do not make a root `red-plugin.toml` that aggregates Go, Swift, and future languages into one package. Red supports multiple `[languages.<id>]` entries in one manifest, but that means the whole set shares one install record, source, enabled flag, update operation, remove operation, and package data namespace [@package-code]. The discussion therefore treats a shared repository as source organization and CI, not as the installable unit [@language-pack-discussion].

The catalog is the intended discovery and vetting layer for official packs. It should point to immutable per-pack, per-target bundles and record the package id, version, Red API compatibility, source repository and path, resolved commit or tag, artifact URLs, SHA-256 digests, target triples, and external LSP requirements [@language-pack-discussion]. The catalog should identify the newest compatible version for each package instead of relying on GitHub's repository-wide latest-release concept [@language-pack-discussion].

## Consequences

Catalog-backed installation needs a new package source shape. A future `PluginInstallSource` variant should retain the catalog id, package version, resolved commit, artifact URL, and digest, then stage the unpacked package through the same manifest validation, checksum, companion or language artifact download, grammar trust, and atomic replacement machinery used today [@package-code] [@language-pack-discussion]. The existing rollback invariant remains important: failed GitHub package activation can restore the previous installation, and failed grammar approval does not publish a new installation [@package-code].

Native grammar trust remains user consent, not catalog trust. Package manifests can define grammar paths or target-specific GitHub HTTPS artifacts with SHA-256 digests, but `merge_package_languages` clears `grammar.trusted` for package languages, and `GrammarTrustStore` approves canonical grammar bytes by SHA-256 before the highlighter opens a dynamic library [@package-code] [@language-code] [@highlighter]. A catalog checksum proves provenance for a release artifact; it does not replace the explicit approval required before native Tree-sitter grammar code runs in the editor process [@language-pack-discussion].

Custom source syntax should stay narrow until a real custom-monorepo use case exists. The current CLI and editor prompt already support local paths and GitHub `owner/repository` sources with optional tags, and the editor UI lists installed packages with install, update, enable/disable, and remove actions [@cli] [@main] [@editor]. Official monorepo paths should be hidden behind catalog entries, so users choose a pack such as Go rather than typing a repository subpath. If community monorepos become common, the discussion recommends an explicit `owner/repo//path/to/pack` form with an optional `@ref` suffix to avoid ambiguity with the GitHub owner/repository pair [@language-pack-discussion].

This decision connects language-pack work to [Plugin Architecture](../../architecture/plugins), [Syntax Services](../../architecture/editor/syntax-services), and the picker/component surface in [UI Components](../../reference/editor/ui-components). It also constrains future release work: release automation may live in a shared language-pack repository, but Red should consume independent package artifacts and keep package lifecycle and native-code approval independent.
