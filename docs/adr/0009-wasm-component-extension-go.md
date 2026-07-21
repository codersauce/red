# ADR 0009: Use WebAssembly Components for portable dynamic extensions

- Status: Accepted, with three-platform CI as a release gate
- Date: 2026-07-19

## Context

Rust does not define a stable ABI for dynamically loading arbitrary Cargo
crates. Loading Rust trait objects or Rust-owned values from a `cdylib` would
couple extensions to compiler, dependency, allocator, panic, and layout details
and would execute them with the host process's full authority.

Husk needs a dynamic tier that can be used by the standalone interpreter while
remaining typed, inspectable, and denied ambient authority by default.

The feasibility implementation and measurements are recorded in
[`../husk-wasm-feasibility-2026-07-19.md`](../husk-wasm-feasibility-2026-07-19.md).

## Decision

Use WebAssembly Components as Husk's portable dynamic extension tier.

- Extension authors compile a narrow Rust/WIT adapter, not a raw Cargo crate.
- The host compiles a Wasmtime `Component` once per Husk engine.
- Each Husk instance owns its own Wasmtime store and component instance.
- The loader derives the Husk `ModuleDescriptor` from dynamic Component export
  and type inspection.
- Calls use Component `Val`; JSON is not an implicit transport.
- Actual imports are checked against manifest requests and host grants.
- No WASI world or capability provider is linked by default.
- Fuel and store resource limits apply to every instance.
- The initial public mapping rejects types without an exact Husk contract.

Statically linked `NativeModule<T>` adapters remain the preferred way for a
Rust embedding application to expose crates. They support crates that cannot
target `wasm32-wasip2` and carry the embedding application's trust.

## Consequences

Portable extensions are sandboxable and dynamically discoverable, but only
dependency graphs that compile to the Component target can use this tier.
Crates requiring native libraries, host threads, or unrestricted OS APIs need
a static adapter.

The compiled Wasmtime dependency is optional at the `husk-runtime` boundary
through the `wasm-extensions` feature. `husk-runtime --no-default-features`
continues to build without it.

A future trusted native dynamic tier may use a narrow versioned C ABI, but it
must remain explicit, opt-in, and out of process trust assumptions. It is not a
fallback that weakens the portable tier.
