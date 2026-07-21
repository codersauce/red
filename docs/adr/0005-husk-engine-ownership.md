# ADR 0005: Husk engine and instance ownership

- Status: accepted
- Date: 2026-07-19
- Scope: compilation, sharing, mutable runtime state, and isolation

## Decision

The public runtime is split into three ownership levels:

- `Engine<HostState>` owns immutable configuration, registered module
  descriptors, compiled Wasm components, and reusable compilation caches.
- `CompiledModule` owns source files, parsed/HIR artifacts, semantic results,
  and resolved imports. It is immutable after successful compilation and can
  be shared.
- `Instance<HostState>` owns globals, heap values, closure cells, limits,
  callback handles, host state, and per-extension mutable stores.

Each standalone script execution receives its own `Instance`. Red creates one
instance per plugin generation; a transactional reload constructs a replacement
instance and swaps it only after validation, activation, and state import
succeed.

Compilation must not mutate a live instance. Wasm `Component` compilation may
be shared at engine scope, while Wasmtime `Store` and component instances stay
at Husk-instance scope.

The migration details are in the
[Husk language extraction plan](../HUSK_LANGUAGE_EXTRACTION_PLAN.md#target-architecture).

## Consequences

- Parsed and checked programs can be compiled once and instantiated repeatedly.
- Mutable state cannot leak between scripts or Red plugins accidentally.
- Limits and capabilities have a clear enforcement owner.
- Reload cannot require cloning arbitrary host resources or Wasmtime stores.
- The existing `Vm` remains a compatibility facade during migration rather
  than the final ownership model.

## Revisit when

Revisit only if a concrete workload requires intentionally shared mutable
script state, component stores become safely shareable, or profiling shows the
per-instance boundary is too expensive after pooling immutable artifacts.
