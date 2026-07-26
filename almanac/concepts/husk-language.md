---
title: "Husk Language"
summary: "Husk is Red's embedded scripting language and standalone Rust workspace for plugins, packages, a CLI, embedding, and portable extensions."
topics: [husk, plugins, scripting]
sources:
  - id: guide
    type: file
    path: docs/HUSK_LANGUAGE_GUIDE.md
  - id: cargo
    type: file
    path: Cargo.toml
  - id: facade
    type: file
    path: crates/husk/src/lib.rs
  - id: plugin-runtime
    type: file
    path: src/plugin/runtime.rs
---

Husk is the scripting language Red embeds for plugins and also a standalone Rust workspace with a CLI, package resolver, runtime, language server, semantic tooling, standard library, and extension system. The repository root manifest lists Husk crates for the facade, CLI, parser, runtime, package handling, LSP, semantic analysis, value/types, standard library, and WebAssembly support, while the language guide describes `husk check`, `husk run`, `husk test`, `husk repl`, local packages, embedding, and portable `.huskext` extensions [@cargo] [@guide]. In Red itself, the plugin runtime hosts Husk VM code behind a Red-specific API that translates plugin calls into editor-owned requests [@plugin-runtime].

## Language And CLI

The guide defines the standalone CLI surface: `cargo run -p husk-cli -- check`, `run`, `test`, and `repl`, with installed binaries invoked as `husk ...` or through Red's forwarded `red husk ...` entrypoint [@guide]. One-file scripts use Rust-like `fn main` signatures, and the CLI's built-in `std` module intentionally exposes only `print` and `println`, without ambient filesystem, network, environment, process, clock, or random access [@guide].

Husk packages are local and filesystem-based. The guide describes `Husk.toml`, deterministic module resolution, lock files, `red husk new`, `red husk add`, `red husk install --locked --offline`, and reproducible `--locked` commands [@guide]. That package and CLI behavior belongs in [Husk command](../reference/cli/husk-command).

## Embedding Model

The public `husk` crate is a facade over `husk_runtime`. Its library documentation says embedders compile source with an `Engine`, instantiate isolated mutable state, and register statically linked Rust crates through `NativeModule`; the exported API includes `Engine`, `EngineBuilder`, `NativeModule`, `Instance`, package types, REPL types, descriptors, values, limits, and extension sources [@facade]. The guide gives the same model in prose: `Engine` and compiled modules are shareable immutable artifacts, while each `Instance` owns VM heap, script state, callback roots, budgets, host state, and Wasm stores [@guide].

The embedding distinction matters for Red. New standalone applications should use the native semantic profile, while Red's plugin compatibility path can select a legacy JavaScript profile for existing plugin behavior [@guide]. The detailed host-facing API is covered by [Husk public embedding API](../architecture/husk/public-embedding-api).

## Red Plugin Runtime

Red's plugin runtime wraps the Red-agnostic VM with a host that turns Husk calls into `PluginRequest` values consumed by the editor [@plugin-runtime]. The runtime's host declarations expose the `red` module used by plugins for commands, event listeners, editor requests, UI pickers and composers, viewport and window snapshots, plugin state, runtime assets, and core helpers [@plugin-runtime].

Reload behavior is part of the language's role inside Red. The runtime stages host effects during plugin replacement, commits them only after the replacement activates and the old plugin tears down successfully, and discards staged requests, logs, and timers on rollback [@plugin-runtime]. Bundled plugin behavior is described in [Bundled Husk plugins](plugins/bundled-husk-plugins), and runtime loading is covered by [Plugin lifecycle and reload](../architecture/plugins/lifecycle-and-reload).

## Extension Boundary

Husk does not load arbitrary Cargo crates directly at runtime. The guide explains that Rust crate artifacts lack a uniform runtime function inventory, stable Rust ABI, and portable representation for generics, traits, macros, closures, and Rust-layout containers; exposed crates therefore need adapters that define callable surfaces and type mappings [@guide]. Static native modules fit embedders that control their Cargo graph, while WebAssembly Components support dynamically discovered standalone extensions [@guide].
