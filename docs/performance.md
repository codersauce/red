# Performance checks

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
