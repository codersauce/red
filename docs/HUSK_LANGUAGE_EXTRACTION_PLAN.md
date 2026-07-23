# Husk language extraction and extension architecture

Status: implementation blueprint with an end-to-end implementation checkpoint

Research date: 2026-07-19

Scope: the Husk implementation and Red integration in this repository and branch

Implementation checkpoint: 2026-07-19. See
[HUSK_IMPLEMENTATION_STATUS.md](HUSK_IMPLEMENTATION_STATUS.md) for the
card-by-card result, remaining release blockers, and exact next-agent work. See
[HUSK_LANGUAGE_GUIDE.md](HUSK_LANGUAGE_GUIDE.md) for the implemented embedding,
CLI, package, and extension interfaces.

## How to use this document

This is both an architecture decision and an execution checklist. It is written
so that an implementation agent can take one task card at a time without having
to redesign the system.

An implementation agent must follow these rules:

1. Treat this repository and branch as the only source-code baseline. Do not
   copy an implementation or architecture from another Husk repository.
2. Complete task cards in dependency order. Do not combine unrelated task cards
   into one change.
3. Preserve Red's working plugin behavior until a task explicitly replaces it.
   Add the replacement and its tests before deleting a compatibility path.
4. Keep one source of truth for every callable module signature. Never add a
   second hand-written declaration string beside a Rust implementation.
5. Do not make `cargo`, `rustc`, a network registry, or a system linker part of
   normal script startup. Compilation of an extension is an explicit developer
   or installation action.
6. Do not expose Rust-layout types, Rust trait objects, `String`, `Vec`, or
   unwinding Rust functions across a native dynamic-library boundary.
7. Run the targeted tests named on each card. Before considering a Rust phase
   complete, also run:

   ```shell
   cargo fmt --all -- --check
   cargo test --workspace --all-targets --all-features
   cargo clippy --all-targets --all-features -- -D warnings
   ```

8. If a task requires a semantic choice not settled here, stop and add a small
   ADR under `docs/adr/` before implementing it. Do not silently invent a new
   language rule.

This document preserves the target design and task-card acceptance criteria.
Implementation state is tracked separately so partial cards are not made to
look complete by a single checkbox.

## Executive decision

Husk should become one language implementation with two front doors:

- a Rust embedding API built around `Engine`, `CompiledModule`, and `Instance`;
- a `husk` executable that uses exactly that API for `run`, `check`, `repl`, and
  eventually `test`.

Rust crate reuse should have two supported tiers and one optional tier:

1. **Static native modules are the primary embedding mechanism.** A Rust
   application adds any desired crate to its normal Cargo dependency graph,
   writes a small typed Husk adapter, and registers that module with the
   `Engine`. This is trusted, fast, and avoids an ABI boundary.
2. **WebAssembly Components are the primary dynamic mechanism.** A crate author
   writes a small component adapter with a WIT interface, compiles that adapter
   and its Cargo dependencies to `wasm32-wasip2`, and distributes a
   `.huskext` bundle. Husk inspects the component's actual type information and
   calls its exports with Wasmtime's dynamic Component Model API.
3. **A native `cdylib` adapter may be added later for trusted, native-only
   crates.** It must use a tiny, versioned C ABI and byte buffers. It is not a
   Rust `dylib`, it is never enabled for untrusted code, and libraries remain
   loaded for the life of the process. This tier is gated behind a spike and is
   not required for Husk 1.0.

There is no sound, general operation that means "load an arbitrary Rust crate
at runtime and call its public API." Cargo crates form a compile-time dependency
graph, do not carry a uniform callable interface, can expose generics, traits,
macros, and target-specific code, and use an unstable Rust ABI. Every runtime
option therefore needs an adapter that selects functions and defines portable
argument and result types.

The runtime should initially remain an interpreter. It should first interpret a
resolved, runtime-neutral HIR; bytecode or JIT work should happen only after
profiling. Rewriting the execution engine and extracting it from Red at the same
time would make regressions unnecessarily hard to isolate.

## Goals

- Make all `crates/husk*` code independent of Red concepts.
- Use one parse, resolution, and type-check pipeline for embedded and standalone
  execution.
- Make the compiler's accepted runtime language match the interpreter's
  behavior.
- Preserve source-aware compile diagnostics and add source-aware runtime stack
  traces.
- Give embedders a typed, versioned way to expose Rust functions and types.
- Let standalone Husk load portable, sandboxed extensions built from compatible
  Rust crates.
- Keep extension capabilities deny-by-default and enforce CPU, memory, call
  depth, and host-call limits.
- Preserve bundled Red plugins, transactional reload, quarantine behavior,
  command metadata, event listeners, pending requests, and persistent plugin
  state through the migration.
- Make the Husk crates independently testable, packageable, and publishable
  before considering a physical repository split.

## Non-goals for the first stable standalone release

- Automatically reflecting an arbitrary crate's Rust API.
- Downloading crates or running Cargo when a script is imported or executed.
- A crates.io-like Husk registry or network package manager.
- A stable native Rust ABI.
- Loading untrusted native shared libraries.
- Unloading or replacing a native shared library in place.
- Async/await syntax or a resumable async VM. Callback-based host requests can
  continue to work.
- A bytecode compiler, JIT, or ahead-of-time Husk compiler.
- WIT resources, futures, streams, or arbitrary WASI access in the first
  extension ABI.
- Executing `js { ... }` in the native runtime. JavaScript-specific syntax may
  remain parseable in an explicitly selected legacy profile, but it is not part
  of native Husk.
- A formatter or language server rewrite. Existing frontend/editor support
  should be preserved and adapted after runtime semantics stabilize.

## Current-state audit

This section records the starting state that motivated the extraction. It is
intentionally historical; do not use it as the current support matrix. The
current result is summarized in
[HUSK_IMPLEMENTATION_STATUS.md](HUSK_IMPLEMENTATION_STATUS.md).

### Repository components

| Area | Current responsibility | Extraction problem |
| --- | --- | --- |
| `crates/husk-lexer` | Tokenization | Mostly reusable. |
| `crates/husk-ast` | A broad Rust-like AST | Contains JavaScript-specific nodes and comments as well as backend-neutral nodes. |
| `crates/husk-parser` | Parses functions, types, traits, impls, patterns, closures, and JavaScript interop | Far more capable than the production interpreter. |
| `crates/husk-types` | Primitive and inferred type representations | Small and reusable, but not yet a public module-signature model. |
| `crates/husk-semantic` | Resolution and type checking | Its default prelude and several inference paths assume JavaScript and `JsValue`. |
| `crates/husk-diagnostics` | Source files, diagnostics, and reports | Reusable foundation for compiler and runtime errors. |
| `crates/husk` | `Program`, `Vm`, `Value`, callbacks, host calls, and Red plugin lifecycle | Claims to be embedded Husk, but owns commands, events, pending Red requests, plugin state, plugin names, and a large `red::*` dispatch. |
| `src/plugin/runtime.rs` | Red host implementation and validation | Duplicates parsing/type checking, supplies a hand-written host declaration string, and wraps a plugin-shaped VM. |
| `src/plugin/registry.rs` | Plugin discovery, metadata, dependency order, reload, and quarantine | Correctly belongs to Red and must remain outside the general language runtime. |

The existing production contract is described in
[PLUGIN_SYSTEM.md](PLUGIN_SYSTEM.md) and [PLUGIN_API.md](PLUGIN_API.md). Those
documents are the compatibility baseline for Red behavior.

### Compiler/runtime mismatch

`Program::parse_at` currently retains only top-level functions. Structs, enums,
type aliases, traits, impls, use items, and extern declarations are discarded.
The VM interprets only a subset of statements and expressions.

| Language area | Parser/semantic support | Production VM status |
| --- | --- | --- |
| Functions and ordinary calls | Yes | Partial; functions are looked up by strings. |
| `let`, assignment, blocks, conditionals, loops | Yes | Mostly supported; patterns are restricted. |
| Structs and field access | Yes | Struct identity and definitions are discarded; literals become generic objects. |
| Enums and pattern matching | Yes | Not executed. |
| Traits, inherent impls, and method calls | Yes | Not executed. |
| Closures | Yes | Not executed; current callbacks are plugin/function name pairs. |
| Tuples and tuple fields | Yes | Not executed. |
| Ranges | Yes | Not executed as language values; Red has helper functions instead. |
| Casts and `?` | Yes | Not executed. |
| `if let`, `let ... else`, destructuring | Yes | Not executed. |
| Formatting expressions | Yes | Not executed by the general VM. |
| `use` | Partial semantic support | No runtime module graph. |
| JavaScript externs and literals | Yes | Native VM cannot execute them. |

