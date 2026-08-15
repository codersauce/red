# Red scrolling and mouse performance — 2026-08-15

Historical baseline. See [the implementation and matched follow-up](performance-scroll-improvements-2026-08-15.md)
for the completed fixes and new measurements.

## Outcome

The current release build does unnecessary full-frame work for mouse motion,
and editor splits multiply the frames produced by scrolling. The issue is
measurable with a populated Agent pane, but these measurements do not establish
which recent commit made it noticeable. Do not describe this as a confirmed
regression in the latest tree-sitter change.

Worktree: `red.codex-overall-performance`, branch `codex/overall-performance`.
Baseline: `a8225610c3e024352019d3dca52a430855234a27` (fresh `origin/main`).
Machine: Apple M4 Max, 128 GiB RAM, Darwin 25.6.0 arm64,
Rust 1.94.1 (`e408947bf`), `cargo build --locked --release --bin red`.
Baseline binary SHA-256:
`f3c01278f6e6fb88fb769e421dbd648614106c9d9676d72c116b6a22ecbe1a17`.

## Reproducible condition

`scripts/workspace_scroll_bench.py` drives the real terminal editor through a
continuously drained 200-column × 60-row PTY with truecolor enabled. It uses
isolated configuration, bundled plugins, and disabled LSP. The main file is
`src/editor.rs` (1,421,300 bytes; 37,475 lines at the baseline).

The three layouts are:

1. Highlighted Rust editor only.
2. The same editor plus a populated Agent pane.
3. Two highlighted Rust editor splits (`editor.rs` and `editor/rendering.rs`),
   NeoTree, and the populated Agent pane.

The Agent is Red's real UI and protocol bridge, backed by the script's local,
deterministic app-server fixture. It renders twelve Markdown sections with Rust
code blocks and lists. It does not contact a model or read user conversations.
The measured Agent is populated and idle, not actively streaming.

Each phase starts near line 100. The stress input is 200 events at 5 ms spacing:
idle, mouse movement, held `j`, wheel-down, then wheel-down with three mouse
moves per wheel. Thus the combined phase injects 800 events in approximately
one second. CPU figures include queue drain and a short settling interval.
Per-event times include Red's handler/render work, not physical-display latency.

## Baseline results

Each cell is the median of three runs. Times are milliseconds unless labeled.
The three-repeat trace run uses `RED_PERF=trace`; the separate CPU control below
disables the instrumentation.

| Layout | Mouse event p95 | Wheel event p95 | Wheel CPU seconds | Combined CPU seconds | Full renders: wheel / combined |
|---|---:|---:|---:|---:|---:|
| Editor | 0.652 | 3.274 | 0.57 | 0.77 | 200 / 800 |
| Editor + Agent | 0.940 | 3.505 | 0.58 | 0.86 | 200 / 800 |
| Two editors + NeoTree + Agent | 0.923 | 3.068 | 0.59 | 0.88 | 597 / 1,173 |

All 200 mouse-only events caused a full render in every accepted run. They
produced 8,800 terminal bytes despite no changed text. All mouse and wheel
events were accounted for. Three of the 1,800 injected `j` keys were discarded
across the editor-only/Agent runs; Red intentionally drops stale repeated
motion input. The harness now reports that separately.

For the full workspace, combined-input drain observations were 25.5, 75.1,
and 0.5 ms after injection stopped. These are polling-resolution observations,
not precise input-to-pixel latency. The traced workload remained below the
existing 16 ms p95 handler/frame budget, so this is evidence of avoidable work
and occasional backlog, not proof of a persistent stall on this workstation.

### Tracing-disabled control

Three otherwise equivalent full-workspace runs, with `RED_PERF=off`:

| Phase | Median CPU seconds | Median terminal bytes |
|---|---:|---:|
| Idle | 0.03 | 0 |
| Mouse only | 0.17 | 8,800 |
| Wheel only | 0.57 | 2,708,730 |
| Wheel + mouse | 0.86 | 2,727,692 |

Adding mouse motion costs approximately **51% more CPU** in this control.
The old `drain_lag_ms` field in these archived no-trace JSON files includes the
intentional 200 ms quiet-window detector and must not be treated as latency.
The current harness distinguishes quiescence observations from exact-count
drain observations.

## Attribution

An instrumentation-only build adds `render:prepare`, `render:panels`,
`render:overlays+cursor`, per-pane `panel:paint`, and
`panel:text_layout_miss`. No performance behavior was changed.

In its combined-input run, full rendering totaled 717.8 ms over 1,153 frames:

| Component | Total ms | Share of full-render time |
|---|---:|---:|
| Panel painting | 225.9 | 31.5% |
| Editor-window painting | 225.0 | 31.3% |
| Terminal diff/flush | 107.1 | 14.9% |
| Global chrome | 88.6 | 12.3% |

