# Husk WebAssembly Component feasibility results — 2026-07-19

## Outcome

The typed WebAssembly Component path passes the local feasibility gate and is
implemented behind the `husk-runtime/wasm-extensions` feature. The release gate
remains conditional on the existing Linux, macOS, and Windows CI matrix passing
the same tests.

This validates the architecture, not arbitrary dynamic loading of Cargo crates.
An extension is a small adapter crate compiled as a WebAssembly Component. The
host dynamically loads that component and derives a Husk module signature from
its Component Model exports.

## Environment

- Host: Apple Silicon macOS, `aarch64-apple-darwin`
- Rust: 1.96.0
- Wasmtime: 46.0.1, with a deliberately small feature set
- Profile: `--release`
- Component: one pure `add(s32, s32) -> s32` export
- Measurement harness:
  `cargo run --release -p husk-wasm --example feasibility`

These numbers are observations from one development machine. They are not
performance promises or regression thresholds.

## Measurements

Five warm-process launches after the initial release build produced:

| Observation | Result |
| --- | ---: |
| Component text input | 361 bytes |
| Cold compile, first launch after build | 9,553 µs |
| Compile, median of five subsequent launches | 829 µs |
| Instantiate, median | 9 µs |
| 100,000 dynamic calls, median | 21,562 µs |
| Mean dynamic call, median run | 215 ns |
| Unstripped release harness binary | 14,752,064 bytes |
| Maximum resident set reported by `/usr/bin/time -l` | 8,978,432 bytes |

The call loop includes Husk `OwnedValue` to Component `Val` conversion,
indexed dynamic export dispatch, resetting fuel, the guest call, and result
conversion. It does not include Husk AST interpretation.

## Functional evidence

`husk-wasm` tests cover:

- root functions and exported interface instances discovered dynamically;
- a function unknown at host build time called through Component `Val`;
- `bool`, `s32`, `s64`, `float64`, `string`, `u8`, list, tuple, option,
  result, record, enum, and variant mappings;
- checked `u8` range conversion;
- deterministic kebab-case to snake_case normalization and collision failure;
- explicit rejection of lossy or unsupported version-1 WIT types;
- pure components with no linked WASI imports;
- `actual imports ⊆ requested capabilities ⊆ granted capabilities`;
- granted imports still receiving no implementation merely because they were
  granted;
- fuel interruption and poisoning only the failed component instance;
- linear-memory/resource limiting during instantiation;
- one compiled component shared across multiple isolated stores;
- descriptor-driven type checking and dispatch through the public Husk
  `Engine`.

The standalone CLI test additionally covers `extension pack`, `extension
inspect`, and `run --extension` end to end.

## Important limitations

- Version 1 links no capability providers. Pure components work; a declared and
  granted import is still rejected at instantiation until a narrow provider is
  implemented.
- Wasmtime exposes structural Component types. Two exported WIT aliases with
  identical shapes cannot always be distinguished nominally through dynamic
  reflection. Husk chooses the first deterministic exported name for an equal
  shape and retains every exported definition in the descriptor.
- Records, enums, and variants used in function signatures must also be
  exported as named WIT types. Anonymous nominal shapes are rejected.
- Wall-clock cancellation is not implemented. Deterministic fuel and store
  resource limits are implemented.
- The local measurement is macOS-only. CI is the source of truth for the
  three-platform portability gate.

## Reproduction

```shell
cargo test -p husk-wasm
cargo test -p husk --test wasm_extension
cargo test -p husk-extension -p husk-cli
cargo run --release -p husk-wasm --example feasibility
```

The workspace CI matrix already runs all-target/all-feature tests on Ubuntu,
macOS, and Windows, so the new component tests become required on all three
hosts.