This mismatch is the largest correctness risk. A standalone language must never
successfully type-check a construct and then fail with an internal
"unsupported expression" error. The end state is: every backend-neutral AST
construct either lowers to HIR and executes, or is rejected during compilation
with a stable, source-labeled diagnostic.

### Coupling that must move to Red

The following are application policy, not language runtime behavior:

- plugin IDs and a multi-plugin `Vm`;
- activation/deactivation lifecycle;
- command registration and `CommandMetadata`;
- event listener ownership;
- `RequestId` and pending editor requests;
- Red plugin state and state migration;
- all `red::*` dispatch;
- Red host API version checks;
- plugin dependency ordering, package metadata, quarantine, and reload.

The general runtime needs only modules, functions, values, instances, limits,
and opaque host state.

## Research conclusion: what dynamic loading can and cannot do

### Why a raw Cargo crate cannot be loaded

Rust's compiler can produce a Rust `dylib`, but Rust's own `cdylib` RFC calls
out the unstable Rust ABI and the rarity of Rust dynamic libraries. Rust type
layout can change on every compilation, and closures have no layout guarantee.
Cargo artifacts such as `rlib` files are compiler inputs, not runtime plugin
packages. A package can also be a proc macro, contain only generic functions,
require a build script, depend on native libraries, or expose no C-callable
symbols.

`libloading` can locate a library and an explicitly named symbol. It cannot
discover a crate's high-level Rust API, validate a Rust type signature, or make
an ABI stable. Its symbol lookup is necessarily unsafe because the host asserts
the symbol's function-pointer type.

Therefore, the user-facing promise must be:

> A Rust crate can power a Husk module after a small adapter selects an API and
> maps it to Husk/WIT types.

It must not be:

> Any crate name can be imported directly from a Husk script.

### Evaluated approaches

| Approach | Arbitrary crate compatibility | Safety/isolation | Portability | Runtime overhead | Decision |
| --- | --- | --- | --- | --- | --- |
| Static Cargo dependency plus native module | Highest; any crate compatible with the embedding application | Same trust as host process | Rebuild per host target | Lowest | **Required and first** |
| Rust `dylib` plus Rust symbols | Low across toolchains and dependency graphs | In-process, ABI-unsound if versions differ | Target-specific | Low | **Reject** |
| `cdylib` plus owned C ABI | High after writing an adapter | Trusted in-process only | Build per target/ABI | Low to medium | **Optional gated tier** |
| `abi_stable` plugin | High after writing an adapter | Trusted in-process; load-time layout checks; no unloading | Build per target | Low to medium | Evaluate during the native spike, not the foundation |
| Core Wasm with bytes-in/bytes-out, Extism-style | Crates that compile to the Wasm target | Sandboxed | Portable | Serialization on every call | Viable fallback, not preferred typed ABI |
| WebAssembly Component plus WIT | Crates that compile to `wasm32-wasip2` | Sandboxed with explicit imports | Portable | Boundary conversion and Wasm execution | **Required dynamic tier** |
| Native helper process plus IPC | Nearly any native crate after an adapter | Process isolation | Build per target | Highest; lifecycle complexity | Future fallback for untrusted native-only needs |
| Invoke Cargo and a linker during `husk run` | Source crates only; requires a complete toolchain | Executes build scripts and compiler processes | Poor startup/reproducibility | Extremely high cold start | **Reject for normal execution** |

WebAssembly Components are preferable to an untyped bytes ABI because WIT
describes machine-readable imports and exports, and the Component Model defines
a canonical ABI. Wasmtime exposes both the component's dynamic type information
and a dynamic `Val` representation, so Husk can inspect an extension unknown at
host compile time without inventing a recursive serialization protocol.

Extism demonstrates that bytes-in/bytes-out Wasm plugins are practical and is a
useful fallback precedent. Husk should nevertheless own its language type
mapping and use the Component Model directly unless the spike uncovers a
blocking issue.

## Target architecture

```text
                         module descriptors
                  +-----------------------------+
                  | built-in and native modules |
                  | Wasm component modules      |
                  +-------------+---------------+
                                |
Husk sources -> module resolver -> compiler -> CompiledModule
                                      |              |
                                      |              v
                                      |         Instance<HostState>
                                      |              |
                          diagnostics/source map     +--> owned results
                                                     +--> callback handles

Rust application ---> Engine builder ----------------------^
`husk` CLI ---------> Engine builder ----------------------^
Red plugin adapter -> Engine builder with `red` module ----^
```

The compiler and interpreter are shared. The CLI is an embedder, not a separate
runtime. Red is another embedder with a substantial native module and plugin
manager.

### Target crate layout

Keep the existing frontend crates. Add boundaries only when the code behind
them exists; do not create empty placeholder crates.

```text
crates/
  husk/                  Stable public facade and embedding API re-exports
  husk-ast/              Existing AST, made backend-neutral where possible
  husk-diagnostics/      Existing source-aware reports
  husk-hir/              Resolved, typed, runtime-neutral executable IR
  husk-lexer/            Existing lexer
  husk-parser/           Existing parser
  husk-runtime/          Compiler orchestration, Engine, Instance, heap, modules
  husk-semantic/         Existing resolver/type checker, made profile-aware
  husk-stdlib/           Neutral prelude descriptors and native implementations
  husk-types/            Core and public signature types
  husk-extension/        `.huskext` manifest/bundle types; no Wasmtime dependency
  husk-wasm/             Optional Wasmtime Component loader and module provider
  husk-cli/              Binary package; executable name `husk`
```

Dependency rules:

- frontend, type, HIR, and diagnostic crates must not depend on runtime, CLI,
  Wasmtime, or Red;
- `husk-runtime` must not depend on Red or `husk-cli`;
- `husk-wasm` may depend on `husk-runtime` and `husk-extension`;
- the `husk` facade may re-export `husk-wasm` behind a
  `wasm-extensions` feature;
- Red initially depends on `husk` without Wasmtime;
- `husk-cli` may enable `wasm-extensions` by default;
- `cargo tree -p husk-cli` must never contain the `red` package.

`husk-runtime` may temporarily contain the compiler orchestration and
interpreter in one crate. Split a separate compiler crate only if a real
dependency cycle or compile-time problem appears.

## Public embedding model

The intended API shape is:

```rust
use husk::{Engine, NativeModule};

#[derive(Default)]
struct AppState {
    calls: usize,
}

let text = NativeModule::<AppState>::builder("text")
    .typed_function("slugify", |ctx, input: String| {
        ctx.data_mut().calls += 1;
        Ok(input.to_lowercase().replace(' ', "-"))
    })
    .build()?;

let engine = Engine::<AppState>::builder()
    .register_module(text)?
    .limits(Default::default())
    .build()?;

let compiled = engine.compile_path("script.hk")?;
let mut instance = engine.instantiate(compiled, AppState::default())?;
let result = instance.call("main", &[])?;
```

Names may change slightly during implementation, but these ownership
boundaries must remain:

- `Engine<T>` is immutable after construction and safe to share.
- `CompiledModule` owns the AST/HIR, semantic metadata, and source map; it has
  no mutable script state.
- `Instance<T>` owns globals, heap, call stack, instruction counters, persistent
  callback roots, extension instances, and embedder state `T`.
- An `Instance` executes one script/package or one Red plugin. It is
  single-threaded and not `Sync`. It may be `Send` when its host state and
  modules permit it.
- No callback or heap value can be invoked without the `Instance` that owns it.

### One module descriptor, two consumers

Every native, built-in, and dynamic extension module supplies one
`ModuleDescriptor`:

