# Husk language and embedding guide

This guide describes the interfaces implemented at the 2026-07-19 extraction
checkpoint. For architecture rationale, see
[HUSK_LANGUAGE_EXTRACTION_PLAN.md](HUSK_LANGUAGE_EXTRACTION_PLAN.md). For known
gaps and exact remaining work, see
[HUSK_IMPLEMENTATION_STATUS.md](HUSK_IMPLEMENTATION_STATUS.md).

## Build and run the CLI

From this workspace:

```shell
cargo build -p husk-cli
cargo run -p husk-cli -- check path/to/script.hk
cargo run -p husk-cli -- run path/to/script.hk -- first-argument second-argument
cargo run -p husk-cli -- test path/to/package
cargo run -p husk-cli -- repl
```

After installing the binary, omit `cargo run -p husk-cli --`:

```shell
cargo install --path crates/husk-cli
husk check script.hk
husk run script.hk
```

CLI status conventions are:

- `0`: success;
- `1`: source, compile, package, extension, test, or runtime failure;
- `2`: invalid command-line usage;
- `husk run` may return a script's validated `i32` `main` result in the range
  `0..=255`.

The source reader accepts bounded UTF-8. A first-line `#!` shebang is replaced
with spaces before compilation so later diagnostic locations remain stable.

## One-file scripts

The smallest executable script is:

```husk
fn main() {
    std::println("hello from Husk");
}
```

Supported standalone entry contracts are:

```husk
fn main()
fn main(args: [String])

fn main() -> i32
fn main(args: [String]) -> i32

fn main() -> Result<(), E>
fn main(args: [String]) -> Result<(), E>
```

Unit or `Ok(())` means success. An `i32` becomes the process status after range
validation. `Err(value)` is rendered as a CLI failure. Passing arguments to a
zero-argument `main` is an error.

The CLI's built-in `std` module intentionally contains only:

```husk
std::print(value: String)
std::println(value: String)
```

The CLI does not grant ambient filesystem, network, environment, process,
clock, or random access.

## Interactive use

Start:

```shell
husk repl
```

The session preserves top-level definitions, local bindings, closures, native
module state, and Wasm instance state:

```text
husk> fn double(value: i32) -> i32 {
....>     value * 2
....> }
husk> let mut answer = 20;
husk> answer += 1
21
husk> double(answer)
42
```

Only three commands are implemented:

```text
:help
:reset
:quit
```

Unclosed delimiters and expressions ending at an expected token boundary ask
for another line. Syntax and semantic errors leave prior session state intact.
Script-owned heap changes from a runtime failure are transactional. A native
or Wasm call may have already mutated host-owned state before returning an
error; the runtime cannot roll that external effect back.

The same behavior is available to embedders:

```rust
use husk::{Engine, OwnedValue, ReplOutcome};

let engine = Engine::<()>::builder().build()?;
let mut session = engine.repl(())?;

assert_eq!(
    session.submit("let answer = 40 + 2;")?,
    ReplOutcome::Value(OwnedValue::Unit),
);
assert_eq!(
    session.submit("answer")?,
    ReplOutcome::Value(OwnedValue::I64(42)),
);
# Ok::<(), anyhow::Error>(())
```

Parser/tooling callers can separately use
`parse_item_fragment`, `parse_statement_fragment`,
`parse_expression_fragment`, or `parse_repl_fragment` from `husk-parser`.

## Local packages

A package is filesystem-only. It has no registry, URL dependency, or version
solver.

Example layout:

```text
calculator/
├── Husk.toml
└── src/
    ├── main.hk
    ├── math.hk
    └── math/
        └── constants.hk
```

`Husk.toml`:

```toml
schema_version = 1

[package]
name = "calculator"
version = "0.1.0"
entry = "src/main.hk"
```

`src/main.hk`:

```husk
mod math;

use crate::math::answer;

fn main() -> i32 {
    answer()
}
```

`src/math.hk`:

```husk
mod constants;

pub fn answer() -> i32 {
    constants::VALUE()
}
```

`src/math/constants.hk`:

```husk
pub fn VALUE() -> i32 {
    42
}
```

For `mod math;`, exactly one of `math.hk` or `math/mod.hk` may exist. The
resolver rejects ambiguity, cycles, duplicate canonical files, symlinks at
protected inputs, and paths escaping the package root. Modules are processed
in deterministic order.

Run any of these:

```shell
husk check calculator
husk check calculator/Husk.toml
husk run calculator
husk test calculator
```

An unlocked package command writes or refreshes `Husk.lock`. Reproducible use
requires the existing lock:

```shell
husk check --locked calculator
husk run --locked calculator
husk test --locked calculator
```

`--locked` rejects a missing or changed lock file.

## Tests

Test functions use Rust-like attributes:

