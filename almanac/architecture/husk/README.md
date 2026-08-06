---
title: "Husk Architecture"
summary: "Husk architecture routes readers through the workspace crates, embedding API, packages and locks, extension tiers, language server, and command/reference surfaces."
topics: [architecture, husk, scripting]
sources:
  - id: cargo
    type: file
    path: Cargo.toml
  - id: facade
    type: file
    path: crates/husk/src/lib.rs
  - id: embedding
    type: file
    path: crates/husk-runtime/src/embedding.rs
  - id: package
    type: file
    path: crates/husk-package/src/lib.rs
  - id: extension
    type: file
    path: crates/husk-extension/src/lib.rs
  - id: lsp-server
    type: file
    path: crates/husk-lsp/src/server.rs
---

# Husk Architecture

Husk is both Red's embedded plugin language and a standalone scripting workspace. The repository workspace includes crates for Husk parsing, diagnostics, analysis, semantic checking, runtime execution, packaging, extension validation, WebAssembly extension support, CLI behavior, and the Husk language server [@cargo]. Use this hub to choose the architecture page for the layer you need to change before moving to the more specific decisions and references.

## Reading Order

Start with the [Husk language](../../concepts/husk-language) concept for the product-level mental model. Then read [Husk Public Embedding API](public-embedding-api) when a change touches the Rust-facing facade, `Engine`, reusable compiled modules, isolated `Instance` state, native modules, boundary values, REPL behavior, or execution limits. The facade crate re-exports the public embedding types, package types, limits, REPL types, and optional Wasm extension types used by embedders [@facade].

Use [Husk Packages And Locks](packages-and-locks) when work touches `Husk.toml`, `Husk.lock`, module resolution, deterministic package graphs, embedded source packages, or extension provenance. The package crate defines the manifest and lock filenames, resolves package source modules, discovers manifests, supports embedded source packages, and validates lock state against declared extension inputs [@package].

Read [Husk Extensions](extensions) when changing static native modules, `.huskext` bundles, component manifests, capability validation, component inspection, adapter workflows, or the CLI extension commands. The extension crate validates portable bundle manifests, size limits, normalized artifact paths, and the capability rule that actual imports must be declared and granted before component use [@extension].

Use [Husk Language Server](language-server) for editor-facing analysis, JSON-RPC handling, dependency stubs, diagnostics, completion, semantic tokens, rename, formatting, and Red's default `.hk` and `.husk` server configuration. The server initializes one workspace, rejects requests before initialization, handles LSP text and workspace methods, and publishes capabilities for the Husk editing surface [@lsp-server].

For exact command lookup, use [Husk Command](../../reference/cli/husk-command). For Red's default LSP launch configuration, use [LSP Configuration](../../reference/lsp/configuration). For why scripts, modules, native profiles, value semantics, engine ownership, and extension tiers have their current shape, use the Husk decision pages under `decisions/husk`.

## Runtime Boundaries

Compilation and execution are separate. `Engine::compile_source`, `compile_path`, and `compile_package` produce reusable compiled modules, while `Engine::instantiate` creates isolated mutable runtime state and verifies that required module descriptors still match the engine registrations [@embedding].

Packages are local and deterministic. The package layer resolves source modules from `Husk.toml`, validates source paths and module layout, and uses `Husk.lock` to connect extension declarations to exact local bundle bytes or vendored artifacts [@package].

Portable extensions are validated before Wasmtime is involved. The `.huskext` bundle boundary checks filesystem shape, manifest schema, normalized artifact paths, size limits, and capability declarations without depending on the Wasm runtime [@extension]. The Wasm-specific architecture and extension tier rationale are covered by [Husk Extensions](extensions) and [Extension Tiers](../../decisions/husk/extension-tiers).
