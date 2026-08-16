# Keyboard viewport-edge scrolling — 2026-08-15

## Outcome

The original investigation found that keyboard scrolling still published
redundant frames after #234. Each normal
`j`/`k` first paints the moved editor, then applies the indent-guide plugin's
queued decorations and paints it again. Crossing the viewport boundary changes
many rows, so those frames generate much more terminal traffic. The output is
not enclosed in synchronized-update markers. A constrained output sink makes
this path exceed the 16 ms frame target by a wide margin.

There are also two correctness concerns: a confirmed stale-row defect with
`scrolloff = 0`, and repeated-key draining that can discard keys across a
direction-change boundary. The implementation and matched remeasurement are
recorded at the end of this report; the original baseline remains frozen.

## Measured revision and setup

- Branch/worktree: `codex/keyboard-scroll-investigation`,
  `red.codex-keyboard-scroll-investigation`, created with `wt`.
- HEAD: `8881e8fbbdbf32a17d14ff37f75de44cb2ef9c20`.
- Tree: `ac69a424b053e47cb93cc333d8b0a93bba2ee72b`, identical to the previously
  validated `4eab875` tree.
- Frozen release binary SHA-256:
  `e60d1b71867b54001bb2a8ee244b4b8d86c84e4b439ca5b692b475af692332c9`.
- Apple M4 Max, 128 GiB RAM, macOS 26.6.1 / 25G76, arm64, Rust 1.94.1.
- A 200×60 truecolor PTY, isolated configuration, bundled plugins, LSP off,
  syntax-highlighted `src/editor.rs`, default wrapping and `scrolloff = 3`.
- The full workspace has two editor splits, NeoTree, and the real Agent UI
  populated by a deterministic local app-server fixture. No model request or
  personal conversation is used.

Each main phase sends 200 keys at 25 ms spacing. The inside-viewport control
alternates `j/k` around the middle; down/up phases first move to the respective
viewport edge. The main matrix runs serially, three times, with editor-only and
full-workspace trace runs plus a tracing-disabled workspace control.

Another Rust build and system indexing were observed using substantial CPU.
Consequently, frame counts and output volume are stronger evidence here than
small CPU differences or individual wall-time spikes. The PTY is not a physical
display-latency measurement. The throttled sink is an explicit stress model,
not a claim about the user's terminal's measured throughput.

## Baseline

Three-run medians; decimal MB. CPU is process CPU time for the entire phase,
including settling. Event p95 covers the handler, not every later background
decoration frame.

| Layout / tracing | Motion | CPU seconds | Output | Nonempty terminal frames | Event p95 |
|---|---|---:|---:|---:|---:|
| Editor / trace | Inside | 0.86 | 0.816 MB | 400 | 9.50 ms |
| Editor / trace | Down edge | 0.95 | 3.911 MB | 400 | 10.25 ms |
| Editor / trace | Up edge | 1.20 | 5.307 MB | 389 | 10.62 ms |
| Workspace / trace | Inside | 0.84 | 0.546 MB | 400 | 7.73 ms |
| Workspace / trace | Down edge | 1.08 | 1.905 MB | 400 | 9.18 ms |
| Workspace / trace | Up edge | 0.90 | 2.504 MB | 390 | 7.16 ms |
| Workspace / off | Inside | 0.81 | 0.546 MB | 400 | — |
| Workspace / off | Down edge | 0.71 | 1.905 MB | 399 | — |
| Workspace / off | Up edge | 0.88 | 2.504 MB | 390 | — |

All 3,600 keys in the six traced baseline runs were handled. The no-trace
workspace sends 3.49× / 4.58× as many bytes when scrolling down/up as when
moving inside the viewport. The larger editor-only viewport emits more text;
the Agent is not the primary cause of this particular slowdown.

### Attribution controls

- Disable only `indent_guides`: exactly 200 nonempty frames in each 200-key
  phase, versus approximately 400 normally. Down/up output is 1.715 / 2.356 MB.
  This directly identifies the extra decoration frame.
- Disable syntax: down/up output falls to 1.205 / 1.635 MB, but frame counts
  remain approximately 400. Ordinary highlighted runs have only seven
  highlight-cache misses per edge phase. Syntax coloring contributes escape
  traffic; repeated parsing is not the leading explanation.
- Disable wrapping: down/up output remains about 1.897 / 2.508 MB. Relative
  numbers also retain the duplicate-frame pattern. Neither toggle removes the
  main cause.
