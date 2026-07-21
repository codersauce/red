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

Status: not started.

### 6. Build in a sandbox

Build for `wasm32-wasip2` with pinned inputs, bounded time/output/resources,
minimal environment inheritance, and no network unless explicitly authorized.
Then verify exports and actual capability imports.

Status: not started.

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

Let users select the compatible API surface, then generate deterministic WIT
for that selection. The milestone is complete when the inspection report for
`regex` can become a reviewable interface containing `Regex::new`,
`Regex::is_match`, and `regex::escape` without compiling the crate.