```rust
pub struct ModuleDescriptor {
    pub name: ModuleName,
    pub version: Version,
    pub types: Vec<TypeDescriptor>,
    pub functions: Vec<FunctionDescriptor>,
    pub interfaces: Vec<InterfaceDescriptor>,
    pub documentation: Option<String>,
}

pub struct InterfaceDescriptor {
    pub name: String,
    pub types: Vec<TypeDescriptor>,
    pub functions: Vec<FunctionDescriptor>,
    pub documentation: Option<String>,
}

pub struct FunctionDescriptor {
    pub name: String,
    pub parameters: Vec<ParameterDescriptor>,
    pub result: TypeDescriptor,
    pub documentation: Option<String>,
}
```

The semantic analyzer uses the descriptor to resolve and check calls. The
runtime uses the same descriptor to validate dispatch and conversions.
Initially, a descriptor may be converted to a declaration AST to minimize
semantic changes. The end state is direct descriptor ingestion; no generated
source is parsed and no `RED_HOST_DECLARATIONS` equivalent exists.

Module construction rejects duplicate names, invalid identifiers, unsupported
types, normalized interface/function collisions, and conflicting versions
before any script is compiled. Root functions use `module::function`. A
WIT-style interface with the same name as its module also exposes its functions
as `module::function`; its repeated interface name is an internal Component
detail, not a public Husk namespace. Distinct WIT-style interfaces continue to
use `module::interface::function`.

### Typed Rust adapters

Implement these public conversion traits:

```rust
pub trait HuskType {
    fn husk_type() -> TypeDescriptor;
}

pub trait FromHusk<'a>: HuskType + Sized {
    fn from_husk(value: ValueRef<'a>) -> Result<Self, ConversionError>;
}

pub trait IntoHusk: HuskType {
    fn into_husk(self, cx: &mut CallContext<'_, '_>)
        -> Result<RuntimeValue, ConversionError>;
}
```

The initial supported native adapter types are `()`, `bool`, `i32`, `i64`,
`f64`, `String`, `&str` for borrowed input, `Vec<T>`, tuples, `Option<T>`, and
a dedicated `ScriptResult<T, E>`. Callback parameters arrive as a borrowed
`FunctionRef<'call>`; `CallContext::persist_function` deliberately converts one
to a `FunctionHandle` when the host needs to retain it. A Rust
`Result<T, NativeError>` represents a host-call failure; it must not be
confused with a Husk `Result<T, E>` value.

Write the builder and manual traits first. A derive/procedural-macro crate for
records and enums is a later ergonomics improvement, not a prerequisite.

### Runtime and host values

Do not let embedders retain an unrooted internal heap reference.

- `RuntimeValue` is instance-bound and internal to evaluation.
- `OwnedValue` is a detached, owned representation for ordinary host
  arguments/results: unit, bool, integers, float, string, bytes, list, tuple,
  record, and variant.
- `FunctionHandle` is an opaque, instance-owned persistent root used for
  callbacks. It includes an instance generation and root ID. It cannot be
  serialized or called through another instance.
- `FunctionRef<'call>` is valid only during one native call. Retaining it
  requires the explicit persistent-root operation above.
- Native module functions normally use typed conversions rather than matching
  raw values.

Red must replace today's `{ plugin, function }` callback strings with
`FunctionHandle`. The Red adapter explicitly releases handles during command,
listener, request, plugin, and instance cleanup.

### Migration of the current dynamic `Value`

Red plugins rely heavily on dynamic JSON-shaped editor payloads, `red::null()`,
missing-field behavior, object/array indexing, and state that can contain
arbitrary JSON-representable values. Removing `JsValue` from native semantic
analysis must not remove this contract accidentally.

Add a neutral built-in `Json` type with null, bool, number, string, array, and
object cases. It is the explicit dynamic boundary:

- a native function parameter of `Json` accepts only a Husk value that can be
  converted to the JSON data model;
- a native function returning `Json` produces a dynamic value with field and
  index access;
- `Json { key: value }` remains the object-construction spelling used by
  existing Red plugins;
- a missing JSON field retains its originating field/span for a useful chained
  access error and compares as JSON null for Red compatibility;
- safe new code can use `std::json::get` and receive `Option<Json>`;
- unit is distinct from JSON null in new native semantics.

Do not make all native Husk values dynamically typed merely to preserve this
host boundary.

| Current `Value` case | Transitional handling | End-state representation |
| --- | --- | --- |
| `Unit`, `Bool`, `Int`, `Float`, `String` | Preserve behavior | Typed HIR primitive; distinguish `i32` from `i64` using resolved type |
| `Null` | Preserve for Red compatibility | `Json::Null`, not a second general null type |
| `Array` | Preserve until the heap lands | Heap array with normal Husk type |
| `Object` | Preserve until nominal structs land | Nominal struct/record or `Json::Object`, never an identity-free mixture |
| `Json` | Preserve host conversions | Heap-backed neutral `Json` value |
| `Callback` | Compatibility wrapper | Instance-owned `FunctionHandle` |
| `Missing` | Keep internal diagnostic behavior | Internal JSON lookup sentinel; never a public storable type |

P2 must introduce this neutral `Json` contract before Red moves from the legacy
semantic profile. P3's Red module descriptor then uses `Json`, typed function
parameters, and generic/container signatures instead of `JsValue`.

### Limits and cancellation

`Limits` must cover at least:

- instructions per top-level call;
- maximum call depth;
- heap bytes and object count;
- maximum string/container length;
- maximum source and module count;
- native host calls per top-level call;
- persistent callback roots;
- extension instances;
- optional cancellation/deadline checks.

Every loop back-edge, function call, allocation, and host/extension call must
charge the relevant counter. Exhaustion produces a catchable host error and a
source-aware Husk runtime diagnostic, never a process panic.

## Native language profile and standard library

The current semantic prelude mixes language contracts with JavaScript
implementation details. Introduce an explicit profile:

```rust
pub enum SemanticProfile {
    Native,
    LegacyJavaScript,
}
```

`Native` is the default for `Engine` and CLI compilation. Its prelude contains
only backend-neutral Husk definitions. Operations such as string methods,
array methods, formatting, JSON, iteration, assertions, and printing are
implemented by HIR/interpreter primitives or registered `std` native modules.

`LegacyJavaScript` may continue to load `JsValue`, `extern "js"`, and
JavaScript globals while old frontend tests require it. Native compilation of
`js { ... }` emits a specific compile error explaining that the expression is
only valid in the legacy JavaScript profile.

Do not overload `extern "js"` to mean a native or Wasm module. User scripts
import registered modules with `use`; module descriptors and WIT supply the
foreign signatures.

### Semantic rules to freeze before HIR

P0's value-semantics ADR must turn these into executable conformance tests.
Unless a bundled-plugin test demonstrates a required exception, use the
following native rules:

- Evaluate call arguments and ordinary binary operands left-to-right.
- Conditions and `!` require `bool`; there is no general truthiness.
- `&&` and `||` short-circuit.
- Blocks create lexical scopes. A binding is not visible after its block.
- `let mut` is required for rebinding and mutation through a local.
- Integer HIR operations stay in their resolved `i32` or `i64` type, use
  checked arithmetic, and report overflow. Never implement integer arithmetic
  by round-tripping through `f64`.
- Integer division/remainder by zero and invalid casts are runtime errors with
  the operator/cast span. Floating-point operations use IEEE `f64`; comparisons
  with NaN follow one documented rule rather than host-language accidents.
- String concatenation with a non-string is resolved through a neutral
  `ToString`/display contract so existing Red expressions remain valid.
- Arrays, tuples, structs, and enums retain the current observable value/copy-
  on-write behavior unless the ADR explicitly versions a change to reference
  semantics. Internal Rust handle cloning must not accidentally change language
  assignment semantics.
- Equality is structural for ordinary data and must terminate on cycles.
  Functions and host resources use identity equality or are rejected as
  non-comparable during compilation.
- Ordinary array, tuple, and string indexing outside the valid range is a
  source-aware runtime error in new native code. Red conversion helpers may
  continue to supply fallback values for compatibility.
- String indexing and iteration operate on Unicode scalar values initially,
  matching the current VM. A future grapheme API is separate.
- JSON missing/null behavior is the explicit compatibility contract in the
  preceding section and does not leak into nominal struct field access.

Where one of these differs from a tested production plugin dependency, keep the
legacy behavior in the Red adapter or an explicit compatibility profile and
record the divergence. Do not give embedded and CLI native Husk different
semantics.

## Standalone scripts and packages

### Initial entry-point contract

