# Quit latency, 2026-08-18

Baseline: `7a668a4cd812da88d53049f320e5c8dfcc04a09c`. Measurements use
release builds on macOS, a 100-column PTY, disposable configuration and files,
the bundled plugins, and disabled AI/LSP. The timer starts when the benchmark
sends Enter to an already-rendered `:q` command. Screen restoration means the
PTY received `LeaveAlternateScreen`, not that a physical display painted it.
Process exit is measured separately. Filesystem and scheduling noise affect the
results; these are local samples, not a latency guarantee.

## Results

| Build / workload | Runs | Screen median | Screen max | Process median |
| --- | ---: | ---: | ---: | ---: |
| Original, 100 lines | 7 | 39.322 ms | 210.427 ms | 40.858 ms |
| Original plus phase instrumentation, 100 lines | 5 | 15.659 ms | 20.408 ms | 18.689 ms |
| Cleanup optimizations only, 100 lines | 5 | 19.621 ms | 238.885 ms | 22.510 ms |
| Early terminal restore, 100 lines | 10 | 1.006 ms | 1.110 ms | 21.687 ms |
| Original, 100,000 lines | 3 | 25.715 ms | 47.445 ms | 28.766 ms |
| Early terminal restore, 100,000 lines | 5 | 1.274 ms | 3.135 ms | 30.506 ms |
| Early terminal restore, 200 exit-hook storage updates | 3 | 1.362 ms | 1.392 ms | 23.408 ms |
| Final, including deferred quit history, 100 lines | 10 | 0.605 ms | 1.092 ms | 21.556 ms |
| Final, 100,000 lines | 5 | 0.742 ms | 1.113 ms | 31.715 ms |
| Final, forced quit after rejected dirty quit | 30 | 0.593 ms | 1.086 ms | 23.114 ms |
| Final, 200 exit-hook storage updates | 3 | 0.638 ms | 0.730 ms | 19.727 ms |
| Early restore with synchronous history, 8 MiB preferences | 3 | 6.097 ms | 10.921 ms | 50.735 ms |
| Final, 8 MiB preferences | 3 | 0.612 ms | 0.696 ms | 29.880 ms |

The instrumented original spent 10.8–16.7 ms persisting its final recovery
snapshot and about 1.8 ms deactivating plugins. Those writes include the
existing atomic replacement, file sync, and directory sync. Removing that
durability would be the wrong tradeoff.

The cleanup-only changes batch plugin preference updates into one write and
start agent shutdown before doing independent cleanup. A controlled plugin
issuing 200 storage updates reduced the storage phase from 22.5–47.1 ms to
0.252–0.291 ms. Snapshot sync still produced a 235 ms outlier, so these
optimizations alone do not reliably remove the visible pause.

The interactive lifecycle now restores terminal modes before exit hooks,
recovery persistence, plugin teardown, and external-process waits. It still
waits for all cleanup before exiting. Repeated terminal cleanup is harmless;
the detached core does not acquire or restore interactive terminal ownership.
An event-loop error also reaches service cleanup, and a terminal-restoration
error does not prevent the recovery snapshot.

## Reproduce

```sh
cargo build --locked --release --bin red
python3 scripts/quit_bench.py --binary target/release/red --runs 10
python3 scripts/quit_bench.py --binary target/release/red --lines 100000
python3 scripts/quit_bench.py --binary target/release/red --storage-updates 200
python3 scripts/quit_bench.py --binary target/release/red --preference-kib 8192
python3 scripts/quit_bench.py --binary target/release/red --dirty
```

The dirty-buffer scenario verifies that ordinary `:q` stays in the editor,
`:q!` restores the terminal exactly once, the source file remains unchanged,
and the final recovery snapshot contains the unsaved edit. `RED_PERF=trace`
now also records `shutdown:*` phases. The benchmark retains slow spans after
the final Enter so pre-shutdown delays can be distinguished from cleanup.
The final harness waits for completed key-event traces rather than relying on
fixed sleeps between Escape, command entry, and Enter.

Early restoration alone exposed a second pre-shutdown cost. An initial
dirty-buffer sample included a 405.887 ms outlier; a subsequent 40-run sample
recorded 278 and 627 ms inside the Enter action. A narrower trace confirmed
synchronous command-history persistence on that path. With a controlled 8 MiB
preferences file, this write took 4.4–10.2 ms before restoration could start.
Quit-bearing commands now update history in memory, then flush it with the
exit-hook preferences after an accepted quit. Rejected or failed commands
still save their history before returning. The original outliers remain in
the evidence; host scheduling and filesystem behavior can still vary.

The final 30-run dirty-buffer sample restored the screen in 0.593 ms median
and 1.086 ms maximum. Its slowest process exit was 390.667 ms: recovery still
finishes before exit, but no longer keeps the alternate screen visible.

Validation: the all-targets/all-features Rust suite passed 2,844 tests, with
one ignored. Strict Clippy, focused
shutdown-order/error regressions, and a retained tmux walkthrough of clean,
rejected, and forced quit. Active network-backed AI/LSP shutdown latency is
not represented by these isolated PTY measurements.
