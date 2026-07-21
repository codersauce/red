# Husk extraction implementation status

Checkpoint: 2026-07-19

This document records what was actually implemented from the
[Husk extraction plan](HUSK_LANGUAGE_EXTRACTION_PLAN.md). It is deliberately
stricter than a feature list: a card is only complete when its important
acceptance criteria are present. The original plan remains the design source of
truth.

## Status vocabulary

- **Complete**: the card's user-visible behavior and principal tests exist.
- **Mostly complete**: usable end to end, with a named acceptance detail still
  missing.
- **Partial**: a meaningful vertical slice exists, but a release-significant
  part of the card remains.
- **Not started**: no implementation intended to satisfy the card.
- **Deferred by design**: deliberately outside the release-critical path.

## Result at a glance

Husk is now usable in two independent ways:

1. Rust applications can embed it through `Engine<T>`, `CompiledModule`,
   `Instance<T>`, `ReplSession<T>`, typed `NativeModule<T>` adapters, and
   detached `OwnedValue` values.
2. The `husk` executable can check, run, test, and interactively evaluate
   scripts; resolve local multi-file packages; and load pure WebAssembly
   Component extensions from validated `.huskext` bundles.

The implemented dynamic-extension answer is not raw Cargo loading. An arbitrary
Rust crate has no stable runtime ABI or discoverable portable function surface.
The supported choices are:

- link the crate normally and expose selected functions through a typed static
  `NativeModule`;
- compile a small adapter and its dependencies as a WebAssembly Component, then
  load that component dynamically.

The standalone crates have a physical-independence smoke test. The script
`scripts/check-husk-independence.sh` copies only `crates/husk*` plus an external
fixture into a temporary workspace, builds it, and runs a static `regex`
adapter. No Red source or Red root package is copied.

Red application policy is now physically outside the Husk crates. Commands,
listeners, requests, plugin state, lifecycle effect staging, request IDs, and
all `red::*` dispatch live in `src/plugin/runtime.rs`. Red owns one compatibility
VM per plugin, and host-retained named callbacks carry a stable compiled
function ID plus a loaded-program generation. A callback retained across a
successful replacement is rejected as stale instead of silently resolving to a
same-named function in the new program.

Native HIR calls no longer perform ordinary name lookup while executing.
Script functions, module functions, intrinsic functions, and methods carry
resolved targets; compiled tables are indexed by deterministic stable IDs.
Dynamic string lookup remains only behind the explicit
`LegacyJavaScript` profile used by existing Red plugins.

This is not yet a release-complete extraction. The main remaining work is:

1. migrate Red from the internal `Vm`/`Host` compatibility layer to the public
   `Instance<RedPluginState>` and `NativeModule<RedPluginState>` API, then delete
   those compatibility APIs from `husk-runtime`;
2. finish pattern/exhaustiveness coverage and the generated AST-to-HIR coverage
   audit;
3. run the Rust `regex` Component fixture and independence suite in supported
   Linux/macOS/Windows CI;
4. complete packaged-source publication proof, fuzzing, output capture, and the
   boundary diagnostic/redaction audit.

## Implemented crate boundaries

| Crate | Current responsibility |
| --- | --- |
| `husk` | Small documented facade that re-exports the public embedding API. |
| `husk-runtime` | Compiler orchestration, resolved HIR interpreter, heap, embedding ownership, limits, and an internal compatibility VM used by Red during migration. |
| `husk-value` | Detached, backend-neutral host boundary values. |
| `husk-types` | Validated module/type/function descriptors and stable hashes. |
| `husk-extension` | Strict `.huskext` manifests, bounded directory bundles, hashes, and capability-set validation. |
| `husk-wasm` | Wasmtime Component inspection, descriptor derivation, dynamic calls, conversion, fuel, and store limits. |
| `husk-package` | Bounded local `Husk.toml`, deterministic module graph, local extensions, and `Husk.lock`. |
| `husk-hir` | Spanned executable HIR with stable per-function node and local IDs. |
| `husk-cli` | `check`, `run`, `test`, `repl`, and extension bundle commands. |
| `husk-lexer`, `husk-ast`, `husk-parser`, `husk-semantic`, `husk-diagnostics` | Frontend and source-aware diagnostics. |