The first `husk run` milestone is deliberately one-file and explicit:

```husk
fn main(args: [String]) -> Result<(), String> {
    // ...
}
```

Also accept `fn main()`, and allow a result of `()`, `i32`, or
`Result<(), E>`. An `i32` becomes the process exit status after validation.
An `Err` is rendered as a runtime failure. Reject other `main` signatures at
compile time.

Strip a UTF-8 shebang only when it is the first line. Pass arguments after `--`
without reinterpretation. Top-level executable statements can be added later
as a parser feature that lowers to a synthetic `main`; do not implement them by
textually wrapping source and corrupting spans.

### Multi-file module rules

Add explicit Rust-like source modules:

```husk
mod util;
use crate::util::slugify;
use regex::api::is_match;
```

For a module declared as `mod util;`, resolve exactly one of:

- `<parent>/util.hk`;
- `<parent>/util/mod.hk`.

Report ambiguity if both exist. Canonicalize paths, keep every source under the
package source root, reject duplicate canonical files, and report import cycles
with the cycle chain. Enforce `pub` across module boundaries. Registered
native/Wasm modules occupy external root names; `std` is reserved.

The single-file CLI does not require a manifest. A multi-file package uses:

```toml
# Husk.toml
[package]
name = "example"
version = "0.1.0"
entry = "src/main.hk"

[extensions.regex]
path = "vendor/regex.huskext"
```

Version 1 resolves only local paths. `Husk.lock` records the canonical extension
identity, version, component digest, and source. No registry or network
resolution is part of version 1.

## Rust crate extensions

### Static native module: preferred for Rust embedders

An embedding application uses an ordinary Cargo dependency:

```toml
[dependencies]
husk = "0.1"
regex = "1"
```

It then registers a narrow adapter:

```rust
let regex = NativeModule::<AppState>::builder("regex")
    .typed_function(
        "is_match",
        |_cx, (pattern, input): (String, String)| {
            let value = regex::Regex::new(&pattern)
                .map(|regex| regex.is_match(&input))
                .map_err(|error| error.to_string());
            Ok(husk::ScriptResult::from(value))
        },
    )
    .build()?;
```

This tier can use crates with native libraries, OS APIs, threads, or types that
cannot compile to Wasm. It is code chosen and compiled by the host application,
so it has the host's trust and capabilities.

### Dynamic WebAssembly Component: preferred for standalone Husk

An extension author creates a small adapter crate. Its WIT can be:

```wit
package example:regex@0.1.0;

interface api {
    is-match: func(
        pattern: string,
        input: string,
    ) -> result<bool, string>;
}

world husk-extension {
    export api;
}
```

The Rust adapter uses `wit-bindgen`, depends on `regex`, exports the WIT world,
and builds explicitly:

```shell
rustup target add wasm32-wasip2
cargo build --locked --release --target wasm32-wasip2
husk extension pack --manifest extension.toml \
  --component target/wasm32-wasip2/release/regex_adapter.wasm \
  --output dist/regex.huskext
```

The current Component Model documentation recommends native Rust
`wasm32-wasip2` tooling rather than the deprecated `cargo-component` workflow.
Only crates that compile for this target and use granted imports can be in the
component's dependency graph.

Version 1 uses a directory bundle, which is easier to inspect and update
atomically than a custom archive:

```text
regex.huskext/
  extension.toml
  component.wasm
  wit/                    # optional, for humans and generated docs
    world.wit
  LICENSES/               # optional distribution metadata
```

Example manifest:

```toml
schema_version = 1
name = "regex"
version = "0.1.0"
module = "regex"
artifact = "component.wasm"
world = "example:regex/husk-extension@0.1.0"
minimum_husk = "0.1.0"

[capabilities]
requested = []
```

The manifest is descriptive, not authoritative about code imports. The loader
must inspect the component and compare its actual imports to both requested and
host-granted capabilities.

#### Load sequence

The implementation must perform these steps in order:

1. Resolve and canonicalize the bundle and artifact paths.
2. Parse the manifest with unknown-field rejection for its schema version.
3. Enforce maximum manifest and component sizes.
4. Hash the component and compare it to `Husk.lock`, when a lock exists.
5. Compile a Wasmtime `Component`.
6. Inspect `Component::component_type()` and enumerate actual imports/exports.
7. Reject unsupported WIT types and name-normalization collisions.
8. Convert exports into the one `ModuleDescriptor` used by Husk compilation.
9. Compare actual imports to the embedder's capability grants.
10. Instantiate one component instance per Husk `Instance`, with resource
    limits and fuel.
11. On a call, convert typed `RuntimeValue` arguments to
    `wasmtime::component::Val`, invoke the dynamic component function, convert
    results back, and attach extension/module/function context to any trap.

Compile a Wasmtime `Component` once per `Engine` and share it. Keep its mutable
`Store` and component `Instance` per Husk `Instance`.

#### Initial WIT/Husk mapping

| WIT | Husk | Status |
| --- | --- | --- |
| `bool` | `bool` | Required |
| `s32` | `i32` | Required |
| `s64` | `i64` | Required |
| `float64` | `f64` | Required |
| `string` | `String` | Required |
| `list<T>` | `[T]` | Required when `T` is supported |
| `tuple<...>` | Husk tuple | Required |
| `option<T>` | `Option<T>` | Required |
| `result<T, E>` | `Result<T, E>` | Required |
| WIT record | Generated nominal external struct | Required |
| WIT enum/variant | Generated nominal external enum | Required |
| `u8` and `list<u8>` | `i32` with checked conversion; later `Bytes` optimization | Allowed only after explicit range tests |
| `s8`, `s16`, `u16`, `float32`, `char`, flags | No implicit lossy mapping | Reject in version 1 |
| `u32`, `u64` | No current exact Husk primitive | Reject in version 1 |
| resource, future, stream, map, error-context | No version 1 runtime contract | Reject in version 1 |

Normalize WIT kebab-case function names to Husk snake_case. Reject a component
if two exports normalize to the same Husk name. Preserve original WIT names in
the dispatch descriptor.

Do not invent JSON serialization for this typed path. If dynamic component
introspection proves unworkable during the spike, the documented fallback is a
versioned bytes-in/bytes-out ABI with an explicit wire schema, not an ad hoc
`serde_json::Value`.

#### Capabilities and isolation

Wasm extensions receive no filesystem, network, environment, clock, random,
process, or editor access merely because a manifest asks for it.

- Actual component imports are the lower bound of requested capabilities.
- A package manifest requests capabilities.
- The embedding application or CLI invocation grants capabilities.
- Instantiation succeeds only when `actual imports ⊆ requested ⊆ granted`.
- Red should initially grant no dynamic extension capabilities and should keep
  Wasm extensions disabled unless explicitly configured.

Use Wasmtime fuel for deterministic CPU limits and a store resource limiter for
memory, table, and instance limits. Epoch interruption may add a wall-clock
deadline, but it must not replace deterministic Husk and Wasm fuel accounting.
Do not link the broad WASI world by default; add only approved interfaces.

### Optional trusted native `cdylib`

This tier exists only for standalone use of crates that cannot compile to Wasm.
It is not a way to make native code safe.

The adapter is a `cdylib` with one exported C symbol:

```c
const struct HuskExtensionApiV1 *husk_extension_entry_v1(void);
```

The returned `#[repr(C)]` table contains:

- ABI major, minor, and table byte size;
- a metadata function returning a bounded byte buffer;
- an `invoke` function taking a versioned request byte slice and returning a
  bounded response byte buffer;
- a function that frees buffers allocated by the extension;
- no Rust references, Rust enums, Rust trait objects, `String`, `Vec`, or
  unwinding ABI.

The loader uses `libloading`, validates the table before the first call, copies
all returned bytes before calling the extension's free function, and keeps an
`Arc<Library>` beside every function pointer. The adapter catches Rust panics
inside its exported function when built with unwinding; a `panic=abort` adapter
can still terminate the process and must be documented as such.

Never unload a library. A development reload copies a new artifact to a unique
versioned filename, loads it, routes new instances to it, and leaves the old
library resident until process exit. Validate target triple, pointer width,
endianness, ABI major version, descriptor schema, artifact size, and digest.

