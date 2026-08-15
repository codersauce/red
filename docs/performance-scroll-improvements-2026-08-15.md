# Scroll performance implementation — 2026-08-15

## Outcome

Implemented the measured navigation/rendering fixes on
`codex/scroll-performance` in the durable `red.codex-scroll-performance`
worktree, created with `wt` from the exact measured base
`a8225610c3e024352019d3dca52a430855234a27`.

The full-workspace traced median fell from 0.90 to 0.38 CPU seconds for
200 wheel events interleaved with 600 mouse moves. Unused mouse movement now
produces no redraw and no terminal bytes. The optimized path preserves every
wheel delta and continues servicing Agent/plugin events between bounded batches.

## What changed

- Ignore unhandled pointer movement without changing pending key sequences.
  Dialogs can still request a hover redraw.
- Gather only adjacent compatible wheel/move events, up to 64 events and an
  8 ms collection budget. Direction, coordinates, modifiers, clicks, drags,
  keys, and resize events remain ordering boundaries. Every wheel delta is
  applied; this is not the existing stale-repeat-key dropping policy.
- Accumulate the strongest required repaint and publish the final navigation
  frame. Ordinary input also yields to background work after an 8 ms budget.
  The collection budget is not a hard deadline for processing an entire batch.
- Repaint the active editor, or all affected editor windows for shared
  decorations, while retaining unchanged docked panes. Focus/layout/overlay
  changes and streamed panel updates retain full-render fallbacks.
- Preserve the existing syntax and Markdown layout caches. Add attribution
  spans and reusable PTY comparison scripts instead of introducing another
  broad surface-cache layer.

## Matched measurements

Apple M4 Max, 128 GiB, Darwin arm64, Rust 1.94.1; optimized release builds.
Both versions open the exact same files in the original investigation
worktree. The terminal is 200 × 60, syntax highlighting is enabled, bundled
plugins are active, and the real Agent UI is populated by a deterministic local
app-server fixture. No model call or personal conversation is involved.

The main matrix is three serial, interleaved before/after repetitions. Each
phase injects 200 events at 5 ms spacing; mixed input adds three mouse moves per
wheel. Values below are medians. The baseline is the frozen instrumentation-only
binary; both versions expose the same full-render attribution spans.

| Layout | Wheel CPU seconds, before → after | Mixed CPU seconds, before → after | Full frames, wheel | Full frames, mixed |
|---|---:|---:|---:|---:|
| Highlighted editor | 0.57 → 0.51 | 0.76 → 0.50 | 200 → 0 | 800 → 0 |
| Editor + Agent | 0.58 → 0.51 | 0.83 → 0.53 | 200 → 0 | 800 → 0 |
| Two editors + NeoTree + Agent | 0.65 → 0.32 | 0.90 → 0.38 | 590 → 0 | 1,173 → 0 |

Zero full frames does not mean no rendering: the optimized workspace emits
200 scoped editor frames in the median wheel and mixed phases. Every traced
mouse/wheel event was accounted for. Held-key dropping is reported separately
because Red intentionally discards stale repeated motion keys.

### Tracing-disabled control

Three matched full-workspace repetitions use the original uninstrumented
baseline and disable tracing in the optimized build.

| Phase | CPU seconds, before → after | Terminal bytes, before → after |
|---|---:|---:|
| Idle | 0.05 → 0.05 | 44 → 44 |
| Mouse movement | 0.30 → 0.03 | 8,800 → 0 |
| Wheel | 0.59 → 0.33 | 2,708,666 → 2,607,820 |
| Wheel + mouse | 0.84 → 0.37 | 2,730,701 → 2,625,287 |

Mixed-input CPU is approximately **56% lower**. The remaining terminal bytes
are principally actual scrolled text; this change removes redundant painting
rather than suppressing the final visible scroll position. CPU accounting has
10 ms resolution, so small differences should not be overinterpreted.

### Extended controls

These are single matched runs, not three-run medians.

| Control | CPU seconds, before → after | Observed rendering |
|---|---:|---|
| Same-buffer splits, relative numbers, wrapping; mixed input | 1.39 → 0.44 | 1,004 full frames → 200 all-editor frames |
| Agent actively streaming during mixed input | 0.91 → 0.44 | Both received 22 Agent updates; optimized build retained 29 necessary full frames |
| Zero-delay 200-wheel burst | 0.46 → 0.03 | 404 full frames → 4 scoped frames |
| Zero-delay mixed burst | 0.80 → 0.08 | 1,004 full frames → 13 scoped frames |

All accepted controls have exact per-type input counts. Three initial unpadded
stress runs decoded one SGR mouse report as ordinary keys; this happened on
both old and new builds. Those runs are retained as rejected evidence, not used
in the table. The corrected harness checks mouse, wheel, and key counts
separately, handles short PTY writes, and uses fixed-width, zero-padded SGR
coordinates for the stress controls. Crossterm 0.27's Unix parser can recognize
a lone escape byte at a short-read boundary as an Escape key; this is a separate
input-decoding follow-up, not a performance fix claimed by this branch.

A final optimized-build smoke enabled the installed `rust-analyzer`, waited
for initialization, and observed document-diagnostic and inlay-hint responses.
Its 100 mouse moves, 100 wheel events, and 400 mixed events were all decoded
exactly; ignored movement still emitted zero bytes. This is a functional
LSP-on check, not a matched LSP performance comparison. The retained tmux
layout also survived a 200×60 → 180×50 → 200×60 resize with both editors,
NeoTree, and the populated Agent pane intact.

## Validation

- Eight new deterministic rendering tests pass. They check ignored movement,
  exact wheel distance, shared/different-buffer splits, relative numbers,
  wrapping, focus changes, batch ordering boundaries, shared decorations,
  namespace replacement, live panel invalidation, and dialog hover.
- Optimized screen cells are compared with a fresh full render.
- The full all-targets/all-features test suite, release build, formatting, and
  strict Clippy pass.

## Reproduction

Use `scripts/compare_workspace_scroll.py` with the frozen before/after binaries
and `--fixture-root` pointing at the original investigation worktree. It runs
the comparisons serially and writes raw logs, JSON, ANSI captures, and median
summaries. Each output run directory must be new.

The retained bundle is
`target/manual-smoke/scroll-20260815-a822561/EVIDENCE.md`. It contains the
optimized binary, exact source patch, raw measurements, walkthrough captures,
verification script, and checksum manifest. The original baseline remains in
the separate `red.codex-overall-performance` worktree.

Per-event handler timing is no longer directly comparable: deferred handlers
finish before the batch's final render. Use CPU, frame counts, output volume,
and queue-drain observations. Neither PTY drain time nor the no-trace quiet
detector measures physical-display latency. A real terminal under output
backpressure and Windows remain follow-up measurement targets.
