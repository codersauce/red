---
title: "Husk Public Embedding API"
summary: "The public Husk embedding API centers on an immutable engine, reusable compiled modules, isolated instances, typed native modules, optional Wasm components, and a transactional REPL."
topics: [husk, embedding, architecture]
sources:
  - id: facade
    type: file
    path: crates/husk/src/lib.rs
  - id: embedding
    type: file
    path: crates/husk-runtime/src/embedding.rs
  - id: embedding-tests
    type: file
    path: crates/husk/tests/embedding_api.rs
---

The Husk public embedding API is the Rust-facing boundary for applications that compile and run Husk without going through the standalone CLI. The facade crate re-exports the runtime types an embedder needs: `Engine`, `CompiledModule`, `Instance`, `NativeModule`, `CallContext`, `OwnedValue`, package descriptors, limits, REPL types, and optional Wasm extension types [@facade]. Its shape is deliberate: the embedder builds one immutable engine, compiles source or packages into reusable artifacts, and creates isolated instances that own mutable VM state and host state [@embedding]. The same boundary is what the [Husk language](../../concepts/husk-language) pages and the extension architecture build on.

## Engine Boundary

`Engine<T>` owns immutable compile options, resource limits, registered static native modules, and, when the `wasm-extensions` feature is enabled, compiled WebAssembly components [@embedding]. The builder accepts native modules with `register_module`, portable components with `register_wasm_component`, explicit `Limits`, a semantic profile, typecheck toggling, and compile-time `cfg` flags [@embedding]. Module registration rejects duplicate names and also rejects name collisions between native modules and Wasm components, so a compiled program cannot bind the same Husk module root to two host providers [@embedding].

Compilation is separate from execution. `compile_source` compiles an in-memory source string with a stable display path, `compile_path` reads one bounded UTF-8 file, and `compile_package` compiles the modules from a deterministically resolved package [@embedding]. Compilation snapshots the module descriptors visible through the engine, while instantiation later verifies that each required module descriptor still matches the engine registration [@embedding]. That descriptor check is the guard that lets `CompiledModule` be cloned and reused without accepting a changed host surface silently.

## Instance Ownership

`Instance<T>` is the mutable execution object. Instantiation creates a VM, assigns a fresh generation, installs runtime budgets, loads the compiled program, stores the caller-provided host state, and creates one Wasm instance per registered component when that feature is present [@embedding]. An instance is intentionally not `Sync`, and the tests assert that two instances created from the same compiled module have independent generations and independent host state [@embedding] [@embedding-tests]. For the ownership rationale, see [Engine Instance Ownership](../../decisions/husk/engine-instance-ownership).

Script calls cross the boundary through detached `OwnedValue` data. `Instance::call`, `capture_function`, and `invoke_function` validate boundary value size before converting values into runtime representation, and they validate detached results before returning them to the caller [@embedding]. Retained closure handles remain roots until `release_function` or instance drop, and the tests cover the callback root limit as an enforced runtime budget [@embedding] [@embedding-tests].

## Native Modules

Static Rust integration uses `NativeModule<T>`. A native module has a `ModuleDescriptor` and a map of handlers; each call receives a `CallContext` containing mutable host state and the current module and function names [@embedding]. `NativeModule::builder(...).typed_function(...)` derives parameter descriptors and conversion logic from Rust types for supported arities, and `NativeError` represents a host-side failure rather than a script-level `Result` [@embedding].

The typed adapter surface supports exact conversions for primitive and structured values such as `bool`, `i32`, `i64`, `f64`, `String`, `Vec<T>`, `Option<T>`, two-element tuples, and `ScriptResult<T, E>` returns [@embedding]. Conversion failures include module, function, argument index, expected type, and actual value kind; the embedding tests assert that those details appear with source context when typechecking is disabled and a bad call reaches the native boundary [@embedding] [@embedding-tests].

## REPL Contract

The embedding API also exposes `ReplSession<T>`. A session compiles each complete item or statement against accumulated session source, preserves top-level items and locals, and returns `ReplOutcome::Incomplete` for fragments that need more input [@embedding]. Invalid syntax, semantic failures, limit failures, and runtime failures do not commit script-owned state; the tests cover preserved definitions, preserved native module state, stable closure targets after later definitions, and rollback of failed script state [@embedding] [@embedding-tests].

The REPL still shares the host-state boundary with ordinary execution. A failed script statement is rolled back inside the VM, but native or Wasm calls made before the failure can already have changed host-owned state because the runtime cannot undo external side effects [@embedding].

## Limits And Extension Hooks

The same `Limits` value controls embedded and standalone execution. Instantiation applies call depth, native host-call budget, detached value size, heap object count, heap byte count, callback root count, and instruction budget to the VM [@embedding]. With portable extensions enabled, the engine maps instruction, heap, value, and extension-instance budgets into per-component Wasm limits before creating the instance-local stores [@embedding]. The dynamic extension tier is documented in [Husk Extensions](extensions), and package-driven extension loading is documented in [Husk Packages And Locks](packages-and-locks).