`abi_stable` can be compared in the spike because it supplies FFI-safe wrappers
and load-time layout checks, but its own documented plugin model does not
support unloading. It must not cause the public Husk ABI to expose arbitrary
Rust crate types.

## Runtime-neutral HIR and heap

The semantic analyzer currently returns lookup maps intended partly for code
generation. Add `husk-hir` after extraction behavior is stable.

HIR requirements:

- every module, type, function, local, field, and variant has a stable ID;
- every expression carries its resolved type and original source span;
- local calls, native module calls, Wasm calls, constructors, and method calls
  are distinct HIR operations, not strings interpreted at runtime;
- generics and traits are resolved to concrete static dispatch before
  execution;
- JavaScript literals cannot enter native HIR;
- HIR retains enough information to produce a Husk call stack.

Use a single-owner heap in `Instance`, not `Arc<Mutex<...>>` throughout the
interpreter. A practical first implementation is a generational slot map plus a
full mark/sweep collector at allocation safe points:

```text
HeapObject =
  Array(Vec<RuntimeValue>)
  Struct(TypeId, Vec<RuntimeValue>)
  Enum(TypeId, VariantId, Vec<RuntimeValue>)
  Closure(FunctionId, Vec<CapturedCellId>)
  Cell(RuntimeValue)
```

Roots are globals, active frames, extension-call temporaries, and explicit
`FunctionHandle` roots. The collector must trace every `HeapObject` variant and
has tests that create cyclic closures and cyclic containers. Do not implement
finalizers in version 1; instance teardown releases host and Wasm resources
explicitly.

Closure capture analysis marks captured locals. Captured mutable locals use
heap cells so all closures observe the same binding. Ordinary uncaptured locals
remain in call frames.

## Red compatibility architecture

The target Red structure is:

```text
src/plugin/
  registry.rs            package discovery, order, compatibility, quarantine
  runtime.rs             orchestration across plugin instances
  husk_module.rs         `red` ModuleDescriptor and function implementations
  lifecycle.rs           commands, listeners, requests, state, cleanup
```

Exact filenames can be adjusted to fit the existing module, but ownership must
follow this division.

Red creates one Husk `Instance<RedHostState>` per plugin. That gives globals,
heap, callbacks, limits, and Wasm state a natural isolation boundary. The
registry owns dependencies and transactional replacement. The lifecycle state
tracks every command, listener, request, callback handle, and state entry by
plugin instance generation.

Migration invariants:

- the `red` module retains its current versioned public API;
- existing plugin source does not need to add imports merely to keep working;
- the current 100,000-instruction Red limit remains until benchmarks justify a
  change;
- failed parse, type check, instantiation, or activation leaves the current
  plugin active;
- deactivation releases all callback handles and owned resources;
- pending requests cannot invoke a callback from a replaced instance;
- command collisions and plugin dependency behavior do not change;
- the bundled runtime self-check and cursor benchmark remain release gates.

At the end, `rg 'red::' crates/husk*` returns no production matches, and
`CommandMetadata` and `RequestId` live in Red rather than the Husk facade.

## Implementation roadmap

Each task card is intended to be one reviewable change. `S`, `M`, and `L` are
relative effort labels, not calendar estimates.

### Phase 0 — Freeze behavior and decisions

#### P0-01: Add a production-runtime conformance baseline (`M`)

Depends on: nothing

Files:

- `crates/husk/tests/` or current `crates/husk/src/lib.rs` tests;
- `src/plugin/runtime.rs` tests;
- `plugins/` and `examples/` only as fixtures.

Steps:

1. Inventory every statement/expression that the current VM intentionally
   executes.
2. Add table-driven positive tests and source-aware error tests.
3. Add Red bridge tests for command registration, listeners, request callback,
   state, activate/deactivate, and instruction exhaustion.
4. Add a test proving a failed replacement does not disturb an active plugin.
5. Record current cursor benchmark output without changing the threshold.

Done when:

- the tests fail if current plugin lifecycle behavior or diagnostics regress;
- unsupported parser constructs are separately inventoried rather than assumed
  to work.

#### P0-02: Record five short ADRs (`S`)

Depends on: P0-01

Create ADRs for:

1. static modules plus Wasm Components plus optional native C ABI;
2. `Engine`/`CompiledModule`/one-`Instance`-per-script ownership;
3. native versus legacy-JavaScript semantic profiles;
4. initial script/module/entry-point rules.
5. primitive value semantics, container copy/alias behavior, closure capture
   cells, JSON null/missing behavior, indexing failures, checked arithmetic,
   and structural equality.

Each ADR links back to this plan and lists what would justify revisiting it.

Done when a later agent does not need to choose these foundations again.

### Phase 1 — Compile once and keep source-aware results

#### P1-01: Introduce a compiled program object (`M`)

Depends on: P0-01

Files:

- `crates/husk/src/lib.rs`;
- `crates/husk-diagnostics`;
- tests in `crates/husk`.

Steps:

1. Add `CompileOptions`, including semantic profile, cfg flags, limits relevant
   at compile time, and external declarations/descriptors.
2. Add `CompiledProgram` containing the source file, parsed `File`, semantic
   result, and source map.
3. Make parsing and semantic errors one ordered diagnostic report.
4. Add `Vm::load_compiled_plugin` as a compatibility entry point.
5. Make existing `load_plugin` call the compiler once and delegate.

Do not move crates or alter runtime semantics on this card.

Done when a successfully loaded plugin is not reparsed by `Vm`.

#### P1-02: Remove Red's duplicate compile pipeline (`M`)

Depends on: P1-01

Files:

- `src/plugin/runtime.rs`;
- `crates/husk` compile API.

Steps:

1. Move all general validation into `CompileOptions` and `CompiledProgram`.
2. Preserve truly Red-specific policy checks as a post-semantic validation
   hook.
3. Pass the resulting compiled object into activation.
4. Add a regression test that parse and semantic diagnostics retain the plugin
   path and correct byte spans.

Done when Red no longer independently parses the same source before the VM
parses it.

### Phase 2 — Establish a native semantic profile

#### P2-01: Make prelude/profile selection explicit (`M`)

Depends on: P1-01

Files:

- `crates/husk-semantic/src/lib.rs`;
- `crates/husk-semantic/src/std/`;
- semantic tests.

Steps:

1. Add `SemanticProfile::Native` and `LegacyJavaScript`.
2. Preserve the existing behavior under the legacy profile.
3. Stop injecting JS globals and `JsValue` permissiveness in the native
   profile.
4. Emit an intentional diagnostic for `JsLiteral` and JS-only extern usage in
   native compilation.
5. Keep Red temporarily on the legacy profile until P2-02 provides neutral
   `Json`; add a test that makes this temporary dependency visible.

Done when native semantic tests cannot resolve JavaScript globals accidentally,
the temporary Red profile choice is explicit, and bundled Red plugins still
type-check.

#### P2-02: Split declarations from implementations in the neutral prelude (`L`)

Depends on: P2-01

Files:

- `crates/husk-semantic/src/stdlib/core.hk`;
- new `crates/husk-stdlib`;
- related stdlib index/tests.

Steps:

1. Inventory every prelude function/method and mark it as language primitive,
   Husk implementation, or native module function.
2. Move JavaScript-only declarations to the legacy profile.
3. Replace codegen-placeholder comments and bodies with neutral declarations
   or real Husk bodies.
4. Define neutral `Json`, including the Red-compatible null and missing-field
   behavior described above.
5. Supply native runtime implementations for the subset Red uses.
6. Generate one stdlib descriptor/index consumed by both semantics and runtime.

Done when the native prelude contains no claim that JavaScript codegen will
replace a body at runtime.

### Phase 3 — Generic modules and removal of Red from the VM

#### P3-01: Add `TypeDescriptor` and `ModuleDescriptor` (`M`)

Depends on: P1-02, P2-01

Files:

- `crates/husk-types`;
- `crates/husk`;
- semantic declaration ingestion.

Steps:

1. Implement validated names, types, functions, module version, and stable
   descriptor hashing.
2. Add a transitional descriptor-to-declaration-AST conversion.
3. Feed descriptors to semantic analysis through `CompileOptions`.
4. Switch Red to `SemanticProfile::Native` using neutral `Json` descriptors.
5. Test duplicate names, invalid signatures, and type-checking of qualified
   calls.

Done when a test module can type-check a call without a hand-written declaration
source string.

