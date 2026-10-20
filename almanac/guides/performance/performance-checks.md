---
title: "Performance Checks"
summary: "This guide explains how to run Red's deterministic CI performance gate and the workstation benchmarks used to judge editor, detached-session, interaction, completion, file-picker, and Git workspace regressions."
topics: [guides, performance, validation, editor, husk]
sources:
  - id: performance-doc
    type: file
    path: docs/performance.md
  - id: performance-workflow
    type: file
    path: .github/workflows/performance.yml
  - id: scroll-bench
    type: file
    path: scripts/scroll_bench.py
  - id: detach-bench
    type: file
    path: scripts/detach_bench.py
  - id: interaction-bench
    type: file
    path: scripts/interaction_bench.py
  - id: git-workspace-bench
    type: file
    path: scripts/git_workspace_bench.py
  - id: edit-replay-bench
    type: file
    path: scripts/edit_replay_bench.py
  - id: textarea-replay-bench
    type: file
    path: examples/textarea_replay_bench.rs
  - id: performance-hotspots
    type: file
    path: examples/performance_hotspots.rs
---

Use this guide when a Red change may affect editor latency, rendering, plugin callbacks, detached sessions, repeated editing, completion, file discovery, or Git workspace responsiveness. Red has one deterministic CI performance gate for the bundled Husk cursor callback path, and a separate workstation runbook for end-to-end editor behavior [@performance-workflow] [@performance-doc]. The key rule is to compare the right path: do not treat a fast Husk callback as proof that rendering is fast, and do not refresh a workstation baseline merely to hide a regression [@performance-doc].

## Run The Deterministic Gate

The shared CI gate is:

```shell
cargo run --locked --release --example husk_cursor_bench -- --assert
```

The path-filtered Performance workflow runs that command in the `perf` job on Ubuntu for relevant pushes and pull requests, scheduled weekly runs, and manual dispatch [@performance-workflow]. The performance guide states that the fixture, viewport, warmup, iteration count, and 4 ms p95 ceiling are fixed, and that this is the only wall-clock performance check enforced on shared CI runners because it has enough margin to avoid ordinary host variance becoming flaky [@performance-doc].

This gate isolates the Husk scripting engine and the hottest bundled cursor plugin. It fails when `indent_guides.hk` exceeds 4 ms at p95 [@performance-doc]. The release benchmark is also the bytecode decision gate: do not add a compiler or bytecode VM while callback p95 remains below 4 ms; profile the editor frame path instead [@performance-doc]. For Husk embedding context, see [Husk Public Embedding API](../../architecture/husk/public-embedding-api).

## Run The Workstation Baseline

Before release or after a risky performance change, run the workstation checks on the same reference machine, plugged into power, with no competing build [@performance-doc]:

```shell
cargo build --locked --release
cargo run --locked --release --example husk_cursor_bench -- --assert
python3 scripts/scroll_bench.py 50 120 200 25
python3 scripts/detach_bench.py 50 120 120 1536
python3 scripts/interaction_bench.py typing
python3 scripts/interaction_bench.py search --query self
python3 scripts/interaction_bench.py picker --query src/editor.rs
python3 scripts/interaction_bench.py signature --cycles 40
python3 scripts/edit_replay_bench.py --binary target/release/red
python3 scripts/git_workspace_bench.py --files 80 --presses 120
cargo run --locked --release --example textarea_replay_bench
cargo run --locked --release --example performance_hotspots -- completion
cargo run --locked --release --example performance_hotspots -- completion-backspace
```

The editor frame gate is not the same as the Husk callback gate. The performance guide says to run the editor with `RED_PERF=summary cargo run --release`, hold `j` and `k` in a representative Rust file for at least five seconds, then confirm `render:motion_delta`, `render:motion_frame`, and `husk:notify cursor:moved` remain below 16 ms at p95 [@performance-doc]. Rendering details live in [Rendering Pipeline](../../architecture/editor/rendering-pipeline).

