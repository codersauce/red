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
