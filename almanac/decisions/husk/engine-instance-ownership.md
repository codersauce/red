---
title: "Husk Engine Instance Ownership"
summary: "Husk separates immutable engine and compilation artifacts from isolated mutable execution instances."
topics: [decisions, husk, embedding, plugins]
sources:
  - id: adr-0005
    type: file
    path: docs/adr/0005-husk-engine-ownership.md
  - id: embedding
    type: file
    path: crates/husk-runtime/src/embedding.rs
  - id: plugin-runtime
    type: file
    path: src/plugin/runtime.rs
---

Husk's accepted ownership model separates reusable program artifacts from mutable runtime state. An `Engine<HostState>` owns immutable configuration, registered modules, and compiled extension components; a `CompiledModule` owns the immutable output of compilation; and an `Instance<HostState>` owns the VM, host state, limits, callback roots, and per-extension mutable stores for one execution boundary [@adr-0005] [@embedding]. This decision matters to Red because plugin reloads can stage a replacement and commit it only after validation, activation, teardown, and state migration succeed, rather than mutating the live plugin instance during compilation [@adr-0005] [@plugin-runtime].

## Status

This decision is accepted by ADR 0005, dated 2026-07-19 [@adr-0005]. The public embedding runtime implements the split with `Engine`, `CompiledModule`, `Instance`, and `EngineBuilder` types [@embedding].

## Context

Husk has to support repeated execution of the same checked program, Red plugin generations, and portable Wasm extension instances without sharing mutable state accidentally. ADR 0005 states that compilation must not mutate a live instance and that Wasmtime `Component` compilation may be shared at engine scope while Wasmtime `Store` and component instances stay at Husk-instance scope [@adr-0005].

The runtime implementation follows that shape. `Engine` is an `Arc` around `EngineInner`, which stores native modules, optionally compiled Wasm components, compile options, and limits [@embedding]. `CompiledModule` wraps an `Arc<CompiledProgram>` and exposes the compiled program without owning VM state [@embedding]. `Instance` contains its engine handle, program name, compiled module, `Vm`, host state, generation id, and optional map of Wasm instances [@embedding].

## Decision

Compilation belongs to the engine and produces an immutable compiled module. `Engine::compile_source`, `Engine::compile_path`, and `Engine::compile_package` compile source or packages using engine compile options and registered module descriptors, then return `CompiledModule` values backed by `Arc<CompiledProgram>` [@embedding].

Instantiation creates the mutable boundary. `Engine::instantiate` checks that every descriptor required by the compiled program still matches a registered native module or Wasm component, creates per-instance Wasm instances when the feature is enabled, allocates a new generation, configures VM limits, attaches host state, and loads the compiled plugin into the VM [@embedding]. That method is the point where reusable compiled code becomes live state.

Red uses a compatibility VM path for plugins while preserving the same ownership intent. `Runtime::load_plugin_at` compiles plugin source first, begins reload staging on the host, reloads the compiled plugin into a plugin VM, commits staged host effects on success, and rolls them back on failure [@plugin-runtime]. This keeps reload behavior aligned with [plugin lifecycle and reload](../../architecture/plugins/lifecycle-and-reload), even though ADR 0005 says the existing `Vm` remains a compatibility facade during migration [@adr-0005].

## Consequences

Compiled programs can be reused or inspected without cloning live VM state. The embedding API exposes compiled program access through `CompiledModule::program`, and instantiation consumes a compiled module to create a separate VM and host-state boundary [@embedding].

Mutable state is intentionally scoped to an instance or plugin generation. Instance generation ids are written into the VM, callback handles carry generation information, and Red allocates plugin VM generations so stale callbacks from old plugin generations can be rejected [@embedding] [@plugin-runtime].

Wasm resource ownership remains local to the instance. The Wasm tier can share compiled components through the engine, but each instantiation owns its own store and component instance, which is the boundary future work must preserve when extending the [public embedding API](../../architecture/husk/public-embedding-api) [@adr-0005] [@embedding].
