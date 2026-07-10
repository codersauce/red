# Performance checks and release gate

Red has two separate performance gates for cursor movement:

- The Husk callback gate isolates the scripting engine and the hottest bundled
  cursor plugin. Run `cargo run --release --example husk_cursor_bench -- --assert`.
  The benchmark fails when `indent_guides.hk` exceeds 4 ms at p95.
- The editor frame gate covers input, plugin notifications, rendering, and
  terminal flushes. Run the editor with `RED_PERF=summary cargo run --release`,
  hold `j` and `k` in a representative Rust file for at least five seconds,
  then quit. `render:motion_delta`, `render:motion_frame`, and
  `husk:notify cursor:moved` should remain below 16 ms at p95.

Use `RED_PERF=trace` only for short investigations. It logs every sample and
can perturb the path being measured.

The release benchmark is the bytecode decision gate. Do not add a compiler or
bytecode VM while the callback p95 remains below 4 ms; profile the editor frame
path instead.

## Deterministic CI gate

CI runs:

```shell
cargo run --locked --release --example husk_cursor_bench -- --assert
```

The fixture, viewport, warmup, iteration count, and 4 ms p95 ceiling are fixed. This is
the only wall-clock performance check enforced on shared CI runners because its budget
has enough margin to avoid turning ordinary host variance into flaky builds.

## Pre-release workstation runbook

Run on the same reference machine, while plugged into power and with no competing build:

```shell
cargo build --locked --release
cargo run --locked --release --example husk_cursor_bench -- --assert
python3 scripts/scroll_bench.py 50 120 200 25
```

Record the date, commit, OS, architecture, CPU, memory, Rust version, build profile, and
all reported samples in a dated `docs/performance-baseline-YYYY-MM-DD.md` file. The PTY
driver isolates its Red config, disables LSP, uses `src/editor.rs` as the large file,
and records:

- `startup:interactive`: terminal setup through the first complete frame;
- `startup:plugins`: Husk VM, all bundled plugin loads, and `editor:ready` handlers;
- `event`: keypress processing;
- `render:motion_delta` / `render:motion_frame`: large-file scroll rendering;
- `husk:notify cursor:moved`: hot plugin callbacks; and
- wall time plus terminal output volume for the scrolling window.

Release thresholds are relative to the most recent baseline on the same machine:

- startup and plugin-startup p95/point measurements: no more than 25% slower;
- keypress-to-render and large-file motion p95: below 16 ms and no more than 20% slower;
- bundled Husk callback p95: below 4 ms;
- output bytes per 200-key scroll window: no more than 25% growth unless the release
  intentionally changes the rendered frame.

A threshold failure blocks the release until it is explained, reproduced, and either
fixed or accepted in the baseline document with the responsible change linked. Do not
refresh a baseline solely to make a regression disappear.