- Limit PTY reads to 256 KiB/s: down/up motion-frame p95 rises to **45.6 / 68.5
  ms**, with maximums of 96.7 / 91.5 ms. All 200 motions execute in each phase;
  fewer intermediate frames are published. This reproduces an output-bound
  stall.
- File limits: 200 `k` keys at BOF emit zero bytes/frames; 200 `j` keys at EOF
  emit 8,800 cursor-control bytes but no nonempty frames. The reported expensive
  operation is scrolling the viewport, not reaching EOF/BOF.
- Fast alternating input at 5 ms: 194 of 200 `j/k` events handled. The initial
  pilot also handled 196 of 200 during a large scheduling stall. These runs are
  retained separately and are not used as exact-input baseline results.

In the second tracing-disabled workspace run, **93.0%** of downward output and
**91.6%** of upward output are ANSI/OSC escape bytes. Downward scrolling emits
134,992 SGR sequences and 33,748 absolute cursor moves; upward emits 174,292 and
43,573. `render_diff` emits absolute positioning plus foreground, background,
bold, and italic commands for every same-style run, including unchanged style
components. This is a promising output-encoding optimization after removing
duplicate frames.

### Debug-build control and profile

A running Red process was observed at `red/target/debug/red`. Its exact loaded
source revision was not established, and its executable may have been rebuilt
since launch. To avoid attributing a stale binary to this revision, the
investigation built and froze a fresh debug executable from the pinned source.

In one full-workspace run, inside/down/up cost **2.74 / 3.02 / 2.95 CPU seconds**.
Down/up event p95 was **14.61 / 14.90 ms**; `cursor:moved` callback p95 was
**9.96 / 10.05 ms**. All 600 keys were handled. Debug mode therefore leaves much
less frame-budget headroom than the optimized build.

A separate four-second macOS sampling profile during a 1,000-key / 5 ms stress
run shows substantial main-thread work in `flush_deferred_plugin_event`,
`Runtime::notify_isolated`, and Husk callback evaluation. Value copying,
`heap_value_size`, and B-tree traversal appear among the busy leaf stacks.
That profiled stress run handled 919/1,000 keys under the existing lossy repeat
policy; it is attribution evidence, not a clean latency benchmark.

## Source-level findings

1. **Two publication boundaries for keyboard navigation.**
   `src/editor.rs::handle_key_action_with_deferred_motion` flushes the plugin
   notification and then the pending frame. `indent_guides.hk::refresh` queues
   `SetDecorations`; `service_background` applies that request afterward and
   requests another motion frame. The wheel path's `process_scroll_batch`
   already services those effects before deciding whether another frame is
   necessary. Reuse that ordering for keyboard navigation.
2. **No atomic terminal presentation.**
   `src/editor/rendering.rs::render_diff` hides the cursor, emits changed runs,
   restores cursor state, and flushes. It never emits mode 2026 begin/end.
   Every captured phase has zero synchronized-output markers. Buffering into
   one application write does not guarantee atomic presentation by a terminal
   or multiplexer. Crossterm 0.27 already exposes the relevant commands; see
   the [synchronized-output protocol](https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md).
3. **Confirmed incorrect viewport invalidation at zero scrolloff.**
   `MoveUp` / `MoveDown` can mutate `vtop` before `finish_cursor_motion` takes
   its `before` snapshot. If `check_bounds` makes no further change, the code
   can choose a cursor-row delta for a moved viewport. The original ignored diagnostic
   compares the resulting cells to a full render: down `100→101` leaves rows
   0–11 stale; up `100→99` leaves rows 1–12 stale. Both are 12-row mismatches in
   the split fixture. This is a distinct correctness bug, not proof that every
   default-scrolloff visual artifact is stale data.
4. **Repeat-discard can cross a key boundary.**
   `drain_repeated_motion_events` processes a nonmatching key in its first loop,
   breaks, and then still enters a second loop that discards the original key's
   signature. Thus queued alternating directions can lose input. The 194/200
   trace supports this path. The 50 ms repeated-motion budget also differs from
   the newer 8 ms ordinary-input/wheel budget.

## Proposed implementation order

1. **Correctness first.** Compare motion against the last painted viewport (or
   a pre-action view snapshot), not a snapshot taken after motion has already
   changed it. Convert the ignored stale-row diagnostic into passing regression
   tests. Cover both directions, scrolloff 0/3, wrapping, inline comments,
   relative numbers, same/different-buffer splits, and cursor styles. Stop a
   repeat-drain/discard run at every direction, modifier, mode, or non-motion
   boundary; account explicitly for any intentionally discarded stale repeats.
