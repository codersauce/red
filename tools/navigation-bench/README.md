# Native navigation replay

Build the driver and two release versions of Red. Freeze each executable under a different name before building the next version. Both builds must use the same target, features, compiler, profile and dependency lock. When using separate checkouts, give them separate target directories or verify Cargo actually rebuilt the Red package. Record the source revision, patch and binary hashes.

```sh
cargo build --locked --release --manifest-path tools/navigation-bench/Cargo.toml --target-dir target
python3 tools/navigation-bench/compare.py \
  --driver target/release/red-navigation-bench \
  --base /path/to/base/red --candidate /path/to/candidate/red \
  --file /path/to/frozen/src/editor.rs --output /path/to/new/results
```

On Windows use the native `.exe` paths and run the same command inside the guest. Verify both binaries start with `--version` first. For an older baseline that sends graphics probe replies into the Windows input queue, pass `--config-override "markdown_images='off'"` to both variants, then run the candidate alone with its default settings.

The runner warms up each executable, alternates which variant goes first, and retains seven measured pairs for each scenario. Use `--scenarios inside scroll reverse window buffer` to select workloads. The manifest records the binaries, fixture, driver, options, and hashes of any candidate runtime overlay. An overlay on the same executable isolates plugin changes. Use two separate builds to measure host changes.

Each run opens Red on a fresh copy of the fixture in a 120 × 40 PTY or ConPTY. Bundled plugins are enabled. The driver disables LSP and release checks, fixes the statusline to show the active cursor position and uses an unshifted command-mode alias for setup and teardown. Window switching uses Ctrl-w w between two files. Buffer switching uses Space n. The files have different remembered cursor positions. After timing, the driver inserts a marker, saves, and checks the full contents of both files in the switching cases.

The driver measures from the write of a navigation command until its expected position appears in the parsed terminal output. It records injection lateness separately. With the default batch size of one it observes every position. With `--batch 64`, it sends same-direction input on schedule and checks each batch boundary. The reported latency then starts at the last send in the batch. Each run emits wall-clock duration, observed output bytes, operation count, latency percentiles and counts exceeding 8, 16 and 32 ms. The comparison reports the median of the per-run p95s. Compare on one machine and inspect injection lateness and individual runs before interpreting small differences.

For a short attribution run, give the driver `--trace /absolute/path/to/trace.log`. Trace records include dispatched input IDs, successful flush IDs, the cursor (zero-based editor column and buffer line), viewport origin, stable buffer ID and revision. A flush may occur during an event, and one event can cause multiple flushes. Dispatch ages start after dequeue. The timestamps describe editor and terminal-stream progress. Visible presentation and SSH/RDP delivery need their own measurements.

Use `--bounded /absolute/path/to/metrics.log` for a long diagnostic run. The `[PERF] bounded` JSON contains per-span counts for inclusive microsecond bucket boundaries and the maximum sample, plus counters and gauges. This mode aggregates timings under static names. Keep tracing disabled for acceptance comparisons.

```sh
target/release/red-navigation-bench --binary /path/to/candidate/red \
  --scenario reverse --presses 512 --batch 64 --interval-ms 0 \
  --bounded /tmp/red-navigation.log
```

For the populated Agent and NeoTree workspace on Unix, use `scripts/keyboard_scroll_tmux.py` for retained terminal captures and `scripts/keyboard_scroll_bench.py --sample-phase down` on macOS for a process sample. Run both executables on the same fixture with the same terminal size. The CI replay checks correctness on each OS. Keep elapsed-time gates on controlled machines.