#### P3-02: Add native module dispatch (`L`)

Depends on: P3-01

Files:

- `crates/husk`;
- new module tests.

Steps:

1. Implement `NativeModule<T>`, its builder, `CallContext`, and low-level
   dispatch.
2. Resolve paths to a callable ID during compilation or program preparation;
   keep a compatibility path only for local functions.
3. Route module calls through their descriptor and implementation.
4. Charge host-call and instruction limits.
5. Convert native errors into source-aware runtime diagnostics.

Done when a test calls `sample::add` through a registered module and there is no
name-specific branch in the evaluator.

#### P3-03: Rebuild `red` as a native module (`L`)

Depends on: P3-02

Files:

- `src/plugin/runtime.rs`;
- new `src/plugin/husk_module.rs` if useful;
- `crates/husk/src/lib.rs`.

Steps:

1. Construct the Red API descriptor from the same builder that implements it.
2. Move every `red::*` match arm into Red-owned native functions.
3. Move command metadata, event listener, pending request, and plugin state
   storage to Red-owned lifecycle state.
4. Delete `RED_HOST_DECLARATIONS`.
5. Preserve JSON null, missing-field, indexing, state round-trip, and all
   existing host argument validation and diagnostics.

Done when:

```shell
rg 'red::' crates/husk*
```

finds no production dispatch and all baseline tests pass.

#### P3-04: Move to one runtime instance per Red plugin (`L`)

Depends on: P3-03

Files:

- `src/plugin/runtime.rs`;
- `src/plugin/registry.rs`;
- Husk instance code.

Steps:

1. Introduce an instance generation ID.
2. Give every loaded plugin its own program, globals, heap, budget, and callback
   root table.
3. Route commands/events/requests to `(instance generation, FunctionHandle)`.
4. Make reload instantiate and activate a candidate before swapping it into the
   registry.
5. Release old handles and resources only after a successful swap.

Done when a test proves two plugins cannot see each other's globals or callback
handles, and stale request callbacks are rejected after reload.

### Phase 4 — Public embedding API and static crate bridge

#### P4-01: Extract runtime and facade crates (`M`)

Depends on: P3-04

Files:

- new `crates/husk-runtime`;
- existing `crates/husk`;
- workspace `Cargo.toml`.

Steps:

1. Move generic implementation mechanically to `husk-runtime`.
2. Keep `husk` as the documented public facade.
3. Re-export compatibility names only as deprecated shims.
4. Update package descriptions and rustdoc so they do not refer to Red.
5. Add dependency-boundary tests or `cargo tree` assertions in CI.

Do not change semantics during the move.

Done when `husk-runtime` has no Red dependency or Red-named public type.

#### P4-02: Land `Engine<T>`, `CompiledModule`, and `Instance<T>` (`L`)

Depends on: P4-01

Steps:

1. Implement the ownership model described in this document.
2. Make engine construction validate all registered module descriptors.
3. Make compilation return reusable immutable `CompiledModule`.
4. Make instantiation own all mutable execution state.
5. Return detached `OwnedValue` from public calls.
6. Add rustdoc examples that compile as tests.

Done when an integration test outside internal modules embeds Husk, registers a
module, compiles source, instantiates it, and calls an exported function.

#### P4-03: Add typed native conversion adapters (`M`)

Depends on: P4-02

Steps:

1. Implement `HuskType`, `FromHusk`, `IntoHusk`, `NativeError`, and
   `ScriptResult`.
2. Support the initial primitive/container list from the public API section.
3. Include argument index, expected type, actual type, module, and function in
   conversion diagnostics.
4. Add compile-fail tests for unsupported adapter signatures where practical.

Done when a native function's semantic signature is derived from the same Rust
types used to convert its call.

#### P4-04: Add a static `regex` integration example (`S`)

Depends on: P4-03

Add an example embedder that depends on `regex`, exposes `is_match`, and handles
an invalid pattern as a Husk `Result`. This is the canonical demonstration that
"Husk can use a Rust crate" does not require dynamic loading.

### Phase 5 — Standalone one-file CLI

#### P5-01: Add `husk check` and `husk run` (`M`)

Depends on: P4-02, P2-02

Files:

- new `crates/husk-cli`;
- CLI integration tests.

Steps:

1. Add a binary package named `husk`.
2. Implement `check <path>` with source-aware diagnostics.
3. Implement `run <path> -- <args...>` with the documented `main` contract.
4. Define stable exit behavior: `0` success, validated script `i32` when
   returned, `1` compile/runtime failure, and `2` CLI usage failure.
5. Read source as bounded UTF-8 and handle a first-line shebang.

Done when CLI integration tests cover success, compile failure, runtime failure,
arguments, exit status, and shebang on all existing CI operating systems.

#### P5-02: Add a minimal native `std` host for the CLI (`M`)

Depends on: P5-01

Implement printing, arguments, and explicitly selected pure utilities. Do not
grant filesystem, network, process, environment, time, or random access merely
because the process has those capabilities. Define those as future registered
modules with explicit grants.

#### P5-03: Prove standalone dependency independence (`S`)

Depends on: P5-02

Add CI checks that:

- build and test `husk-cli` without building Red;
- build `husk`/`husk-runtime` with no default features;
- verify `cargo tree -p husk-cli` contains no package named `red`.

### Phase 6 — WebAssembly Component feasibility gate

#### P6-01: Build a throwaway typed Component spike (`M`)

Depends on: P4-03, P5-01

The spike may live under `experiments/` and must not enter the public API.

Steps:

1. Build the WIT `regex` example for `wasm32-wasip2`.
2. Load it with Wasmtime's Component API on Linux, macOS, and Windows.
3. Inspect exports dynamically through `Component::component_type()`.
4. Call an unknown-at-host-build-time function using component `Val`.
5. Demonstrate no linked WASI capabilities for the pure example.
6. Demonstrate fuel interruption and a memory/resource limit failure.
7. Record cold compile, instantiate, call-loop, binary-size, and memory
   observations in a dated document; do not invent a performance target before
   measuring.

#### P6-02: Make the Component go/no-go decision (`S`)

Depends on: P6-01

Go when all of these are true:

- dynamic export and type inspection is sufficient to build a Husk descriptor;
- primitive, list, tuple, option, result, record, and variant conversions work;
- the pure extension instantiates without broad WASI;
- fuel and resource limits reliably stop abusive guests;
- all three CI operating systems pass;
- errors can be mapped to useful Husk diagnostics.

If any item fails, write an ADR selecting either:

- a fixed Component world with a versioned bytes wire format; or
- Extism's bytes-in/bytes-out runtime model.

Do not proceed to production Wasm code without recording this decision.

### Phase 7 — Production portable extensions

#### P7-01: Implement `.huskext` manifest and bundle validation (`M`)

Depends on: P6-02 go decision

Files:

- new `crates/husk-extension`;
- manifest tests and malicious fixture tests.

Implement schema-version checking, unknown-field rejection, bounded reads,
canonical paths, semantic versions, module names, artifact digests, and bundle
inspection. Version 1 accepts directory bundles only.

#### P7-02: Implement Component-to-module descriptor conversion (`L`)

Depends on: P7-01

Files:

- new `crates/husk-wasm`;
- type mapping tests.

Steps:

1. Inspect imports, exported instances, functions, and named types.
2. Implement the exact version 1 mapping table above.
3. Normalize and collision-check names.
4. Store original WIT export indices/names in dispatch data.
5. Produce deterministic descriptor hashes independent of hash-map order.

Done when Husk can type-check a fully qualified
`regex::api::is_match(...)` call using only the loaded component and manifest.
P8 subsequently makes the equivalent `use regex::api::is_match;` import work
through the general module graph.

#### P7-03: Implement Wasm call dispatch and resource limits (`L`)

Depends on: P7-02

Steps:

1. Share compiled components per engine.
2. Instantiate per Husk instance.
3. Convert arguments/results through dynamic component `Val`.
4. Apply fuel, memory, table, instance, and call-result size limits.
5. Map traps, missing imports, bad results, and out-of-fuel to extension
   diagnostics with source call-site context.
6. Poison only the failing extension instance when Wasmtime says it cannot
   safely resume; do not crash unrelated Husk instances.

#### P7-04: Implement capability comparison (`M`)