The intended dependency direction is:

```text
frontend crates ──> husk-hir ──> husk-runtime ──> husk facade
                         │              │
husk-extension ──> husk-package         └──> husk-wasm (optional feature)
       │                  │
       └────────> husk-types <──────── husk-value

husk-cli ──> husk facade
Red ──────> husk-runtime compatibility API (one VM per plugin)
```

No Husk crate depends on the Red root package.

## Roadmap card status

### Phase 0 — baseline and decisions

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P0-01 production conformance baseline | Complete | `docs/HUSK_RUNTIME_SUPPORT_MATRIX.md`, `crates/husk/tests/runtime_conformance.rs`, and existing Red lifecycle tests freeze the starting behavior. |
| P0-02 architecture decisions | Complete | ADRs 0004 through 0008 record extension tiers, ownership, profiles, scripts/modules, and value semantics. ADR 0009 records the Component decision. |

### Phase 1 — compile once

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P1-01 compiled program | Complete | `CompileOptions` and `CompiledProgram` retain source, syntax, semantics, HIR functions, source maps, descriptors, and test metadata. `Vm::load_compiled_plugin` consumes it. |
| P1-02 remove duplicate Red compilation | Complete | Red compiles through the shared object and passes it to runtime activation; source paths and spans are regression tested. |

### Phase 2 — native semantics

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P2-01 explicit profiles | Complete | `SemanticProfile::{Native, LegacyJavaScript}` is explicit. The native profile rejects JavaScript-only constructs and does not receive JS globals. |
| P2-02 neutral prelude | Mostly complete | `crates/husk-semantic/src/stdlib/native.hk` is a neutral native prelude and runtime primitives implement its supported behavior. The proposed separate `husk-stdlib` crate/index was not created; descriptor generation should replace the remaining declaration-file duplication before release. |

### Phase 3 — generic modules and Red separation

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P3-01 descriptors | Complete | `husk-types` validates names and signatures, models interfaces and nominal types, computes deterministic hashes, and feeds generated declarations into semantics. |
| P3-02 native module dispatch | Complete | Typed module registration, semantic checking, host-call budgets, boundary limits, source-context errors, deterministic `ModuleFunctionId`s, and ID-indexed dispatch work. Native execution does not route module calls by evaluator-specific name branches. |
| P3-03 Red as a native module | Mostly complete | All `red::*` execution and Red policy/state types moved to `RedHost`; Husk crates contain no Red dispatch or Red policy types. Red still uses a hand-written compatibility declaration and `Host::call_module` rather than one typed `NativeModule<RedPluginState>` builder as descriptor/handler source of truth. |
| P3-04 one runtime instance per Red plugin | Mostly complete | Red owns one VM, heap, limits, and unique instance generation per plugin. Reload is staged and transactional. Named callbacks carry stable function IDs and program generations, so replaced callbacks fail stale. Red still needs to migrate to public `Instance` values and retained closure-capable `FunctionHandle`s. |

### Phase 4 — embedding and static Rust crates

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P4-01 runtime/facade crates | Complete | `husk` explicitly re-exports the supported compile/embed/package/extension API and does not expose `Vm`, `Host`, `Callback`, dynamic `Value`, or `Program`. `husk-runtime` has no Red dependency or Red-named public type; Red temporarily depends on its internal compatibility API directly. |
| P4-02 ownership API | Complete | Immutable engines and compiled modules, isolated mutable instances, detached values, generation IDs, reusable compilation, and rustdoc/integration examples exist. |
| P4-03 typed adapters | Complete | `HuskType`, `FromHusk`, `IntoHusk`, `NativeError`, `ScriptResult`, typed builder handlers, and contextual conversion failures are implemented. |
| P4-04 static `regex` example | Complete | `crates/husk/examples/regex_embed.rs` and the independent smoke fixture expose `regex::Regex` through a typed adapter, including script-level invalid-pattern errors. |

