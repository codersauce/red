# `husk add <crate>` implementation plan

## Goal

Let a user run:

```shell
red husk add regex
```

Husk should inspect the requested crates.io crate, expose the parts it can map
safely, build a portable adapter, and explain every part it cannot support.

The command accepts an arbitrary crate name. It does not promise that every
Rust API can be represented in Husk.

## Product contract

`husk add` will:

1. resolve the requested crate and feature set;
2. inspect its public API;
3. classify supported and unsupported items;
4. show the proposed Husk interface;
5. generate a Rust-to-WIT adapter;
6. build it in an isolated environment;
7. verify the resulting component and its capabilities; and
8. update the Husk manifest and lock file atomically.

Normal `check`, `test`, and `run` commands never invoke Cargo or access the
network.

## Implementation order

### 1. Represent common Rust values

Add mappings for integers, strings, lists, tuples, options, results, records,
enums, and byte buffers. Unsupported values must produce precise diagnostics.

Status: core value mappings exist; broader integer and collection coverage is
still needed.

### 2. Represent objects and resources

Many crates return stateful objects such as a compiled regex or HTTP client.
Husk needs opaque handles that can be constructed, borrowed by methods, moved,
and destroyed safely.

Status: initial implementation complete.

- Component `own<T>` and `borrow<T>` are reflected in module descriptors.
- Husk values can carry instance-scoped opaque handles.
- Owning calls invalidate all copied aliases.
- Stale, wrong-type, and cross-instance handles are rejected.
- Explicit destruction and instance cleanup call the guest destructor.
- A component fixture proves construct, borrow, transfer, and drop through
  real Husk source.

### 3. Control capabilities

Adapters must declare filesystem, network, clock, environment, random, and
process access. Installation must not grant any capability implicitly.

Status: Husk already checks declared versus observed imports. Host providers
and user-facing grant policy remain to be implemented.

### 4. Inspect crate compatibility

Add:

```shell
red husk crate inspect <crate>
```

The final report will list:

- the resolved version and features;
- public functions, types, methods, and constants;
- the Husk mapping for each supported item;
- unsupported generics, traits, lifetimes, callbacks, macros, and platform
  requirements;
- required capabilities; and
- whether adapter generation can proceed.

Status: initial implementation complete.

- Cargo metadata resolves the exact package, target, and feature set without
  building the crate.
- crates.io releases use prebuilt Rustdoc JSON from docs.rs, so inspection does
  not execute the crate or its build script.
- Public functions, constructors, associated functions, borrowed methods, and
  resource candidates are discovered without crate-specific rules.
- Each callable is reported as compatible or incompatible with a precise
  reason.
- `--offline`, local paths, and releases without Rustdoc JSON keep the safe
  metadata-only report rather than running Rustdoc locally.

Current classification is intentionally conservative. More Rust/WIT type
mappings and capability inference will expand the accepted surface.

### 5. Generate the adapter

Generate deterministic WIT and Rust glue only for items accepted by the
inspection report. Preserve the report in the generated artifact so no API is
silently omitted.

Status: initial deterministic WIT and Rust glue generation implemented.

```shell
red husk crate interface regex \
  --include regex::Regex::new \
  --include regex::Regex::is_match \
  --include regex::escape
```

- Selection uses exact public API paths from `crate inspect`.
- Incompatible or unknown selections fail with an explicit reason.
- Resource constructors, methods, free functions, and dependent resource
  types are emitted in stable order.
- Generated proposals are covered by the same WIT parser used by the component
  runtime.
- No crate code is compiled during proposal generation.

The selected interface can also be materialized as an unbuilt adapter crate:

```shell
red husk crate adapter regex \
  --output ./regex-adapter
```

The new directory contains pinned Cargo inputs, parser-validated WIT, generated
Rust calls into the selected APIs, and the complete machine-readable selection
report. Generation refuses to overwrite an existing path and does not invoke
Cargo. Current Rust lowering covers primitive and string calls plus fallible
resource construction; additional collection and resource-argument shapes
remain explicit generation errors. By default, every API with complete adapter
lowering is included and every skipped API is recorded with a reason.
`--include <PATH>` remains available as an optional, repeatable filter when a
smaller surface is desired.

### 6. Build and componentize in a sandbox

Build a core module for `wasm32-unknown-unknown` with pinned inputs, bounded
time/output/resources, minimal environment inheritance, and no network unless
explicitly authorized. Componentize it on the trusted host, then verify exports
and actual capability imports.

Status: host-side componentization and verification implemented; the isolated
Cargo runner is next.

`wasm32-wasip2` was rejected for the default path after a real `regex` build
introduced ambient WASI CLI, I/O, environment, process, and random imports.
Building a WIT-aware core module first and componentizing it separately produced
the same selected exports with no capability imports:

```shell
red husk extension componentize \
  --core-module ./target/wasm32-unknown-unknown/release/husk_adapter_regex.wasm \
  --output ./regex.component.wasm
```

Componentization rejects oversized or invalid inputs, refuses to overwrite its
output, validates the encoded component, checks every export through Husk's
runtime descriptor, and rejects all capability imports.

### 7. Implement `husk add`

Make installation transactional and reproducible:

- update `Husk.toml` and `Husk.lock`;
- cache artifacts by digest;
- support `--locked`, `--offline`, and explicit feature selection;
- roll back every partial change after failure.

Status: not started.

## Temporary command location

Until Husk becomes its own project, its CLI is available through Red:

```shell
red husk --help
red husk crate inspect regex
red husk check script.hk
red husk run script.hk
```

The standalone `husk` binary remains available for development.

## Safety boundary

Generated adapters use portable WebAssembly Components by default. Husk will
not load arbitrary Rust libraries into its own process. Crates that require
unsupported native code or capabilities fail with an explicit report.

## Next milestone

Compile a generated adapter to a `wasm32-unknown-unknown` core module inside a
constrained Cargo sandbox. The existing trusted componentization step must then
verify that its exports match the proposal and that it imports no capabilities.
Ordinary Husk commands must still never invoke Cargo.
