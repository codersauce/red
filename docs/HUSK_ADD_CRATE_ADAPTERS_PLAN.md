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

Status: initial cross-platform build sandbox, host-side componentization,
resource limits, and exact export/capability verification implemented.

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

Generated adapter source can now be built explicitly:

```shell
red husk crate build-adapter ./regex-adapter
```

This dedicated command is offline by default. Dependency resolution may use the
network only with `--allow-network`; compilation is always offline and
network-denied. The build runs against a copied source tree with a minimal
environment, bounded time, output, aggregate resident memory, and process
count. Toolchain and registry access is read-only, writes are confined to a
disposable directory, and only the lockfile, verified `component.wasm`, and
verified build status are published back.

The macOS backend uses Seatbelt, Linux uses Bubblewrap and fails closed when
`bwrap` is unavailable, and Windows uses Windows Sandbox with networking
disabled. Windows fails closed when the optional Windows Sandbox feature is
unavailable. Windows Sandbox requires a memory limit of at least 2 GiB.

### 7. Implement `husk add`

Make installation transactional and reproducible:

- update `Husk.toml` and `Husk.lock`;
- vendor and install artifacts by digest;
- support `--locked`, `--offline`, and explicit feature selection;
- roll back every partial change after failure.

Status: initial implementation complete.

```shell
red husk add regex
red husk add glob --features feature-a,feature-b
```

- Crate inspection, automatic or explicit API selection, source generation,
  sandboxed compilation, componentization, and exact verification run as one
  command.
- Verified bundles are committed under `vendor/husk/<sha256>.huskext` and
  installed under the ignored `.husk/extensions/<sha256>.huskext` directory.
- Each bundle keeps `husk-adapter.json`, including every selected and skipped
  API with its reason.
- `Husk.toml` comments are preserved and contain declarative crate adapter
  inputs rather than generated paths. `Husk.lock` records the exact crate,
  features, selected API, package checksum, component digest, installed path,
  and vendored acquisition source.
- Both package files are prepared before publication, validated through the
  normal package resolver afterward, and restored if any publication or
  validation step fails.
- `--offline` uses only Cargo inputs and Rustdoc JSON already present in local
  caches. Online inspection populates the bounded Rustdoc cache.
- `--locked` validates the existing lock and refuses an add that would change
  either package file.
- `--features`, `--no-default-features`, `--version`, and repeatable
  `--include` selections flow through the complete pipeline.

### 8. Create and install packages

Status: initial implementation complete.

```shell
red husk new example
cd example
red husk add glob
red husk install --locked --offline
```

- `husk new <path>` creates `Husk.toml`, `Husk.lock`, `src/main.hk`, and a
  `.gitignore` containing `/.husk/`.
- `husk new . --name <name>` safely initializes an existing directory,
  preserves existing ignore entries, refuses conflicting project files, and
  rolls back files created after a failure.
- `husk install` validates `Husk.toml` against `Husk.lock`, verifies every
  vendored digest and bundle, stages the complete extension set, atomically
  replaces `.husk/extensions`, and prunes stale installations.
- Version 1 installation is local and network-free. A future registry can add
  remote artifact acquisition without changing the manifest, lock, or install
  directory contract.

## Temporary command location

Until Husk becomes its own project, its CLI is available through Red:

```shell
red husk --help
red husk new example
red husk add regex
red husk install --locked
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

Expand conservative Rust-to-WIT lowering so more arbitrary crates are useful:
additional integer widths, collections, records/enums, associated constructors,
owned resource arguments, and capability inference. Add native Linux and
Windows CI smoke tests for their sandbox backends. Ordinary Husk commands must
still never invoke Cargo.
