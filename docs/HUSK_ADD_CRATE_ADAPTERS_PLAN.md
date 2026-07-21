# `husk add` and crate-adapter plan

## Objective

Make a command such as:

```shell
husk add serde-json
```

install a reproducible, typed Husk adapter powered by a Rust crate, update the
package manifest and lock file, and leave the package immediately usable with
`husk check`, `husk test`, and `husk run`.

The dependency named by `husk add` is a **Husk adapter package**, not an
arbitrary Rust crate. Rust crates have no stable runtime ABI or discoverable
Husk-safe API. An adapter must deliberately select an API, map its types and
errors, and declare its capability boundary.

`serde-json` should be the first end-to-end adapter. It is portable, requires
no host capabilities, and can expose a useful string-oriented API without
trying to represent all of Serde's generic Rust surface.

## Target user experience

From a Husk package:

```shell
husk add serde-json
husk check --locked .
husk run --locked .
```

The add command should:

1. discover the nearest `Husk.toml`;
2. resolve a compatible adapter version from a configured registry;
3. verify and cache a prebuilt `.huskext`, or build a published adapter recipe
   when no compatible artifact is available;
4. inspect the component and verify its declared interface and imports;
5. install the bundle under the package's managed vendor directory;
6. atomically update `Husk.toml` and `Husk.lock`;
7. type-check the package with the installed extension; and
8. roll back all package changes if any step fails.

The resulting Husk API for the first adapter should be deliberately small:

```husk
serde_json::is_valid(input: String) -> bool
serde_json::pretty(input: String) -> Result<String, String>
serde_json::minify(input: String) -> Result<String, String>
```

For example:

```husk
fn main() {
    match serde_json::pretty("{\"answer\":42}") {
        Ok(value) => std::println(value),
        Err(error) => std::println(error),
    }
}
```

## Existing foundation

The repository already provides the execution half of this workflow:

- typed static Rust adapters through `NativeModule`;
- dynamic WebAssembly Component adapters;
- strict `.huskext` bundle validation, inspection, and packing;
- WIT-to-Husk descriptor generation and typed value conversion;
- capability checking using `actual imports ⊆ requested ⊆ granted`;
- local extension paths in `Husk.toml`;
- deterministic local extension entries in `Husk.lock`; and
- loading extensions for `check`, `run`, `test`, and the REPL.

The missing work is dependency resolution, adapter distribution/generation,
controlled Cargo builds, installation, and stronger provenance locking.

## Product and architecture decisions

### Adapter packages, not raw crates

`husk add NAME` resolves a versioned Husk adapter package. The adapter may use
one or more Cargo crates internally. This preserves the explicit boundary from
ADR 0004 and avoids implying that Husk can safely infer an arbitrary crate's
callable API.

The initial implementation should support two authoring paths:

1. **Curated adapter:** fully specified interface and implementation, eligible
   for verified prebuilt components and reproducible source builds.
2. **Generated skeleton:** `husk adapter init` creates WIT, Rust, and extension
   manifests for a developer to complete. It does not claim to automatically
   adapt arbitrary Rust APIs.

Do not use Rustdoc JSON or AI generation as a trusted automatic adapter
contract. They may assist authors later, but generics, traits, lifetimes,
macros, error policy, capabilities, and semantic API selection require an
explicit adapter design.

### Prefer verified artifacts; preserve source builds

Normal `husk add` should prefer a registry-provided `.huskext` whose digest,
adapter source, interface, and provenance are authenticated by registry
metadata. A source build is a fallback or an explicit `--build` mode.

This keeps Cargo, compiler execution, and build scripts out of ordinary
`husk run`. Only installation/build commands may invoke a toolchain.

### Keep the execution trust boundary unchanged

Installed extensions remain subject to existing bundle limits, Wasmtime fuel
and store limits, type validation, and capability checks. Installing an
adapter must not grant capabilities implicitly. Version 1 should accept only
pure components because Husk does not yet provide capability imports.

### Separate package names from module names

The registry package may be named `serde-json`; its exported Husk module is
`serde_json`. Registry metadata must carry both values and reject ambiguous or
colliding normalization.

## Manifest and lock-file design

Add a registry dependency section while retaining local extension paths:

```toml
[dependencies]
serde-json = "1.0"

[extensions.local-math]
path = "vendor/local-math.huskext"
```

Open design question: managed registry dependencies can either be materialized
as generated `[extensions]` entries or remain solely in `[dependencies]` with
their installed paths owned by the resolver. Prefer keeping user intent in
`[dependencies]` and derived local paths only in the lock file.

