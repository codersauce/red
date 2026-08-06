---
title: "Official Language Pack Distribution"
summary: "Official language packs are catalog-discovered Red plugin packages; catalog provenance does not replace package lifecycle, platform artifacts, or native grammar trust."
topics: [decisions, plugins, syntax, release, safety]
sources:
  - id: catalog-code
    type: file
    path: src/plugin/catalog.rs
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
  - id: registry
    type: file
    path: src/plugin/registry.rs
  - id: language-doc
    type: file
    path: docs/LANGUAGES.md
  - id: language-pack-discussion
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/03/rollout-2026-08-03T22-35-21-019fca69-3a66-7590-b9c3-65e02dde412d.jsonl
---

# Official Language Pack Distribution

Red's official language-pack catalog treats each language pack as an independent external plugin package, not as one aggregate package containing every language. `PluginCatalog` is a bounded schema-versioned snapshot whose entries identify one package id, version, Red API requirement, repository and source path, resolved commit, license, review tier, contributed languages, external command requirements, and target-specific release artifacts [@catalog-code]. The install record still stores one id, version, enabled state, source, package root, and install time, so update, removal, data directories, and installed-package listing all operate through the package id [@package-code]. Language-only packages are allowed, and package language definitions merge into runtime configuration only when the user has not defined the same language id explicitly [@package-code] [@language-code]. The language-pack monorepo is the authoring home and the catalog is the discovery and vetting layer, while release, install/update, and trust remain per-package boundaries [@language-pack-discussion].

## Status

This decision is implemented for the current catalog surface. Red accepts local package paths, GitHub `owner/repository` sources with optional refs, and immutable catalog entries; `PluginInstallSource` retains the corresponding `Path`, `GitHub`, or `Catalog` source [@cli] [@main] [@package-code]. The CLI exposes `red plugin catalog` and `red plugin install --catalog <id>`, with `--catalog-url` available for an explicit catalog override [@cli] [@main]. The editor language-pack picker fetches the curated catalog, shows official or curated entries beside custom sources, reports compatibility and missing-command requirements, and routes catalog installations through a confirmed `PluginManagerCatalogInstall` action [@editor]. The user documentation describes the same catalog flow and keeps custom local or GitHub package installs separate from reviewed catalog entries [@language-doc].

## Decision

Official language packs may be authored and validated together when that improves shared tooling, but each pack stays a self-contained Red package with its own manifest, stable package id, version, release artifact, update stream, and enabled state. Do not make a root `red-plugin.toml` that aggregates Go, Swift, and future languages into one package. Red supports multiple `[languages.<id>]` entries in one manifest, but that means the whole set shares one install record, source, enabled flag, update operation, remove operation, and package data namespace [@package-code]. The rollout discussion therefore treats a shared repository as source organization and CI, not as the installable unit [@language-pack-discussion].

The catalog is discovery and provenance metadata, not native-code approval. It points to immutable per-pack, per-target bundles and validates package id, version, Red API compatibility, source repository and path, resolved commit, artifact URLs, SHA-256 digests, target triples, declared languages, and external command requirements [@catalog-code]. `PluginInstallSource::Catalog` records the catalog URL, package id, version, resolved commit, artifact URL, and artifact digest, so catalog-installed packages can update from their retained source without collapsing into a generic GitHub install [@package-code].

## Consequences

Catalog-backed installation retains the catalog id, package version, resolved commit, artifact URL, and digest. It stages the selected release archive, verifies the catalog artifact bytes, extracts into a temporary package root, validates the manifest against the catalog entry, downloads package language artifacts when needed, verifies their exact digests, obtains explicit native grammar trust, writes the catalog install record, and only then atomically replaces the installed package [@package-code]. This preserves the existing rollback invariant: a failed install or grammar approval path does not publish a partial replacement [@package-code].

Catalog compatibility uses the supported host API set, not only the current host API version. `CatalogPackage::supports_current_red_release` delegates to the same helper as plugin activation and catalog installation, so a language pack declaring a supported prior minor such as `^0.6.0` remains installable on a `0.7.0` Red release while genuinely unsupported requirements stay unavailable [@catalog-code] [@package-code] [@registry] [@language-pack-discussion]. The CLI reports catalog compatibility through that package method, and the picker uses the same check before marking a row selectable or opening install choices [@main] [@editor].

Catalog UI states keep expected incompatibilities recoverable. Red API mismatches and missing host-target artifacts appear as unavailable picker rows; selecting one reports the precise reason and keeps the language-pack picker open instead of entering the install flow [@editor]. This keeps catalog discovery useful on every host even when only some packages are installable on that host.

Native grammar consent is bound to the package and artifact the user reviewed. The picker copies the confirmed catalog URL and package into the install action, and the regression test mutates the later catalog package to prove the action still carries the originally shown grammar digest [@editor]. Downloaded bytes still need checksum verification before activation, but verification is not a substitute for consent to the exact digests displayed [@package-code] [@language-pack-discussion].

Native grammar trust remains user consent, not catalog trust. Package manifests can define grammar paths or target-specific GitHub HTTPS artifacts with SHA-256 digests, but `merge_package_languages` clears `grammar.trusted` for package languages, and `GrammarTrustStore` approves canonical grammar bytes by SHA-256 before the highlighter opens a dynamic library [@package-code] [@language-code] [@highlighter]. A catalog checksum proves provenance for a release artifact; it does not replace the explicit approval required before native Tree-sitter grammar code runs in the editor process [@language-pack-discussion].

Shared upstream language inventories, including Arborium, are build-time inputs rather than runtime bundles. Each imported language must keep its own pinned grammar source, reviewed query overlays, package artifact, lifecycle, and approval boundary. Upstream quality tiers are publication-review metadata and must not be confused with Red's `official` or `curated` catalog tier. Query inheritance can be flattened while building a package, but runtime injected languages remain optional: loading HTML, for example, must never install or implicitly approve a JavaScript grammar [@highlighter] [@language-doc].

Custom source syntax stays narrow until a real custom-monorepo use case exists. The CLI and editor prompt support local paths and GitHub `owner/repository` sources with optional tags, and the editor UI lists installed packages with install, update, enable/disable, trust, and remove actions [@cli] [@main] [@editor]. Official monorepo paths are hidden behind catalog entries, so users choose a pack such as Go rather than typing a repository subpath. If community monorepos become common, the discussion recommends an explicit `owner/repo//path/to/pack` form with an optional `@ref` suffix to avoid ambiguity with the GitHub owner/repository pair [@language-pack-discussion].

This decision connects language-pack work to [Plugin Architecture](../../architecture/plugins), [Syntax Services](../../architecture/editor/syntax-services), and the picker/component surface in [UI Components](../../reference/editor/ui-components). It also constrains [Arborium Language Pack Source](arborium-language-source): Arborium or any future shared grammar source may feed release automation, but Red should consume independent package artifacts and keep package lifecycle and native-code approval independent.
