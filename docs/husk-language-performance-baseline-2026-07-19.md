# Husk language extraction performance baseline — 2026-07-19

This is the pre-extraction callback baseline required by Phase 0 of the
[Husk language extraction plan](HUSK_LANGUAGE_EXTRACTION_PLAN.md). It records
the existing threshold and behavior without changing either.

## Environment

- Base commit: `b6442b6e38e63ef7062c9c8c68e0a82e9e859193`
- Branch: `feat/husk-language-runtime`
- Profile: Cargo `release`, locked dependencies
- Architecture: `aarch64-apple-darwin`
- OS kernel: Darwin 25.4.0
- Hardware: Apple M5 Max, 128 GiB memory
- Rust: `rustc 1.96.0 (ac68faa20 2026-05-25)`, LLVM 22.1.2
- Cargo: `1.96.0 (30a34c682 2026-05-25)`

## Deterministic Husk callback

Command:

```shell
cargo run --locked --release --example husk_cursor_bench -- --assert
```

The fixture used 200 warmup and 2,000 measured callbacks, with 2,201 recorded
host effects.

| Metric | Result | Existing gate |
| --- | ---: | ---: |
| p50 | 1.163 ms | — |
| p95 | 1.235 ms | 4.000 ms |
| p99 | 1.359 ms | — |
| max | 1.598 ms | — |

Result: pass, with 2.765 ms of p95 headroom.

Future measurements are comparable only on equivalent hardware and with the
same command. This snapshot is evidence for behavior preservation, not a new
performance promise.

## Post-extraction optimization — 2026-07-20

The extracted runtime was measured on the same Apple M5 Max with the same Rust
1.96.0 toolchain. An initial six-run comparison confirmed a callback
regression:

| Runtime | Median p50 | Median p95 |
| --- | ---: | ---: |
| Pre-extraction commit | 1.167 ms | 1.236 ms |
| Extracted runtime before optimization | 1.254 ms | 1.331 ms |

Sampling attributed material callback time to recursively cloning and dropping
compiled HIR functions, traversing heap values, and heap-backed local storage.
Two low-risk changes recovered the regression:

- loaded function tables now share immutable functions through `Arc`, so a
  call does not recursively clone its HIR;
- a top-level call skips garbage collection when the runtime heap has no live
  objects.

Six alternating pre-extraction/optimized runs controlled for ambient machine
load:

| Runtime | Median p50 | Median p95 | p95 change |
| --- | ---: | ---: | ---: |
| Pre-extraction commit | 1.150 ms | 1.212 ms | — |
| Optimized extracted runtime | 1.061 ms | 1.106 ms | -8.7% |

Every optimized run remained below the existing 4 ms p95 gate. Alternating
runs are reported because absolute latency varied under concurrent system load;
the optimized runtime was faster in every paired p50 run and every paired p95
run.

### Loaded-plugin memory

The Red compatibility VM originally retained the complete compiler artifact
for every loaded plugin, including syntax and semantic maps. Red now retains a
smaller executable `LoadedProgram`; the public `CompiledModule` continues to
retain compiler metadata for embedding and tooling clients.

Peak resident memory from the release benchmark's load-only mode:

| Loaded plugins | Before | After | Change |
| --- | ---: | ---: | ---: |
| 1 | 15.6 MB | 15.8 MB | Startup-dominated |
| 10 | 24.5 MB | 19.2 MB | -21.6% |
| 50 | 63.9 MB | 34.2 MB | -46.4% |

Reproduce the scaling measurement with:

```shell
cargo build --locked --release --example husk_cursor_bench
/usr/bin/time -l target/release/examples/husk_cursor_bench --load-only --plugins=50
```

### Red build footprint

Red no longer depends on the public `husk` facade merely to format one parser
diagnostic. It uses `husk-runtime` directly, without default features, while
the standalone `husk` facade and CLI retain WebAssembly Component support.
Consequently, `cargo tree -p red` no longer contains Wasmtime. The release
callback benchmark binary decreased from 4,418,896 to 4,237,472 bytes in this
worktree; link-time dead-code elimination means clean build dependency cost is
the more important benefit.

Clean-target release builds of the callback benchmark took 34.86 seconds at
the pre-extraction commit and 37.85 seconds after these changes. The remaining
8.6% increase reflects the larger compiler/runtime implementation even without
Wasmtime in Red's graph; it is a build-time cost, not editor callback latency.