Introduce a new lock schema that records, for every registry adapter:

- adapter package name and exact version;
- registry identity and source URL or index revision;
- exported module name and interface/WIT digest;
- adapter recipe/source digest;
- resolved Cargo lock digest for source builds;
- target and relevant toolchain compatibility;
- requested and observed capabilities;
- artifact source and SHA-256 digest; and
- final installed/cache location in a relocatable form.

Lock entries must not depend on canonical absolute paths. `--locked` must
reject missing artifacts, digest changes, incompatible interfaces, dependency
resolution changes, or capability drift without rewriting anything.

## Registry contract

Define a small versioned adapter-index record containing at least:

```text
package name and version
Husk version requirement
exported module name and module version
WIT/interface digest
requested capabilities
license and source metadata
adapter recipe/source archive and digest
available target-independent component artifact and digest
Cargo crate provenance and Cargo.lock digest
yanked state
```

The first implementation can use a static HTTP index or local filesystem
registry fixture. The protocol must support:

- deterministic semantic-version resolution;
- checksum verification before unpacking;
- bounded metadata and artifact sizes;
- cache reuse and an offline mode;
- yanked-version behavior compatible with existing lock files;
- configurable registry roots for tests and private deployments; and
- atomic cache population resistant to concurrent installs.

Signing and transparency policy must be decided before treating a public
registry as trusted. Checksums protect integrity after resolution but do not
authenticate a compromised index.

## Adapter source and build contract

A source adapter should have a conventional layout:

```text
serde-json-adapter/
├── adapter.toml
├── Cargo.toml
├── Cargo.lock
├── extension.toml
├── wit/
│   └── world.wit
└── src/
    └── lib.rs
```

`adapter.toml` should bind adapter identity to its Cargo inputs, WIT world,
expected module, and capabilities. Cargo must build with:

```shell
cargo build --locked --release --target wasm32-wasip2
```

The build runner must:

- execute only for explicit installation/build commands;
- use a controlled working directory and environment;
- avoid inheriting secrets unnecessarily;
- bound captured output and execution time;
- support cancellation and clean partial output;
- verify the resulting component instead of trusting the recipe;
- compare actual imports with declared capabilities;
- compare actual exports with the indexed interface digest; and
- surface actionable target/toolchain/build-script errors.

Initially reject adapters whose dependency graph does not compile to
`wasm32-wasip2` or whose component imports require unsupported capabilities.
Native-only adapters remain available to custom Rust embedders through
`NativeModule`; they are not installable into the generic CLI by `husk add`.

## Proposed crate and module changes

Keep network/build policy out of the runtime crates.

- `husk-cli`
  - add `husk add`, `husk remove`, and eventually `husk update`;
  - provide user-facing progress, diagnostics, offline/frozen flags, and
    transaction handling.
- `husk-package`
  - parse dependency requirements;
  - model resolved adapter dependencies;
  - introduce lock schema v2 and locked/frozen verification;
  - perform atomic manifest and lock updates.
- New `husk-registry` crate
  - parse and validate index records;
  - resolve versions and sources;
  - download, verify, and cache metadata/artifacts;
  - contain no compiler or runtime execution.
- New `husk-adapter` crate
  - scaffold adapter projects;
  - validate adapter recipes;
  - orchestrate controlled Cargo builds;
  - inspect output and assemble `.huskext` bundles.
- `husk-extension`
  - extend bundle/provenance metadata only where it belongs in the portable
    artifact contract;
  - retain bounded, symlink-safe validation.
- `husk-wasm` and `husk-runtime`
  - should require little or no product-specific change for pure v1 adapters;
  - remain unaware of registries and Cargo.

Avoid placing HTTP, registry, Cargo, or filesystem mutation policy in the
`husk` facade or runtime engine.

## Phased implementation

### Phase 1: local curated adapter fixture

1. Add a real Rust `serde-json` Component adapter fixture.
2. Define its WIT API and error behavior.
3. Build for `wasm32-wasip2`, pack it, inspect it, and execute it through Husk.
4. Add Linux, macOS, and Windows CI coverage.
5. Record build size and cold/cached call behavior.

Exit criterion: the existing explicit `husk extension` workflow proves the
exact adapter and API that `husk add` will later install.

### Phase 2: local-registry `husk add`