## Understand The PTY Drivers

`scripts/scroll_bench.py` drives `target/release/red` in a PTY, disables LSP through `--config-override`, opens `src/editor.rs`, enables `RED_PERF=trace`, holds `j`, and reports startup timings, event/render samples, plugin notifications, wall time, and terminal output volume from the marked measurement window [@scroll-bench]. Use it when large-file scrolling or terminal output size is suspicious.

`scripts/detach_bench.py` builds a Unicode-heavy temporary buffer, starts a detached owner with LSP disabled, performs edits, mouse actions, resizes, a bracketed paste, detach, attach, and stop, then reports `detach:*` timings, counters, gauges, wall time, and output volume [@detach-bench]. The performance guide expects `detach:idle_tick` to rise while idle without matching serialization work, and expects `detach:rendered_tick` and `detach:serialize_frame` to track actual visible updates [@performance-doc].

`scripts/interaction_bench.py` measures user-visible typing, search, picker, and signature-help latency. It waits for first paint, marks a benchmark window in the log, runs the selected scenario, and reports first-paint time, output volume, log volume, startup samples, and p50/p95/p99/max timings [@interaction-bench]. Its signature scenario starts a deterministic local LSP fixture, displays a parameter popup, and verifies that every requested character reached the document [@interaction-bench]. It accepts `--file`, `--root`, `--rows`, `--cols`, and repeated `--config-override` arguments for representative repositories and layouts [@interaction-bench].

`scripts/edit_replay_bench.py` measures dot repeat, macros, counted deletion, visual-block insertion, paste, substitute, indentation, and undo through a real PTY, verifies the saved result, and can add bundled callbacks, shared-buffer splits, Unicode, CRLF, or a local LSP sync fixture [@edit-replay-bench]. `examples/textarea_replay_bench.rs` measures large embedded text-area replay with assertions but no timing threshold [@textarea-replay-bench]. `examples/performance_hotspots.rs` isolates editor-thread hotspots such as completion, completion backspace, and buffer completion without replacing the end-to-end PTY checks [@performance-hotspots] [@performance-doc].

`scripts/git_workspace_bench.py` creates a temporary Git repository with many modified Rust files, opens Red with LSP disabled, runs `:GitDashboard`, measures row movement in the file list and diff pane, and fails if file-list churn spawns more than two Git processes, core-owned diff navigation spawns any Git process, or the Git plugin hits its process budget or quarantine path [@git-workspace-bench].

For streaming file-picker work, use the ignored release-mode benchmark named `file_picker_streaming_large_workspace_performance` with a representative `RED_FILE_PICKER_BENCH_ROOT`; set `RED_FILE_PICKER_VERIFY_PARITY=1` when the change could drop eligible paths, because the benchmark compares the streaming walker against the serial walker [@performance-doc].

## Record And Judge Results

Record the date, commit, OS, architecture, CPU, memory, Rust version, build profile, and all reported samples in a dated `docs/performance-baseline-YYYY-MM-DD.md` file [@performance-doc]. Compare release results against the most recent baseline on the same machine.

The release thresholds are relative to that baseline: startup and plugin-startup measurements may be no more than 25 percent slower, keypress-to-render and large-file motion p95 must be below 16 ms and no more than 20 percent slower, typing, search, and picker p95 must be below 16 ms, bundled Husk callback p95 must be below 4 ms, and terminal output bytes per 200-key scroll window may grow no more than 25 percent unless the release intentionally changes the rendered frame [@performance-doc]. The newer repeated-editing, textarea, signature-help, streaming file-picker, and completion hotspot checks are diagnostic gates unless the release changes their corresponding baseline or test assertion [@performance-doc].

A threshold failure blocks the release until it is explained, reproduced, and either fixed or accepted in the baseline document with the responsible change linked [@performance-doc]. Use [Build, Test, And Validate](../development/build-test-and-validate) for the broader validation pass after a performance fix.