The Agent pane itself accounts for 113.0 ms and NeoTree 96.8 ms within panel
painting. There were no text-panel layout-cache misses during the measured
phases. Editor highlighting missed twenty times, totaling 11.4 ms (p95 0.794
ms). These spans overlap their parent render spans; do not sum all labels.

Turning syntax highlighting off in both editor splits reduced combined output
from 2.71 MB to 1.64 MB, but CPU was 0.90 versus 0.89 seconds in this single
instrumented comparison. This is a useful control, not a statistically robust
claim about the exact cost of syntax highlighting. It argues against starting
with a highlighter rewrite.

## Source-grounded findings

- `Editor::process_editor_event` calls `render` when no action/render advanced
  `render_generation`, including ignored `MouseEventKind::Moved` events.
  The fallback predates this investigation (June 2026).
- `ScrollUp`/`ScrollDown` render directly. The action epilogue renders again
  whenever more than one editor window exists; plugin effects may then request
  another render. The multi-window fallback dates to 2025.
- `read_ready_event` coalesces resize events, and the held-key path has a
  bounded motion drain, but ordinary mouse movement and wheel events do not
  get an equivalent frame budget. The outer input loop can postpone background
  servicing while a continuous stream remains ready.
- `render` repaints all editor windows and all visible panels. Text-panel
  Markdown layout is already cached, but its cells and chrome are repainted on
  each full frame. Even an empty terminal diff still updates cursor state and
  flushes output.
- Render generations also drive session-snapshot freshness. Review this
  coupling when removing redundant renders, rather than accidentally weakening
  recovery or causing needless snapshots.

## Proposed implementation plan

1. **Stop invalidation-free work.** Give event handling an explicit visible-state
   change/render outcome. Ignore unhandled mouse moves without repainting;
   preserve hover-enabled dialogs, divider drags, selection, focus restoration,
   keymap hints, and panel interactions. Add deterministic render-count tests.
   Acceptance: an idle mouse-move flood produces zero full renders and no
   terminal output, unless visible hover state actually changes.
2. **Render once per bounded input batch.** Remove unconditional multi-window
   action redraws in favor of dirty windows/regions and a single flush point.
   Coalesce adjacent wheel/motion events while preserving scroll distance,
   direction changes, pointer target, modifier state, clicks, drags, and key
   ordering. Service timers, LSP, and Agent events between bounded batches.
   Acceptance: ordinary scroll does not produce two or three redundant full
   frames per input; no stale scrolling continues after input stops.
3. **Reuse unchanged surfaces.** Preserve rendered inactive editor windows and
   idle panels; invalidate on content, viewport, selection, focus, size, theme,
   diagnostics, or plugin-decoration changes. Keep the existing Markdown layout
   cache. Use bulk rectangle fills where appropriate, and avoid cursor-only
   terminal writes when cursor state is unchanged. Re-measure before introducing
   more cache complexity.
4. **Expand the regression matrix.** Add same-buffer/distant-viewport splits,
   active Agent streaming, LSP/diagnostics enabled, relative line numbers,
   wrapped/long lines, resize drags, and a real terminal with output backpressure.
   Use deterministic work-count assertions in CI and retain workstation p95,
   CPU, output-volume, and queue-drain comparisons. Bisect the July/August panel
   changes only if a historical regression attribution is still needed.

The order deliberately targets confirmed redundant work first. No speedup is
claimed until the same retained baseline workload is rerun against a fix.

## Reproduction and evidence

```sh
cargo build --locked --release --bin red
python3 scripts/workspace_scroll_bench.py --layout editor --output target/perf/editor
python3 scripts/workspace_scroll_bench.py --layout agent --output target/perf/agent
python3 scripts/workspace_scroll_bench.py --layout workspace --output target/perf/workspace
python3 scripts/workspace_scroll_bench.py --layout workspace --perf-mode off --output target/perf/untraced
python3 scripts/workspace_scroll_bench.py --layout workspace --syntax-off --output target/perf/syntax-off
```

Output directories must not already exist. The retained bundle is
`target/manual-smoke/perf-20260815-a822561/EVIDENCE.md`; it includes the original
binary, accepted raw traces/JSON, exact instrumentation patch, colored terminal
capture, and checksum manifest. Attach to `tmux attach -t red-perf-a822561` for
the interactive layout. Failed setup pilots are excluded from the result table.

Limitations: no historical binary A/B, no active model stream, no LSP load, no
physical-terminal presentation timing, and no Windows measurement. The broad
performance work is not implemented, committed, pushed, or published as a PR.
