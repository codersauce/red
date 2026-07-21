# ADR 0004: Husk extension tiers

- Status: accepted
- Date: 2026-07-19
- Scope: using Rust crates from embedded and standalone Husk

## Decision

Husk supports three deliberately separate extension tiers:

1. **Static native modules** are the preferred Rust embedding API. The
   embedding application links ordinary Cargo dependencies and registers typed
   adapter functions with a Husk `Engine`.
2. **WebAssembly Components** are the preferred dynamically loaded extension
   format for the standalone CLI. An adapter crate compiles for
   `wasm32-wasip2`, exports a versioned WIT world, and is loaded with Wasmtime's
   Component Model API.
3. A **native C ABI** may be added later for trusted, platform-specific
   deployments. It is opt-in, unsafe by definition, and never the default
   package format.

Husk does not attempt to load an arbitrary Rust `rlib` or Rust-language symbol
from a dynamic library. Rust has no stable general-purpose ABI for that
contract. Each tier instead exposes a narrow, versioned Husk boundary.

The full packaging, type mapping, capability, and load sequence is specified in
the [Husk language extraction plan](../HUSK_LANGUAGE_EXTRACTION_PLAN.md#rust-crate-extensions).

## Consequences

- Embedders can use any crate their application can compile, with no runtime
  loading tax.
- Standalone extensions are portable and sandboxable when their dependency
  graph supports `wasm32-wasip2`.
- Extension APIs are descriptors and typed values, not Rust implementation
  types or ad hoc JSON.
- Native-only crates need a static host adapter or the future trusted ABI.
- Wasm imports are denied unless both declared and granted.

## Revisit when

Revisit this decision only if a stable Rust ABI becomes a supported ecosystem
contract, the Component Model cannot provide the required typed introspection,
or measured Wasm overhead prevents a documented production use case. A Wasm
fallback must still use an explicit versioned wire schema rather than
unversioned JSON.