Depends on: P7-02

Add typed capability names and the rule
`actual imports ⊆ requested ⊆ granted`. Start with no grants. Add only the
minimum logging interface needed for diagnostics, then test that filesystem,
network, and environment imports fail closed.

#### P7-05: Add extension CLI workflow and end-to-end example (`M`)

Depends on: P7-03, P7-04

Commands:

- `husk extension inspect <bundle>`;
- `husk extension pack ...`;
- `husk run --extension <bundle> <script>`;

`pack` may invoke Cargo only when the user separately runs an explicit build
subcommand; ordinary `pack`, `inspect`, and `run` do not. Add the WIT/Rust
`regex` adapter as an end-to-end fixture and run it in CI.

### Phase 8 — Multi-file modules and local package manifests

#### P8-01: Add module AST and deterministic filesystem resolution (`L`)

Depends on: P5-03

Files:

- lexer/parser/AST;
- runtime module resolver;
- diagnostics tests.

Implement `mod name;`, the two allowed file paths, ambiguity errors, package
root containment, canonical duplicate rejection, cycle diagnostics, and stable
module ordering.

#### P8-02: Make semantic analysis operate on a module graph (`L`)

Depends on: P8-01, P3-01

Steps:

1. Build symbol tables for every source and registered external module.
2. Resolve `crate`, `self`, `super`, external roots, imports, and visibility.
3. Type-check cross-module calls and types.
4. Preserve per-file spans in all diagnostics and references.
5. Keep editor hover/reference data usable across modules.

Done when two source files can share public functions, structs, and enums and
private access fails at the importing source span.

#### P8-03: Add `Husk.toml` and local-only `Husk.lock` (`M`)

Depends on: P8-02, P7-01

Implement nearest-manifest discovery, explicit entry source, local extension
paths, and a deterministic lock file. `--locked` rejects missing or changed
entries. Do not implement URLs, registries, or version solving.

### Phase 9 — Runtime-neutral HIR and full backend-neutral language

This phase uses vertical slices. Each card adds parser-to-HIR-to-runtime tests;
do not land an HIR node that the interpreter cannot execute.

#### P9-01: Lower the already-supported subset to HIR (`L`)

Depends on: P8-02, P4-02

Implement IDs, typed/spanned HIR, lowering for current VM constructs, and an HIR
interpreter. Run the same conformance fixtures through the old AST path and the
HIR path until their results and diagnostics match. Keep the old evaluator
behind a temporary test-only feature.

#### P9-02: Add the instance heap and collection (`L`)

Depends on: P9-01

Implement generational handles, arrays/objects/cells, neutral JSON values,
roots, mark/sweep, heap limits, stale-handle checks, and cycle tests. Add a
stress test that repeatedly creates and discards cyclic values without
unbounded growth.

#### P9-03: Add nominal structs and inherent methods (`L`)

Depends on: P9-02

Support definitions, construction, field reads/writes, privacy, `self`,
`&self`, `&mut self`, inherent method resolution, and structural diagnostics.
Struct identity must no longer be discarded into a generic object.

#### P9-04: Add enums and all pattern forms (`L`)

Depends on: P9-03

Support unit/tuple/struct variants, constructors, exhaustive `match`, bindings,
wildcards, literals, tuples, `if let`, `let ... else`, and destructuring lets.
Add unreachable/non-exhaustive diagnostics where the semantic analyzer already
has enough type information.

#### P9-05: Add tuples, ranges, casts, and `?` (`M`)

Depends on: P9-04

Define and test:

- tuple values and fields;
- exclusive/inclusive ranges and iteration;
- checked numeric casts, with no silent truncation;
- `?` propagation for `Option` and `Result`;
- source spans on failed casts and invalid range operations.

#### P9-06: Add closures and persistent function handles (`L`)

Depends on: P9-02

Implement capture analysis, shared captured mutable cells, nested closures,
recursive cycles, function values, callback roots, explicit release, instance
generation checks, and call-stack frames. Migrate Red callbacks fully to this
mechanism before deleting string callbacks.

#### P9-07: Add traits and generics by static dispatch (`L`)

Depends on: P9-03, P9-04, P9-06

Resolve trait bounds, impl selection, default methods, generic functions, and
`impl Trait` before execution. HIR calls a resolved implementation ID; the
runtime does not perform string-based trait lookup. Reject ambiguous or missing
impls during semantic analysis.

#### P9-08: Complete formatting, iterators, cfg, and test attributes (`L`)

Depends on: P9-05, P9-07, P2-02

Implement formatting expressions, neutral iterator methods, `#[cfg]`,
`#[test]`, `#[ignore]`, and `#[should_panic]` semantics needed by the future
test runner. Ensure every backend-neutral `ExprKind`, `StmtKind`, and
`ItemKind` has either an HIR lowering or an intentional native-profile compile
diagnostic.

#### P9-09: Delete the AST interpreter (`M`)

Depends on: P9-01 through P9-08

Delete the duplicated evaluator only after:

- differential tests have no unexplained differences;
- Red baseline, self-check, and benchmark pass on HIR;
- CLI and Wasm extension integration tests pass;
- no production path constructs a callable from an unresolved string.

Keep the AST as a compiler input, not an executable representation.

### Phase 10 — Standalone developer experience

#### P10-01: Add a REPL (`M`)

Depends on: P9-09

Add parser entry points for a complete item, statement, or expression. A REPL
session owns one instance and source map with synthetic numbered sources.
Incomplete input requests continuation; compile errors do not destroy prior
state. Add `:quit`, `:help`, and `:reset` only.

#### P10-02: Add `husk test` (`M`)

Depends on: P9-08, P8-03

Discover test functions in the package graph, instantiate tests in isolation,
honor ignore/should-panic/cfg, render captured output on failure, and return a
nonzero process status on any failed test.

#### P10-03: Finish diagnostics and user documentation (`M`)

Depends on: P10-01, P10-02

Document embedding, CLI use, package layout, native modules, extension authoring,
capabilities, limits, and unsupported WIT/native cases. Add runtime stack traces
that include Husk frames and the native/Wasm module boundary without exposing
host secrets.

### Phase 11 — Optional trusted native dynamic extensions

Do not start this phase merely because it is next. It requires a concrete
native-only crate use case that static embedding cannot satisfy for standalone
users.

#### P11-01: Implement a C-ABI spike (`M`, optional)

Depends on: P7-05 and a recorded use case

Build a pure-function adapter, the versioned API table, bounded wire messages,
and a `libloading` host on Linux, macOS, and Windows. Test:

- ABI major mismatch;
- truncated and oversized table;
- missing symbol;
- wrong target metadata;
- invalid pointers where safely detectable;
- extension-allocated response and matching free;
- panic converted to an error under `panic=unwind`;
- unique-filename reload without unloading.

Run the library under sanitizers on supported CI where practical.

#### P11-02: Decide C ABI versus `abi_stable` versus no native tier (`S`)

Depends on: P11-01

Compare safety surface, dependency health, binary size, error quality,
cross-toolchain tests, and adapter ergonomics. Default to **no native dynamic
tier** if a robust boundary cannot be demonstrated. If accepted, keep it behind
`native-extensions`, print an explicit trust warning, and never enable it in
Red by default.

### Phase 12 — Remove shims, harden, and package

#### P12-01: Remove Red-era compatibility types from Husk (`M`)

Depends on: P9-09 and completed Red migration

Delete deprecated plugin-shaped `Vm` APIs, `Host`, plugin callback strings,
`CommandMetadata`, `RequestId`, multi-plugin maps, and pseudo-stdlib `red`
helpers from the Husk facade. Update Red and the cursor benchmark to use the
public engine API.

#### P12-02: Add the final quality/security matrix (`M`)

Depends on: P12-01, P7-05

CI must cover:

- stable and nightly Rust where the repository already requires both;
- Linux, macOS, and Windows;
- default and no-default Husk features;
- CLI end-to-end runs;
- a `wasm32-wasip2` extension build and execution;
- malformed bundle/component fixtures;
- fuel, heap, recursion, callback-root, and component-memory exhaustion;
- parser/module/manifest fuzz targets or a documented scheduled fuzz job;
- bundled Red plugin self-check and deterministic performance gate.

#### P12-03: Prove independent packaging (`M`)