### Phase 5 — standalone CLI

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P5-01 `check` and `run` | Complete | Bounded UTF-8, shebang preservation, `main` contracts, arguments, exit codes, and source diagnostics have CLI integration tests. |
| P5-02 minimal `std` | Complete | The CLI grants only `std::print` and `std::println`; filesystem, network, process, environment, clock, and random access are not ambient. |
| P5-03 independence | Complete locally | `cargo tree -p husk-cli` has no Red package, no-default runtime builds are supported, and `scripts/check-husk-independence.sh` proves a copied Husk-only workspace builds and runs. Add this script to all supported CI operating systems. |

### Phase 6 — Component feasibility

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P6-01 typed spike | Mostly complete | Dynamic type/export inspection, unknown-at-host-build-time calls, rich WIT values, no-WASI pure instantiation, fuel/store limits, and measurements exist in `husk-wasm`, its tests/example, and `docs/husk-wasm-feasibility-2026-07-19.md`. A Rust `regex` component and explicit Linux/macOS/Windows CI matrix remain. |
| P6-02 go/no-go | Complete | ADR 0009 selects the Component Model with a deliberately narrow first ABI. |

### Phase 7 — portable extensions

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P7-01 bundle validation | Complete | Schema and unknown-field checks, bounded reads, canonical containment, symlink rejection, module/version validation, and SHA-256 digests are tested. |
| P7-02 descriptor conversion | Complete for v1 | Root/interface exports, deterministic normalized names, signatures, records, variants, enums, and supported containers derive from the Component type. Unsupported WIT kinds fail explicitly. |
| P7-03 dispatch and limits | Complete for v1 | Components compile once per engine, instantiate once per Husk instance, call through dynamic `Val`, enforce fuel/store/value limits, and poison a failing guest instance. |
| P7-04 capability comparison | Complete for deny-by-default v1 | `actual imports ⊆ requested ⊆ granted` is enforced. The runtime intentionally links no capability providers yet, so ordinary execution currently supports pure components only. |
| P7-05 CLI workflow | Mostly complete | `extension pack`, `extension inspect`, explicit/package `--extension` loading, and an end-to-end dynamically typed math component are tested. Add the planned Cargo-built Rust `regex` component fixture to cross-platform CI. |

### Phase 8 — packages and modules

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P8-01 deterministic modules | Complete | `mod name;`, both file layouts, stable traversal, ambiguity/cycle/escape/duplicate checks, and bounded source loading are implemented. |
| P8-02 semantic module graph | Complete for current language | `crate`, `self`, `super`, imports, visibility, external roots, cross-file functions and nominal types, and per-file diagnostics are covered by package tests. |
| P8-03 manifest and lock | Complete | Strict local-only `Husk.toml`, explicit entry source, local `.huskext` paths, deterministic `Husk.lock`, and `--locked` enforcement work. No network resolver exists. |

### Phase 9 — HIR and native language

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P9-01 HIR interpreter | Complete | All production execution lowers through `husk-hir`; compiled artifacts expose HIR summaries and source spans. |
| P9-02 heap and collection | Complete | Generational cells/closures, roots, stale checks, mark/sweep, byte/object limits, cycle tests, and finished-frame reclamation are implemented. |
| P9-03 structs and inherent methods | Complete | Nominal construction, field access/mutation, `self`, immutable/mutable receivers, privacy, and methods execute in the native profile. |
| P9-04 enums and patterns | Partial | Unit/tuple/struct variants, tuple and enum patterns, `match`, `if let`, `let ... else`, and destructuring execute. Literal patterns are absent from the AST, nested struct-field patterns are intentionally rejected, and exhaustiveness/unreachable analysis is not complete. |
| P9-05 tuples/ranges/casts/`?` | Complete for documented forms | Nominal tuples, checked casts, `Option`/`Result` propagation, lazy integer ranges, range methods, iteration, and bounded array/tuple/Unicode-string/JSON slicing execute. |
| P9-06 closures and handles | Mostly complete | Capture analysis, shared mutable cells, nested/recursive closures, generation-safe retained handles, release, root limits, and call frames work for embedders. Red named callbacks are stable-ID/program-generation safe, but Red has not adopted closure-capable retained `FunctionHandle`s. |
| P9-07 traits/generics | Complete for accepted forms | Semantic bound checks, impl completeness/ambiguity checks, default methods, generic calls, and runtime behavior have focused tests. Native calls and methods lower to stable `FunctionId`/`MethodTarget` values and dispatch through ID-indexed tables. |
| P9-08 formatting/iterators/cfg/tests | Mostly complete | Formatting, core iterator/collection methods, `cfg`, test attributes, discovery, and expected-panic behavior work. Iterator protocol breadth and captured test output remain limited. |
| P9-09 delete AST execution/unresolved calls | Complete for the native profile | Runtime execution is HIR-based; AST remains compiler/tooling input. Native direct calls, methods, function values, module calls, and intrinsics are resolved before execution. Ordinary name fallback is isolated to the explicit legacy profile. |