1. Add the dependency manifest model and lock schema v2.
2. Implement a filesystem registry and deterministic resolver.
3. Add an artifact cache and atomic package installation transaction.
4. Implement `husk add serde-json --registry PATH`.
5. Add `--offline`, `--locked`, and `--frozen` semantics.

Exit criterion: an integration test adds the adapter from a temporary local
registry, then checks, tests, and runs the package without network access.

### Phase 3: source recipes and controlled builds

1. Define and validate `adapter.toml`.
2. Add `husk adapter init` scaffolding.
3. Add a bounded Cargo build runner and `--build` behavior.
4. Verify source, Cargo lock, WIT, imports, exports, and output component.
5. Prove clean failure and rollback for missing targets, build failures,
   malicious paths, oversized output, timeouts, and cancellation.

Exit criterion: deleting the prebuilt artifact allows the published recipe to
reproduce an accepted extension with matching interface and capability policy.

### Phase 4: authenticated remote registry

1. Finalize registry transport, authentication, signing, and mirroring policy.
2. Add HTTPS fetching with strict size and redirect limits.
3. Add cache concurrency, retries, resumability where safe, and redacted
   diagnostics.
4. Implement yanks and locked historical resolution.
5. Add `husk remove`, `husk update`, and dependency-conflict diagnostics.

Exit criterion: installs are reproducible, authenticated, offline-capable after
the first fetch, and safe under interruption or concurrent commands.

## Testing strategy

### Resolver and manifest tests

- exact, compatible, conflicting, missing, and yanked versions;
- package/module-name normalization and collisions;
- malformed, duplicated, unknown, and oversized metadata;
- deterministic resolution independent of index iteration order;
- schema migration and strict `--locked` behavior.

### Supply-chain and filesystem tests

- incorrect metadata, source, interface, and artifact digests;
- traversal, absolute paths, symlinks, and archive expansion attacks;
- interrupted and concurrent cache writes;
- package mutation rollback after every failure point;
- offline cache hits and misses;
- secrets absent from subprocess environments and diagnostics.

### Adapter/build tests

- successful prebuilt and source-built `serde-json` installation;
- missing Cargo or target, compiler failure, timeout, and cancellation;
- WIT/export mismatch;
- undeclared, denied, and unsupported imports;
- unsupported WIT types and normalization collisions;
- Cargo dependency or lock drift.

### End-to-end tests

- start with a minimal Husk package;
- add `serde-json`;
- call every exported function, including malformed JSON errors;
- run `check`, `test`, and `run` with and without `--locked`;
- copy the project and cache to another path and verify portability;
- repeat fully offline;
- remove and re-add without leaving stale manifest, lock, or vendor state.

## Explicit non-goals for the first release

- Automatically exposing every public function from any crates.io crate.
- Loading Rust `rlib`, `dylib`, or compiler-specific Rust symbols at runtime.
- Native in-process installation into the generic standalone CLI.
- Implicit filesystem, network, environment, process, clock, or random access.
- Executing Cargo during `husk check`, `husk test`, or `husk run`.
- Supporting crates that cannot compile as pure `wasm32-wasip2` components.
- Solving a general dynamic JSON object model as part of the first adapter;
  `serde-json` should initially exchange JSON text as `String`.

## Open decisions before implementation

1. Does the first milestone distribute only verified prebuilt components, or
   also enable local source builds immediately?
2. What authenticates the adapter index and its release metadata?
3. Does `Husk.toml` use `[dependencies]`, `[adapters]`, or an extended
   `[extensions]` source model?
4. Where is the managed cache located, and which artifacts are vendored into a
   project for source-control portability?
5. Which Rust toolchain identity is required for a reproducible source build?
6. Are adapter recipes allowed to use Cargo build scripts in the first release?
7. What compatibility promise applies to a module's WIT/interface digest across
   semver-compatible adapter updates?

Recommended initial choices: prebuilt-first with explicit `--build`,
`[dependencies]` for user intent, a local filesystem registry fixture before
HTTP, no capabilities, and `serde-json` strings-only API as the guinea pig.

## Definition of done for the MVP

- `husk add serde-json` resolves from a local test registry without manual file
  editing.
- The installed artifact and all relevant provenance are locked and verified.
- The package works with `husk check --locked`, `husk test --locked`, and
  `husk run --locked`.
- Repeating the command is idempotent.
- Offline use works after installation.
- Every failed install leaves the original manifest, lock file, cache-visible
  state, and package behavior intact.
- No ordinary execution command invokes Cargo or accesses the network.
- Documentation explains that adapters expose curated APIs rather than raw
  Rust crates.