```husk
#[test]
fn addition_works() {
    assert_eq(20 + 22, 42);
}

#[test]
#[ignore]
fn slow_case() {}

#[test]
#[should_panic = "expected text"]
fn expected_failure() {
    panic("expected text");
}
```

Commands:

```shell
husk test package/
husk test package/ addition
husk test --list package/
husk test --include-ignored package/
```

Each test receives a new runtime instance. Filtering is a qualified-name
substring match. A failure makes the process status nonzero. Output capture is
not complete yet: native `std` output is written as it occurs.

## Embed Husk in Rust

Add the facade and any Rust crate you want to expose:

```toml
[dependencies]
anyhow = "1"
husk = { path = "../path/to/crates/husk" }
regex = "1"
```

Create one immutable engine, compile once, then instantiate mutable state as
often as needed:

```rust
use husk::{
    CallContext, Engine, NativeError, NativeModule, OwnedValue, ScriptResult,
};

#[derive(Default)]
struct State {
    regex_calls: usize,
}

let regex = NativeModule::<State>::builder("regex")
    .typed_function(
        "is_match",
        |context: &mut CallContext<'_, State>,
         pattern: String,
         input: String|
         -> Result<ScriptResult<bool, String>, NativeError> {
            context.data_mut().regex_calls += 1;
            Ok(regex::Regex::new(&pattern)
                .map(|compiled| compiled.is_match(&input))
                .map_err(|error| error.to_string())
                .into())
        },
    )
    .build()?;

let engine = Engine::builder()
    .register_module(regex)?
    .build()?;

let compiled = engine.compile_source(
    "matcher",
    "scripts/matcher.hk",
    r#"
        fn matches(pattern: String, input: String) -> Result<bool, String> {
            regex::is_match(pattern, input)
        }
    "#,
)?;

let mut first = engine.instantiate(compiled.clone(), State::default())?;
let mut second = engine.instantiate(compiled, State::default())?;

let result = first.call(
    "matches",
    &[
        OwnedValue::String("^husk$".into()),
        OwnedValue::String("husk".into()),
    ],
)?;
assert!(matches!(
    result,
    OwnedValue::Variant { case, .. } if case == "Ok"
));
assert_eq!(first.data().regex_calls, 1);
assert_eq!(second.data().regex_calls, 0);
# Ok::<(), anyhow::Error>(())
```

`Engine` and `CompiledModule` are shareable immutable configuration/artifacts.
Each `Instance` owns its VM heap, script state, callback roots, budgets, host
state, and Wasm stores. An `Instance` is intentionally not `Sync`.

### Typed adapter surface

Typed handlers currently accept zero, one, or two arguments. Built-in adapter
types are:

| Rust type | Husk descriptor/value |
| --- | --- |
| `()` | `()` |
| `bool` | `bool` |
| `i32` | `i32` |
| `i64` | `i64` |
| `f64` | `f64` |
| `String`, borrowed `&str` | `String` |
| `Vec<T>` | `[T]` |
| `Option<T>` | `Option<T>` |
| `(A, B)` | `(A, B)` |
| `ScriptResult<T, E>` return | `Result<T, E>` |

A host failure is `NativeError` and aborts the call. An expected
script-visible failure should be returned as `ScriptResult<T, E>`.

Use the low-level builder `function` method with an explicit
`FunctionDescriptor` for record/variant adapters or arities not covered by the
typed helpers. The descriptor and handler must agree.

### Runtime limits

`Limits::default()` currently configures:

| Limit | Default |
| --- | ---: |
| instructions per call/submission | 100,000 |
| nested call depth | 512 |
| live heap bytes | 64 MiB |
| live heap objects | 1,000,000 |
| one boundary value | 16 MiB |
| one source/module | 1 MiB |
| source/external modules | 256 |
| native/Wasm host calls per call | 10,000 |
| retained closure roots | 10,000 |
| extension instances per engine instance | 64 |

Set limits before building:

```rust
use husk::{Engine, Limits};

let engine = Engine::<()>::builder()
    .limits(Limits {
        instructions_per_call: 20_000,
        max_heap_bytes: 8 * 1024 * 1024,
        ..Limits::default()
    })
    .build()?;
# Ok::<(), anyhow::Error>(())
```

Retained closures use `Instance::capture_function`,
`Instance::invoke_function`, and `Instance::release_function`. Handles contain
instance and slot generations; using a released handle or a handle from another
instance fails.

### Semantic profiles

New engines default to `SemanticProfile::Native`. This profile rejects
JavaScript literals and JS-only extern assumptions. Red's temporary
compatibility path uses `SemanticProfile::LegacyJavaScript`.

Do not select the legacy profile for new standalone applications merely to
silence a native diagnostic. It intentionally preserves dynamic arity,
object-like struct behavior, and other plugin compatibility rules.

## Why Cargo crates are not loaded directly