### Phase 10 — developer experience

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P10-01 REPL | Mostly complete | Public item/statement/expression fragment parsers distinguish complete, incomplete, and invalid input. `ReplSession` preserves items, locals, heap, native/Wasm state, and rolls back script-owned state on runtime failure. `husk repl` implements only `:help`, `:reset`, and `:quit`. Diagnostics use one accumulated `<repl>` source rather than numbered synthetic source files. |
| P10-02 test runner | Mostly complete | Package-wide discovery, filtering/listing, cfg, ignore, expected panic, isolated instances, and failure exit status work. Native `std` output is not captured and replayed only on failure. |
| P10-03 diagnostics/docs | Partial | This status document and `HUSK_LANGUAGE_GUIDE.md` cover public workflows. Source-aware Husk call frames and module boundary context exist in important paths, but the complete diagnostic matrix and secret-redaction audit remain. |

### Phase 11 — optional native dynamic libraries

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P11-01 C ABI spike | Deferred by design | There is no concrete use case that pure Components plus static adapters fail to cover. Do not start this card speculatively. |
| P11-02 native-tier decision | Deferred by design | The current product decision is no native dynamic tier. Reopen only after a successful cross-platform, sanitizer-tested spike for a real use case. |

### Phase 12 — hardening and packaging

| Card | Status | Evidence and remaining work |
| --- | --- | --- |
| P12-01 remove shims | Partial | The public `husk` facade is clean, and Red policy types/dispatch have moved out. Red still imports the internal `Vm`, `Host`, `Callback`, and dynamic `Value` compatibility layer; migrate it to public instances/modules before deleting those APIs from `husk-runtime`. |
| P12-02 final matrix | Partial | Focused limits, malformed bundles, package security, CLI, runtime, extension, and Red tests exist. Cross-platform Component CI, fuzzing, nightly coverage where required, and the post-migration matrix remain. |
| P12-03 independent packaging | Mostly complete locally | Every crate has version/license/repository/description metadata. A Husk-only copied workspace and external fixture build and run. Leaf crates pass `cargo package --allow-dirty --no-verify`; dependent package dry runs require publishing internal crates in dependency order or a temporary local registry. |

## Remaining release work

### R1 — finish the Red compatibility-layer migration

Completed in this checkpoint:

1. `CommandMetadata`, `RequestId`, commands, listeners, pending requests,
   plugin state, lifecycle effect staging, and request allocation are Red-owned.
2. Every `red::*` match arm and compatibility helper is outside
   `crates/husk*`.
3. Red owns one compatibility VM per plugin with an independent heap, budget,
   instance generation, and loaded program.
4. Reload compiles and activates a staged clone, migrates state, orders
   teardown effects, and commits only when every step succeeds.
5. Named callbacks retain `FunctionId` plus program generation. Reloaded
   callbacks cannot silently retarget by name.
6. The public `husk` facade no longer exports compatibility VM types.

Still required:

1. Build the Red API from one `NativeModule<RedPluginState>` builder instead of
   the hand-written `RED_HOST_DECLARATIONS` plus `Host::call_module` dispatch.
2. Store one public `Instance<RedPluginState>` per Red plugin.
3. Make Red callback registration accept and retain closure-capable
   `FunctionHandle`s, not only named `Callback` values.
4. Preserve all lifecycle/failure-isolation tests while moving transactional
   host policy into the public instance/module ownership model.
5. Delete `Vm`, `Host`, `Callback`, dynamic `Value`, and plugin-shaped lifecycle
   entry points from `husk-runtime` once Red has no callers.