Depends on: P12-02

Steps:

1. Assign crate metadata, licenses, README links, and publish order.
2. Run `cargo package`/publish dry runs for all Husk crates.
3. Build a small external fixture project using only packaged Husk crates.
4. Build and run the standalone binary from packaged sources.
5. Confirm no package includes Red source or depends on the Red root crate.

Only after this proof should maintainers decide whether moving Husk to a
different physical repository has enough value to justify the release and
history-management work.

## Dependency order at a glance

```text
P0
 |
P1 compile-once
 |
P2 native semantics
 |
P3 generic modules and Red separation
 |
P4 public embedding/static crates
 +-------------------+
 |                   |
P5 CLI              P6 Wasm spike
 |                   |
P8 modules <------- P7 production extensions
 \                   /
  +------ P9 HIR/full language
              |
            P10 DX
              |
            P12 release

P11 native dynamic is optional after P7 and is not on the release critical path.
```

## Test strategy

### Conformance corpus

Create `tests/husk/cases/` or an equivalent shared fixture location with:

```text
cases/
  compile-pass/
  compile-fail/
  run-pass/
  run-fail/
  modules/
  extensions/
```

Each failure fixture has a checked expected diagnostic code, primary span,
message fragment, and relevant note/help. Avoid snapshots that accept wholesale
unreviewed diagnostic churn.

Every syntax feature gets at least:

- ordinary success;
- nested/composed success;
- type error;
- runtime error if applicable;
- limit-exhaustion behavior if it can allocate, loop, recurse, or call a host.

### Embedding contract tests

Test the facade as an external user:

- engine/module construction;
- compile once, instantiate many times;
- isolated globals;
- typed conversions;
- host-state mutation;
- native error versus script `Result`;
- callback handle ownership and release;
- limits and cancellation;
- `Send`/`Sync` assertions for intended public types.

### Extension contract tests

- component signature inspection and name normalization;
- every supported and rejected WIT type;
- manifest/requested/actual/granted capability combinations;
- traps, fuel, memory, oversized values, invalid UTF-8 at native boundaries;
- deterministic descriptor hash and lock digest;
- extension state isolation between Husk instances;
- no broad WASI imports in the pure regex example.

### Red regression tests

Continue to run:

```shell
cargo test --all-features -p red husk
cargo test --all-features \
  -p husk -p husk-ast -p husk-diagnostics -p husk-lexer \
  -p husk-parser -p husk-semantic -p husk-types
cargo run --all-features -- --self-check
cargo run --locked --release --example husk_cursor_bench -- --assert
```

Add new crate names to the package test list as they land.

## Risk register

| Risk | Failure mode | Mitigation and early signal |
| --- | --- | --- |
| Red extraction changes behavior | Bundled plugins silently differ | Phase 0 corpus, compatibility wrappers, one-instance migration after generic dispatch |
| Semantic layer remains JS-shaped | Native code accepts impossible operations | Explicit profile first, neutral prelude inventory, native-profile rejection tests |
| Parser/runtime drift returns | A construct type-checks but crashes as unsupported | HIR exhaustiveness, per-variant matrix, no raw-AST production interpreter |
| Module signature duplication | Compile-time and runtime arity/types disagree | One `ModuleDescriptor`; descriptor-derived declarations only as a transition |
| Callback lifetime bugs | Use-after-reload or leaks | Instance generations, persistent root table, explicit cleanup, stale-handle tests |
| Cyclic script values leak | Long-lived editor grows indefinitely | Single-owner tracing heap, allocation limit, cyclic stress test |
| Wasmtime increases build/binary cost | Lightweight embedders pay for unused dynamic loading | Separate `husk-wasm`, feature-gated facade, Red does not enable by default |
| Rust crate fails on Wasm | Extension cannot be portable | Static module path; optional trusted native or future helper process |
| Wasm imports grant too much | Extension accesses host resources unexpectedly | No broad WASI linker, actual/requested/granted subset rule, denial tests |
| Component/WASI tooling changes | Build instructions or APIs churn | Pin a tested Wasmtime major, keep WIT ABI versioned, use P2 stability, isolate in one crate |
| Native plugin crashes host | Abort, invalid pointer, or malicious code | Trusted-only warning, small C ABI, size checks, no unload, optional feature, preferably Wasm |
| Premature repository split | Broken dependencies and duplicated fixes | Package independently first; physical move is the last decision |
| Scope explosion | Extraction never reaches a usable milestone | Static bridge and one-file CLI before full language; optional features stay off critical path |

## Definition of done

The extraction is complete when all of the following are true:

- [ ] No production `crates/husk*` code refers to Red, plugin IDs, Red commands,
      Red events, editor requests, or `red::*`.
- [ ] Red uses one general Husk instance per plugin and all current plugin
      lifecycle/reload tests pass.
- [ ] Rust can embed Husk through documented `Engine`, `CompiledModule`, and
      `Instance` APIs.
- [ ] A static native adapter demonstrates a normal Rust crate (`regex`) from an
      embedding application.
- [ ] The `husk` binary runs and checks a standalone script without depending on
      Red.
- [ ] Multi-file modules and local manifests resolve deterministically.
- [ ] A typed, sandboxed `.huskext` component built from the `regex` crate loads
      and executes on Linux, macOS, and Windows.
- [ ] Extension capabilities are denied by default and CPU/memory limits have
      adversarial tests.
- [ ] Every backend-neutral parser construct either executes through HIR or
      receives an intentional native-profile compile diagnostic.
- [ ] Native dynamic loading is either absent or clearly opt-in, trusted-only,
      versioned, and tested through a C ABI.
- [ ] Husk crates package independently and an external fixture builds from
      those packages.
- [ ] Workspace tests, strict Clippy, formatting, Red self-check, and the
      deterministic plugin benchmark pass.

## Primary research sources

Rust ABI and dynamic loading:

- [Rust Reference: linkage and crate types](https://doc.rust-lang.org/reference/linkage.html)
- [Rust Reference: type layout guarantees](https://doc.rust-lang.org/stable/reference/type-layout.html)
- [Rust Reference: ABI](https://doc.rust-lang.org/reference/abi.html)
- [Rust Nomicon: FFI and unwinding](https://doc.rust-lang.org/nomicon/ffi.html)
- [RFC 1510: `cdylib`](https://rust-lang.github.io/rfcs/1510-cdylib.html)
- [Cargo build scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html)
- [Cargo target and crate-type model](https://doc.rust-lang.org/cargo/reference/cargo-targets.html)
- [`libloading` documentation](https://docs.rs/libloading/latest/libloading/)
- [`abi_stable` documentation](https://docs.rs/abi_stable/latest/abi_stable/)
- [`abi_stable` library loading behavior](https://docs.rs/abi_stable/latest/abi_stable/library/index.html)

WebAssembly Components and isolation:

- [Component Model: WIT design](https://component-model.bytecodealliance.org/design/wit.html)
- [Component Model: canonical ABI](https://component-model.bytecodealliance.org/advanced/canonical-abi.html)
- [Component composition and distribution](https://component-model.bytecodealliance.org/composing-and-distributing/composing.html)
- [Building a Rust component with current native tooling](https://component-model.bytecodealliance.org/language-support/building-a-simple-component/rust.html)
- [Rust `wasm32-wasip2` target](https://doc.rust-lang.org/rustc/platform-support/wasm32-wasip2.html)
- [Wasmtime Component embedding API](https://docs.wasmtime.dev/api/wasmtime/component/index.html)
- [Wasmtime dynamic Component `Val`](https://docs.rs/wasmtime/latest/wasmtime/component/enum.Val.html)
- [Wasmtime security model](https://docs.wasmtime.dev/security.html)
- [Wasmtime interruption with fuel and epochs](https://docs.wasmtime.dev/examples-interrupting-wasm.html)
- [Extism's bytes-in/bytes-out plugin model](https://extism.org/docs/questions/)
- [Extism Rust PDK crate-adapter example](https://github.com/extism/rust-pdk)

Embedding/module design precedents:

- [Rhai native Rust functions](https://rhai.rs/book/rust/functions.html)
- [Rhai module resolvers](https://rhai.rs/book/rust/modules/resolvers.html)

These sources support the ABI and extension decisions. The current-state audit
and migration sequence come only from this repository's code and documentation.