Cargo packages are compile-time dependency graph nodes. Their artifacts do not
provide a uniform runtime function inventory, stable Rust ABI, or portable
representation for generics, traits, macros, closures, and Rust-layout
containers. Loading an `rlib` is not meaningful, and looking up symbols in a
Rust `dylib` would require unsafe guesses about compiler-specific ABI and type
layout.

Every exposed crate therefore needs a small adapter that:

1. selects the callable surface;
2. names functions and parameters;
3. maps arguments and results to Husk types;
4. decides which errors are host failures and which are script values;
5. defines the trust/capability boundary.

Use a static native module when the Rust application controls its Cargo graph.
Use a WebAssembly Component when a standalone process must discover and load
the adapter dynamically.

## Portable `.huskext` extensions

Version 1 is a validated directory bundle:

```text
math.huskext/
├── extension.toml
└── component.wasm
```

Example `extension.toml`:

```toml
schema_version = 1
name = "math"
version = "1.0.0"
module = "math"
artifact = "component.wasm"
world = "example:math/husk-extension@1.0.0"
minimum_husk = "0.1.0"

[capabilities]
requested = []
```

Packaging is explicit and never invokes Cargo:

```shell
husk extension pack \
  --manifest extension.toml \
  --component target/wasm32-wasip2/release/component.wasm \
  --output dist/math.huskext

husk extension inspect dist/math.huskext
husk run --extension dist/math.huskext script.hk
```

If an adapter is written in Rust, build it separately with the appropriate
Component toolchain. `pack`, `inspect`, ordinary package resolution, and
`run` do not compile or download crates.

A package can declare a local bundle:

```toml
[extensions.math]
path = "vendor/math.huskext"
```

The manifest key must equal the extension package `name`. `Husk.lock` records
the local source, module/version, and component SHA-256.

### WIT mapping

Supported v1 function types are:

- `bool`;
- `u8` (boundary bytes/elements) and `s32` as Husk `i32`;
- `s64` as Husk `i64`;
- `float64`;
- `string`;
- `list<T>` and optimized `list<u8>` bytes;
- tuples;
- options;
- results;
- exported named records, enums, and variants.

Multiple WIT results become one Husk tuple. Kebab-case names normalize to
underscores, and normalization collisions are rejected.

The following are rejected by v1:

- `s8`, `s16`, `u16`, `u32`, and `u64`;
- `float32` and `char`;
- maps and flags;
- owned or borrowed resources;
- futures, streams, async functions, and error contexts;
- anonymous record/variant/enum types where nominal identity is required;
- core-module, nested-component, resource, or other non-function exports in a
  callable position.

### Capability behavior

Bundle validation and component inspection enforce:

```text
actual component imports ⊆ manifest requested capabilities ⊆ host grants
```

Recognized capability categories are `filesystem`, `network`, `clock`,
`random`, `environment`, `process`, `io`, and `log`. Unknown imports fail
closed.

The current standalone engine grants none and deliberately links no WASI or
custom capability providers. Consequently, runtime loading currently supports
pure components. `extension inspect` can display and validate a requested
import set, but that does not make a provider available for execution.

Per Wasm instance, the runtime configures fuel, memory, table, core-instance,
table-count, memory-count, and boundary-value limits. A guest failure poisons
that extension instance when it cannot be safely resumed; it does not poison
other Husk instances.

## Build without portable extensions

`husk-runtime` makes Wasmtime optional:

```shell
cargo check -p husk-runtime --no-default-features
```

The `husk` facade enables its `wasm-extensions` feature by default and forwards
it to the runtime. An embedder that needs a smaller dependency surface can
disable facade defaults:

```toml
husk = { version = "0.1", default-features = false }
```

## Independence proof

Run:

```shell
sh scripts/check-husk-independence.sh
```

The script creates a temporary workspace containing only Husk crates and a
small external `regex` embedder, then checks all features and executes the
fixture. It is designed to catch accidental dependencies on Red source or the
root package.

## Known limitations

The release-relevant limitations are tracked in detail in
[HUSK_IMPLEMENTATION_STATUS.md](HUSK_IMPLEMENTATION_STATUS.md). The most
important current ones are:

- Red policy and `red::*` are Red-owned, but Red still calls the internal
  `Vm`/`Host` compatibility API instead of public `Instance`/`NativeModule`
  values;
- Red registrations currently accept named, generation-safe callbacks rather
  than retained closure-capable `FunctionHandle`s;
- nested struct-field patterns, literal patterns, and full exhaustiveness
  diagnostics are incomplete;
- test output capture is incomplete;
- REPL diagnostics use one accumulated source rather than numbered snippets;
- capability providers are not linked, so portable runtime extensions must be
  pure;
- native dynamic libraries are intentionally unsupported;
- there is no network package manager or runtime Cargo integration.