Acceptance checks:

```shell
rg 'red::|CommandMetadata|RequestId' crates/husk-runtime crates/husk
cargo test -p red --lib plugin
cargo test --workspace --all-targets --all-features
```

The first command currently has no production matches.

### Completed gate — R2 resolved call targets

`husk-hir` now defines `FunctionId`, `ModuleFunctionId`,
`IntrinsicMethodId`, `CallTarget`, and `MethodTarget`. Compilation finalization
assigns deterministic FNV-1a IDs, rejects collisions, resolves native calls and
method selections, and builds immutable ID-indexed tables. The evaluator
dispatches those IDs directly. Function values also carry resolved IDs, and
host-retained compatibility callbacks add a program-generation token.

Stable-ID tests cover source declaration reordering and REPL recompilation.
Trait/default-method tests exercise resolved method targets. Ambiguous and
missing implementations fail during semantic analysis.

String-based call and method selection remains only in functions explicitly
named `call_legacy_*` or `resolve_legacy_*`, reached solely from
`SemanticProfile::LegacyJavaScript`. The compile-finalization pass constructs a
type-qualified method spelling once in order to resolve it to an ID; native
execution does not repeat that lookup.

Acceptance check:

```shell
rg 'resolve_script_function_name|method_name = format!|call_named' crates/husk-runtime/src
```

The remaining runtime lookup matches are legacy-only; the other
`method_name = format!` match is in compile finalization.

### R3 — finish patterns and static language coverage

1. Decide in an ADR whether Husk adopts literal and range patterns.
2. If adopted, add AST/parser/semantic/HIR/runtime vertical slices.
3. Permit recursively nested struct-variant patterns and remove
   `check_struct_pattern_depth`.
4. Implement enum exhaustiveness and unreachable-arm diagnostics.
5. Add a generated coverage test that matches every backend-neutral AST variant
   to either HIR lowering or a stable native-profile rejection.

### R4 — close the portable-extension CI proof

1. Add a tiny Rust Component adapter that depends on `regex`.
2. Build it explicitly for `wasm32-wasip2`; ordinary Husk execution must never
   invoke Cargo.
3. Pack, inspect, and execute the bundle on Linux, macOS, and Windows.
4. Run pure, malformed, out-of-fuel, memory-growth, unsupported-type, undeclared
   import, denied capability, and poisoned-instance fixtures.
5. Decide whether to implement the first capability provider. A grant alone
   must never imply broad WASI linkage.

### R5 — finish package publication proof

Publish or stage in this dependency order:

```text
husk-ast
husk-lexer
husk-types
husk-value
husk-diagnostics
husk-parser
husk-hir
husk-semantic
husk-extension
husk-package
husk-wasm
husk-runtime
husk
husk-cli
```

Use either crates.io dry-run staging or a temporary local registry so Cargo can
resolve already packaged internal dependencies. Then build the external smoke
fixture exclusively from packaged sources and verify package file lists contain
no Red paths.

### R6 — final hardening

- add the independence script and Component fixture to supported OS CI;
- run default and no-default features;
- add parser/module/manifest fuzz targets or a documented scheduled fuzz job;
- capture `husk test` output and show it only for failures;
- audit diagnostics at native/Wasm boundaries for host-secret leakage;
- rerun and record the deterministic performance gate after R1/R2.

## Validation commands

Run these from the repository root before handing off Rust changes:

```shell
cargo fmt --all -- --check
cargo test --workspace --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo check -p husk-runtime --no-default-features
sh scripts/check-husk-independence.sh
```

The repository-level `AGENTS.md` requires the strict Clippy command before
pushing. Do not claim P12 complete merely because local validation passes; its
acceptance criteria include cross-platform CI and migration from Red's internal
compatibility API to public Husk instances.

At this checkpoint, all five commands above pass in the
`feat/husk-language-runtime` worktree. The exact all-target test command passes
the Husk unit/integration/CLI/Component suites, parser benchmark targets,
doctests, and all 848 Red library tests. The independence script also builds
and executes the external `regex` fixture from a temporary workspace containing
no Red source.
