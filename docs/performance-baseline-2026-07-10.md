# Performance baseline — 2026-07-10

This is the Phase 0 reference baseline for Red's release-performance runbook. Compare
future measurements only on equivalent hardware and with the same commands; `RED_PERF`
trace logging intentionally adds overhead but makes regressions comparable.

## Build and machine

- Commit: `ab02fd2ce77ba2b524521d476bcb293be14f62f7`
- Profile: Cargo `release`, locked dependencies
- Red: `0.1.1`
- OS: macOS 26.5.2 (25F84), Darwin 25.5.0
- Hardware: MacBook Pro Mac16,5, Apple M4 Max, 16 cores, 128 GB memory
- Architecture: `aarch64-apple-darwin`
- Rust: `rustc 1.94.1 (e408947bf 2026-03-25)`, LLVM 21.1.8
- Cargo: `1.94.1 (29ea6fb6a 2026-03-24)`
- Power/load: AC power; no competing build intentionally running

## Deterministic Husk callback

Command:

```shell
cargo run --locked --release --example husk_cursor_bench -- --assert
```

The fixture used 200 warmup and 2,000 measured callbacks with a 48-row representative
viewport and the production 100,000-instruction plugin ceiling.

| Metric | Result | Gate |
|--------|-------:|-----:|
| p50 | 1.499 ms | — |
| p95 | 1.674 ms | 4.000 ms |
| p99 | 1.746 ms | — |
| max | 3.243 ms | — |
| host effects | 2,201 | — |

Result: pass, with 2.326 ms of p95 headroom.

## Interactive startup and large-file scroll

Command:

```shell
python3 scripts/scroll_bench.py 50 120 200 25
```

The isolated profile disabled LSP, loaded all 13 bundled plugins, opened the 14,838-line
`src/editor.rs`, moved into the file, and sent 200 `j` keys at 25 ms intervals through a
50×120 PTY. Process/plugin activity remained enabled so this represents the shipped
default plugin set rather than a stripped editor.

| Metric | Samples | Mean | p95 | Max |
|--------|--------:|-----:|----:|----:|
| interactive startup through first frame | 1 | 21.098 ms | 21.098 ms | 21.098 ms |
| bundled plugin startup and ready handlers | 1 | 8.226 ms | 8.226 ms | 8.226 ms |
| key event through action/render path | 200 | 3.294 ms | 3.982 ms | 51.331 ms |
| `cursor:moved` Husk notification | 199 | 1.444 ms | 2.041 ms | 4.010 ms |
| full render | 199 | 1.274 ms | 1.600 ms | 25.078 ms |
| motion frame render | 188 | 0.866 ms | 1.229 ms | 2.341 ms |
| window rendering | 199 | 0.727 ms | 1.155 ms | 3.433 ms |
| render diff and terminal flush | 199 | 0.368 ms | 0.230 ms | 23.694 ms |

- Measured window wall time: 8.2 s, including the 2 s drain period.
- Terminal output during the window: 1,964 KiB.
- Keypress-to-render p95 gate: pass at 3.982 ms against 16 ms.
- Large-file motion p95 gate: pass at 1.229 ms against 16 ms.

The isolated high maxima did not affect p95 and coincided with background bundled-plugin
process events. They remain recorded so a future baseline can distinguish a systematic
tail regression from one-off scheduling noise.
