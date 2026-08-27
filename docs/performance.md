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

Detached owners expose a separate lightweight render audit. `detach:idle_tick` counts background polls that correctly skipped serialization, `detach:rendered_tick` counts polls that produced a frame, `detach:serialize_frame` measures row/span serialization, and `detach:changed_rows` reports the maximum number of rows sent in one delta. These metrics complement, rather than replace, the native motion-frame gate above.

## Repeated editing

Run `python3 scripts/edit_replay_bench.py --binary target/release/red` for dot,
macros, counted deletion, visual-block insertion, paste, substitute, indentation,
and undo. Each case uses a real PTY and verifies the saved result. Add `--plugins`
and `--split` to include bundled callbacks and shared-buffer windows. Compare
exact-base and feature binaries on the same machine; the script reports complete
input-event time, frame counts, highlight misses, and change notifications.
Use `--lsp incremental` or `--lsp full` to launch a local protocol fixture that
reconstructs the document and reports notification/payload counts. `--unicode`
and `--crlf` exercise UTF-16 coordinates and Windows line endings without an
installed language server.

`cargo run --locked --release --example textarea_replay_bench` separately measures
large embedded text areas. Neither benchmark imposes a wall-clock CI threshold.
The deterministic replay tests instead assert publication counts, final frames,
notification ordering, cancellation, and undo correctness. `RED_PERF=summary`
also reports `edit:replacements`, `edit:change_notifications`,
`edit:notifications_deferred`, `edit:replay_slices`, `edit:inline_summary_refreshes`,
and full/incremental LSP byte counters.

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
python3 scripts/detach_bench.py 50 120 120 1536
python3 scripts/interaction_bench.py typing
python3 scripts/interaction_bench.py search --query self
python3 scripts/interaction_bench.py picker --query src/editor.rs
python3 scripts/interaction_bench.py signature --cycles 40
python3 scripts/git_workspace_bench.py --files 80 --presses 120
```

The detach driver creates an isolated config and Unicode-heavy buffer, disables LSP,
exercises edits, mouse click/scroll, repeated resizes, a 1.5 MiB bracketed paste, and
reattach, then reports wall time, output volume, and all `detach:*` samples/counters.
The interaction driver uses the same isolated profile and reports process-launch-to-first-paint,
event/render percentiles, terminal output, and log volume while typing alternating ASCII/Unicode,
editing an incremental search query, or repeatedly filtering a picker with a file preview. Use
`--file`, `--root`, `--rows`, `--cols`, and `--config-override` to exercise large repositories,
single-line files, wrapping, and other representative layouts. For example:

The Git workspace driver creates an isolated repository with many modified Rust files, rapidly
moves through the file list, then repeats the same motion in the diff pane. It reports frame and
plugin-callback percentiles and fails if selection churn starts more than two subprocesses, core
diff navigation starts any subprocess, or the Git plugin exceeds its process budget.

```shell
python3 scripts/interaction_bench.py picker \
  --root ../codex \
  --file ../codex/codex-rs/tui/src/bottom_pane/chat_composer.rs \
  --query chat_composer.rs
python3 scripts/interaction_bench.py signature \
  --root ../codex \
  --file ../codex/codex-rs/tui/src/bottom_pane/chat_composer.rs
RED_FILE_PICKER_BENCH_ROOT=../codex \
  cargo test --release --lib file_picker_large_workspace_performance -- --ignored --nocapture
RED_FILE_PICKER_BENCH_ROOT=../openai RED_FILE_PICKER_VERIFY_PARITY=1 \
  cargo test --release --lib file_picker_streaming_large_workspace_performance \
  -- --ignored --nocapture --test-threads=1
cargo run --release --example performance_hotspots -- completion
cargo run --release --example performance_hotspots -- completion-backspace
RED_COMPLETION_BENCH_FILE=../codex/codex-rs/tui/src/bottom_pane/chat_composer.rs \
  cargo run --release --example performance_hotspots -- buffer-completion
```

The streaming picker benchmark measures the actual discovery/query/UI handoff,
including first results, complete discovery, cached reopen, query completion,
input handling, rendering, and cancellation. It hashes the complete file set;
`RED_FILE_PICKER_VERIFY_PARITY=1` also runs the original serial walker and checks
that every eligible path is preserved. The older picker benchmark remains useful
for isolating synchronous matching over a fixed list. Run performance measurements
without other builds or scans competing for resources. A fresh application index
does not imply a cold filesystem cache.

File discovery uses at most eight walkers and an eight-batch queue, with at most
512 paths per batch. Matching and sorting run outside the UI thread. The picker
shows partial results and progress until discovery completes; errors retain any
partial results with an explicit incomplete status. Enter waits for the current
query rather than selecting a row from an older query. `Ctrl+r` refreshes the index;
`Ctrl+e` changes hidden/ignored visibility and starts a separate scan.
Automatic selection follows the best match as files arrive. Once the user navigates
the results, updates preserve the selected path until the query changes.

Completed file indexes are shared within an editor, keyed by canonical root and
visibility. They expire after 30 seconds and refresh on the next open, keeping the
old results searchable until the replacement finishes. Known creates, deletes,
renames, and ignore-rule changes invalidate the relevant indexes. Ordinary saves
of already indexed source files do not trigger rescans. External changes appear
after explicit refresh or expiry; no recursive polling of the monorepo is added.
The cache evicts unused indexes beyond four roots/options or 1 GiB of estimated
row storage. Active indexes are never truncated, and refresh can temporarily hold
both old and new snapshots. This is a cache budget, not a process memory limit.

For an interactive detach audit with real plugins/background updates, start an owner
with performance summaries enabled, leave it idle briefly, exercise the same paths,
then detach/reattach and stop it:

```shell
RED_PERF=summary target/release/red --detach=perf-check src/editor.rs
# Press Ctrl-\ after the interaction pass.
RED_PERF=summary target/release/red --attach perf-check
target/release/red --stop perf-check
```

The owner's log should show `detach:idle_tick` increasing while idle with no matching serialization work. During interaction, `detach:rendered_tick` and `detach:serialize_frame` should track actual updates, and ordinary input should keep `detach:changed_rows` well below the terminal height. A full-height delta is expected on connect, resize, or an intentional full repaint.

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

The interaction driver additionally records launch-to-first-paint (which includes file/config
loading before `startup:interactive`), typing/search/picker/signature p50/p95/p99/max, and
log volume. The signature scenario starts a deterministic local LSP fixture, displays an
actual parameter popup, and verifies that every requested character reached the document.
`completion-backspace` measures bounded query-history reuse, while `buffer-completion`
measures a complete no-match scan of either its synthetic document or
`RED_COMPLETION_BENCH_FILE`.

Release thresholds are relative to the most recent baseline on the same machine:

- startup and plugin-startup p95/point measurements: no more than 25% slower;
- keypress-to-render and large-file motion p95: below 16 ms and no more than 20% slower;
- typing, incremental search, and picker keypress-to-render p95: below 16 ms;
- bundled Husk callback p95: below 4 ms;
- output bytes per 200-key scroll window: no more than 25% growth unless the release
  intentionally changes the rendered frame.

A threshold failure blocks the release until it is explained, reproduced, and either
fixed or accepted in the baseline document with the responsible change linked. Do not
refresh a baseline solely to make a regression disappear.