2. **One coherent navigation frame.** Apply a bounded keyboard batch, notify
   plugins once for its final state, drain its relevant queued effects, then
   render once using the strongest required damage scope. Preserve background
   fairness, exact counted motions, macro/operator semantics, and full-render
   fallbacks for layout, dialogs, focus, LSP, and streaming Agent changes.
3. **Atomic presentation.** Enclose each completed terminal frame in
   synchronized-update begin/end, with safe unsupported-terminal behavior and
   restoration on error/exit. Test the actual terminal and tmux path. This
   addresses presentation tearing; it does not replace correct damage tracking.
4. **Reduce terminal bytes.** Track the emitted style/cursor state within a
   frame, emit only changed SGR components, and evaluate cost-aware contiguous
   runs. Consider terminal scrolling only for geometries that can safely retain
   side panes and split contents; never scroll the entire terminal blindly.
5. **Remeasure before wider runtime work.** Repeat this exact release/debug
   matrix and slow-sink control. If the Husk callback remains material after
   batching, cache viewport-invariant indent-guide work or optimize the measured
   value-copy/accounting path. Do not start with a broad syntax-cache or VM
   rewrite.

### Acceptance criteria

- Every optimized screen-cell result equals a fresh full frame, including both
  viewport directions and comment/wrap boundaries.
- Exact input accounting for alternating keys, counted motions, and ordering
  boundaries; any held-repeat discard is explicit and confined to one run.
- A stable 200-key workspace edge phase should need approximately 200 frames,
  not 400, unless an independently changing background surface requires more.
- Synchronized frame markers are balanced; resize, interruption, and exit never
  leave synchronization enabled.
- Preserve the existing release p95 target below 16 ms; report CPU, emitted
  bytes, frame counts, and constrained-sink results together on a quieter host.
- Run the full Rust suite, formatting, strict Clippy, and retained real-terminal
  smoke before publishing an implementation.

## Reproduction and evidence

Run `scripts/compare_keyboard_scroll.py` with the frozen release binary,
`--group main` and `--group controls`, using new output directories. The
single-run `scripts/keyboard_scroll_bench.py` also supports a pinned debug
binary and a separate macOS `--sample-phase` capture.

The original retained entry point is
`target/manual-smoke/keyboard-20260815-8881e8f/EVIDENCE.md`.
It contains raw traces/ANSI output, machine-readable timings, exact binary
hashes, the ignored diagnostic patch and failure log, the sampling profile,
four tmux captures, and a checksum verifier. Attach to the preserved session
with `tmux attach -t red-keyboard-8881e8f`.

## Implementation

The implementation starts from `0928ba8a2ecd40c07ae7be7af29d0c3b26dc79d4`,
which includes #235's adjacent insertion-line-end viewport fix.

- A committed viewport key includes `BufferId`, revision, scroll position,
  wrapping, terminal size, and window geometry. Cursor-row damage is allowed
  only while that key still matches the painted screen.
- Normal navigation defers publication until its final plugin notification and
  queued decoration effects have been applied. Native repeat batches preserve
  every key, stop at input boundaries, and yield after 8 ms or 64 events.
  Counts, operators, macro recording/replay, and semantic replay are not
  consumed as ordinary repeat runs. Detached input uses the same publication
  ordering.
- Nonempty terminal frames use synchronized-update mode 2026, with best-effort
  cleanup on write errors, panic, and exit. The encoder retains style and cursor
  state only within one frame, emits only changed style components, and skips
  redundant cursor moves without losing wide-grapheme padding.
- `RED_PERF=trace` now exposes `navigation:publish`, which includes the deferred
  plugin, background-effect, and final-render work. Key-handler timings alone
  are no longer an honest measure of the whole publication path.

The reusable paired runner is `scripts/compare_keyboard_scroll.py`; the
verification entry point is `scripts/finalize_keyboard_comparison.py`.

### Matched remeasurement

The new comparison freezes both binaries at the updated base and extracts one
immutable source fixture from that commit. It alternates before/after order,
runs the three release configurations three times, repeats every original
attribution control, and runs three debug pairs. The 200×60 PTY, 200 keys,
25 ms spacing, highlighted file, populated Agent, splits, and NeoTree are
unchanged. LSP is disabled for these timing runs.

The verifier passed **38 runs / 19 fixture pairs**. Every one of the **9,400
traced after-keys** was handled. Every nonempty after-frame has one balanced,
properly ordered synchronized-update pair. The exact source files and binary
hashes are checked independently of the current checkout.

Three-run medians for the normal, tracing-disabled workspace:

| Motion | Output before → after | Reduction | Frames before → after | CPU seconds before → after |
|---|---:|---:|---:|---:|
| Inside viewport | 0.546 → 0.211 MB | 61.4% | 400 → 200 | 0.73 → 0.54 |
| Down edge | 1.905 → 0.878 MB | 53.9% | 400 → 200 | 0.75 → 0.59 |
| Up edge | 2.504 → 1.102 MB | 56.0% | 390 → 200 | 0.45 → 0.65 |

The CPU result is mixed, not a universal speedup. In the traced workspace,
down/up CPU medians improve from 0.47/0.82 to 0.39/0.54 seconds. Desktop
scheduling and terminal-reader stalls still affect timings; a process-name-only
load snapshot is retained. One editor-only after-run had upward frame p95
21.4 ms, and another caught up after a 322 ms event outlier without losing
input. Do not interpret the medians as a hard latency guarantee.

The new workspace `navigation:publish` p95 medians are **2.35 / 2.49 / 7.96 ms**
inside/down/up. This includes deferred plugin callbacks and the final render;
the shorter key-handler span is not compared directly with the old handler.
Ordinary down/up motion-frame p95 medians are 0.94/2.39 ms. Editor-only edge
output falls from 3.911/5.307 MB to 1.715/2.296 MB; its after frame-count
medians are 193/200 because queued repeats can share a frame.

The second tracing-disabled downward run emits 35,647 SGR sequences instead
of 134,992, and 17,805 absolute cursor moves instead of 33,748. This confirms
that reduced command encoding contributes independently of frame coalescing.

### Stress and debug results

- At **256 KiB/s**, down/up motion-frame p95 improves from **46.5/67.3 ms to
  20.8/31.9 ms**. The after publication p95 is 24.6/36.0 ms. All keys execute;
  the after path publishes 200/176 frames versus 168/123 before. This slow
  sink still cannot meet a universal 16 ms budget, but it receives more
  intermediate positions with substantially shorter stalls.
- At 5 ms spacing, alternating `j/k` handles **195/200 before, 200/200 after**.
  The after down/up phases also handle 200/200. BOF/EOF still emit no nonempty
  screen frames.
- The three debug edge pairs handle every key on both binaries. Down/up CPU
  medians improve from **3.04/3.01 to 2.63/2.53 seconds** (about 13%/16%).
  After publication p95 is **15.51/15.37 ms**. The debug inside control is not
  an equal-input CPU comparison: baseline runs handle only 193, 193, and 175
  of 200 keys, while every after-run handles 200.
- The debug `cursor:moved` callback remains about 10 ms p95. That is the next
  isolated CPU investigation if debug-build responsiveness remains a problem.
  This change does not rewrite Husk allocation/accounting or introduce a new
  cross-frame plugin cache. The release workspace callback is much cheaper,
  and the confirmed output/correctness fixes can be evaluated independently.

### Validation and retained evidence

- `cargo fmt --all -- --check`: passed.
- `cargo test --locked --all-features --all-targets -- --test-threads=1`:
  **2,257 passed**, including a second run on the final instrumented source.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Screen-equivalence regressions cover both directions, scrolloff 0/3,
  wrapping, inline comments, relative numbers, and shared/different-buffer
  splits. Input tests cover bounded batches, counts, operators, macro state,
  direction/modifier/release/resize boundaries. Encoder tests cover style
  transitions, wide cells, and cleanup after a simulated write failure.
- Retained tmux 3.6a smoke: highlighted split editors, Agent, NeoTree, down/up
  navigation, and resize 200×60 → 180×50 → 200×60 all passed. This validates
  final screen state and terminal protocol, not physical display latency or a
  direct observation of the user's reported tearing.

Implementation evidence:
`target/manual-smoke/keyboard-after-20260815-0928ba8/EVIDENCE.md`.
It includes the four frozen binaries, fixture archive, raw ANSI/log/JSON
measurements, paired summaries, validation logs, tmux captures, source patch,
and verified checksums. The retained session is
`tmux attach -t red-keyboard-after-0928ba8`.

To repeat the matrix, supply `--before-binary`, `--binary`, `--root` pointing
to the extracted fixture, and a **new** `--output` directory to
`scripts/compare_keyboard_scroll.py`. Run `--group main` and `--group controls`
with the release pair, then `--group debug` with the debug pair. Run
`scripts/finalize_keyboard_comparison.py --output <evidence-directory>
--summarize` to check fixture identity, exact after-input counts, synchronization
ordering, and medians. The original investigation bundle remains unchanged.
