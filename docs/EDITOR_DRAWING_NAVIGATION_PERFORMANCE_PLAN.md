# Red drawing and navigation performance plan

Drive down input latency, drawing cost, terminal traffic, and idle CPU across
macOS, Linux, and Windows while preserving exact navigation and rendered output.
Measure the full interaction before choosing optimizations, then keep taking the
largest remaining measured cost out of the path.

This is a plan, not a benchmark result. Source inspection used `main` at
`6706f4194acbcc71eaa00b9c58ce756ab911adcb` on 2026-11-04. No builds or performance
runs were performed for this document. Existing dated reports are historical
evidence; their timings and retained binaries need fresh verification.

1. **Establish three native measurement environments.**

   | Platform | Execution environment | Measurements and profiler | Additional interaction check |
   | --- | --- | --- | --- |
   | macOS | Local Mac, native release build | Existing PTY drivers, process CPU and allocation profiles; existing `--sample-phase` capture and Instruments Time Profiler | Direct terminal and tmux; record terminal version, font, dimensions, refresh rate, and display scaling |
   | Linux | Dedicated existing devbox reached through `dbox` | Run the PTY driver and Red together inside the devbox; symbolized CPU sampling and hardware counters where available | SSH from the Mac, direct and through tmux, reported separately from guest-local measurements |
   | Windows | Native Cloud PC session opened through Windows App | Native MSVC build, ConPTY replay driver, process CPU/private bytes, WPR/WPA traces with matching symbols | Windows Terminal inside the Cloud PC, then the experience observed through Windows App |

   Compare base and candidate on the same machine, terminal backend, build
   configuration, and workload. Cross-machine milliseconds are not a platform
   ranking. Give each platform its own baseline and regression decision.

   The current release matrix includes macOS ARM64 and Intel, Linux x86-64, and
   Windows x86-64 MSVC. Run native performance baselines on the available hardware;
   retain build/correctness coverage for both Mac architectures. Do not label an
   architecture performance-validated until a native reference run exists.

   Access inspection found `dbox` installed and verified the following interfaces:

   ```text
   dbox ls
   dbox ssh DEVBOX_NAME --no-tmux
   dbox exec run DEVBOX_NAME COMMAND...
   dbox exec get DEVBOX_NAME EXEC_ID
   ```

   Devbox inventory is currently unverified: `dbox ls` failed obtaining an Azure
   token, including outside the local filesystem sandbox. At execution time,
   restore the normal authenticated access, select an idle existing devbox, and
   record its actual OS, CPU allocation, RAM, architecture, and contention.
   A machine name alone does not establish available capacity.

   Windows App is installed at `/Applications/Windows App.app` and visibly lists
   the user's Cloud PC. Guest connectivity, machine specification, compiler,
   terminal, and profiler availability still need verification. Open that session
   and run the automation within Windows; use Windows App to observe the result.
   WSL runs are Linux coverage, not native Windows coverage.

   Use the same pinned source revisions and fixture hashes on all three hosts.
   Build on each OS, retain matching symbols, and record executable hashes.
   Create implementation worktrees with `wt`; keep immutable baseline binaries
   outside build output paths that later builds overwrite.

2. **Reuse the existing work and close the measurement gaps.**

   | Existing capability | Current source | Next measurement |
   | --- | --- | --- |
   | Full, editor-window, active-window, and cursor-row rendering | `src/editor/rendering.rs` | Count each path and record the reason a narrower path was unavailable |
   | Exact repeated-key processing with bounded publication batches | `src/editor.rs`, `src/terminal_input.rs` | Queue age, batch size, time until the final state is flushed, and background-service delay |
   | Layout and viewport highlight caches | `src/editor.rs`, `src/editor/display_layout.rs`, `src/highlighter.rs` | Hits, misses, invalidation reasons, lines/bytes scanned, retained memory |
   | Cell reuse, changed-cell output, style/run reuse, blank-suffix erasure, synchronized terminal updates | `src/editor/render_buffer.rs`, `src/editor/rendering.rs` | Split cell comparison, ANSI encoding, writing, and flush blocking; count bytes and writes |
   | Keyboard/wheel/workspace replay, throttled output, matched comparisons | `scripts/keyboard_scroll_bench.py`, `scripts/workspace_scroll_bench.py`, `scripts/compare_keyboard_scroll.py`, `scripts/compare_workspace_scroll.py` | Native Windows support, explicit completion assertions, comparable structured results |
   | Isolated motion/layout/render benchmarks | `examples/performance_hotspots.rs`, `scripts/compare_performance_hotspots.py` | Parameterize relevant cases by file size, line length, viewport, and state |
   | Optimized-frame versus full-frame correctness tests | `src/editor/navigation_perf_tests.rs` | Extend to every proposed fast path and native input backend |

   Read [the current performance gates](performance.md), the
   [keyboard investigation](performance-keyboard-scroll-2026-08-15.md), the
   [scroll implementation report](performance-scroll-improvements-2026-08-15.md),
   and the [previous optimization campaign](performance-optimization-2026-08-21.md)
   before revisiting an old idea. Several simple optimizations already shipped;
   others were rejected after measuring memory growth or weak end-to-end gains.

   Extend `RED_PERF` with input sequence IDs and frame IDs. For a batch, record
   which inputs its published state includes, oldest/newest input age, and the
   final cursor, viewport, buffer revision, and relevant background revisions.
   Separate arrival/read, dispatch, motion, notification, background servicing,
   layout, highlight, paint, diff, encode, write, and successful flush milestones.
   Attribute nested spans without double-counting or adding stage percentiles.

   Distinguish five endpoints: driver injection, editor read, completed editor
   flush, terminal consumption, and visible presentation. A completed write does
   not prove pixels have changed. A silence timeout or PTY read chunk does not
   identify which input has completed. Use deterministic final-state assertions
   and frame correlation; label timings whose endpoint remains only a proxy.
   Keep host-local clocks together and avoid subtracting unsynchronized Mac,
   devbox, and Cloud PC timestamps.

   The current recorder stores every sample in a mutex-protected map of vectors
   and formats labels. Add a bounded collection mode and machine-readable
   summaries for long runs, with explicit dropped-sample counts and bounded label
   cardinality. Benchmark instrumentation off, summary, and short trace captures.
   Accept improvements using the off/low-overhead controls, not trace alone.

3. **Make the replay scenarios portable before tuning shared code.**

   Keep one scenario definition, input schedule, fixture set, correctness oracle,
   and result schema. Introduce a small process/terminal adapter boundary around
   the existing scripts instead of replacing the whole benchmark suite.

   The current Python drivers import `pty`, `fcntl`, and `termios`, and obtain CPU
   time through `ps`; these require a Windows adapter. Use native ConPTY for the
   Windows harness, including resize, input, output draining, process accounting,
   timeouts, and cleanup. Its synchronous pipes need independent servicing to
   prevent the harness from creating a read/write deadlock. Follow Microsoft's
   [pseudoconsole session guidance](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session).
   Port the deterministic Agent fixture launch as well; a Unix executable script
   path is not a portable Windows process command.

   Validate the harness first: handle short writes, partial reads, escape sequences
   split across reads, synchronized-update markers, process exit, and resize.
   Confirm exact decoded input counts and final state for every accepted run.
   Record intended injection times and actual send times so slow consumers cannot
   silently reduce offered load and make latency look better.

   Run workload generation and timing within the Linux guest or Cloud PC. Separate
   Red CPU from terminal/console-host CPU and remote-client CPU. Use SSH/RDP only
   for a distinct interaction pass. Record transport conditions and display
   settings for that pass; remote latency is not attributable to Red alone.
   A Linux PTY on a headless devbox does not validate a Linux GUI compositor.

4. **Freeze a repeatable benchmark matrix.**

   | Dimension | Core cases | Extended/stress cases |
   | --- | --- | --- |
   | Movement | `h/j/k/l`, `w/b/e`, line start/end, `gj/gk`, page/half-page, counts, search next/previous | Long-distance jumps, marks/jumplist, diagnostic/symbol navigation, direction reversals, BOF/EOF no-ops |
   | Scrolling/input | Held keys, wheel, horizontal scroll, click/drag selection | Mixed mouse/key/resize, zero-delay bursts, rapid reversals, paste interrupting navigation |
   | Documents | Fixed real Rust and Markdown snapshots, plain text; 10k and 100k lines | 1M short lines; 10KB, 100KB, and 1MB single lines; minified JSON; ASCII, CJK, emoji/ZWJ, combining marks, tabs, LF/CRLF |
   | Display layout | Wrap off/on, logical and screen-line movement, absolute/relative numbers, scrolloff 0/default | Narrow windows, horizontal offsets, break indentation, Markdown conceal/tables/images, inline comments and predictions |
   | Workspace | Single editor; two distinct-buffer splits; shared-buffer splits | Editor + NeoTree + Agent; populated panels; repeated window/buffer switches; zoom, split, close and resize |
   | Background state | Bundled plugins, deterministic idle LSP | Diagnostic/inlay updates, search highlights, streaming Agent, decorations, preview and completion popups |
   | Output | 120×40 and 200×60; unthrottled sink | Larger viewport, throttled sink sweep, direct terminal/tmux, SSH or RDP observation |

   Use a small daily core and targeted combinations for each hypothesis; do not
   run the entire Cartesian product. Start with normal configuration, then disable
   syntax/plugins/LSP individually to attribute cost. Reduced-feature runs are
   diagnostic controls, not the main performance result. Exercise edits between
   navigation sequences so an optimization cannot win through stale caches.

   Keep input rate independent of response time: 25 ms spacing for ordinary
   repetition, 5 ms for pressure, and explicit bursts. Measure how quickly the
   editor catches up after input stops, plus cancel and direction-change latency.
   Include a long idle phase and a 30-minute navigation/edit/switch soak for
   retained memory, cache eviction, wakeups, and degradation over time.

   Perform warmup, then at least seven alternating base/candidate repetitions for
   the selected cases, serially on each reference machine. Repeat base/base to
   estimate noise. Use enough events for tail claims: target at least 10,000
   observed interactions across long runs when reporting p99, and retain each
   run's distribution instead of hiding variability in a pooled average.
   Keep cold-open and warmed navigation results separate.

5. **Set budgets and rank the measured bottlenecks.**

   These are proposed targets to validate against the three initial baselines,
   not claims about present performance or automatic shared-CI thresholds:

   | Metric | Initial target |
   | --- | --- |
   | Guest/local injection to completed flush, core scenarios at ordinary input rate | p95 ≤16 ms, p99 ≤32 ms; include queueing and identify the completion endpoint |
   | Editor read to completed flush, unthrottled reference sink | Stretch: in-viewport motion p95 ≤4 ms; viewport scrolling p95 ≤8 ms |
   | Existing bundled Husk cursor callback | Preserve the existing p95 <4 ms gate |
   | Correctness and input | Every accepted key/wheel action accounted for, exact final state, full-frame parity |
   | Unchanged work | No paint caused solely by ignored hover or an unchanged idle tick; explain every additional navigation publication |
   | Fairness | Record maximum background-service and cancellation gaps; ordinary navigation should not introduce gaps above a proposed 16 ms service budget |
   | Memory and output | No unbounded cache growth; report peak/retained bytes, allocations, output bytes, writes, and cells compared per interaction |

   Throttled output, huge semantic scans, and remote presentation have separate
   budgets derived from their baseline and throughput; a blocked output pipe
   cannot promise a universal 16 ms completion. Always retain max latency and
   threshold-exceedance counts, not just percentiles. Report idle CPU separately
   from active throughput and latency.

   Prioritize by absolute milliseconds and CPU saved in frequent scenarios, tail
   impact, affected platforms, and implementation cost. Use sampled profiles to
   explain expensive spans. On macOS, use Instruments
   [CPU call-tree analysis](https://developer.apple.com/documentation/xcode/analyzing-cpu-profiles-with-call-tree-views).
   On Linux, qualify the devbox's available sampling/counter permissions and use
   timing/allocation instrumentation if hardware counters are unavailable. On
   Windows, use [WPR/WPA](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/introduction-to-wpr)
   to distinguish execution, scheduling, and I/O delay. Profiling captures and
   quantitative acceptance runs remain separate.

6. **Optimize in small, measured changes, reranking after each one.**

   The following is the initial investigation order. Profiles may reorder it.
   These are hypotheses and experiments, not confirmed current defects.

   | Order | Experiment | Evidence required to keep it |
   | --- | --- | --- |
   | A | Remove duplicate navigation publication or unnecessary broad invalidation; explain cursor-row fast-path exclusions such as relative numbers, search, diagnostics, and overlays | Fewer painted rows/frames and lower completion latency with exact decoration and overlay behavior |
   | B | Reduce repeated layout, grapheme/column conversion, line materialization, and highlight work across adjacent viewports | Lower scans/misses/allocations; stable behavior for long lines and every cache-key/invalidation input |
   | C | Reduce row snapshots, cell/style cloning, scratch allocation, and whole-frame comparisons where damage is already known | Allocation and CPU reduction; optimized frame matches fresh full render, including wide-character continuations |
   | D | Reduce terminal encoding and transport cost beyond the existing run/style/blank-suffix optimizations | Fewer bytes/writes and less blocked time in real terminals on all OSes, with output safety and write-failure recovery intact |
   | E | Optimize measured motion scans and cached position conversions in `src/buffer.rs` and `src/editing`; avoid repeated whole-line/document traversal | Better scaling with line/document size; identical counts, selections, operator ranges, and cursor goals |
   | F | Bound background work and avoid irrelevant plugin/LSP/panel updates delaying navigation | Better p99, cancellation and service gaps under streaming; no starvation or lost updates |

   Preserve existing `gj/gk` semantics and the configurable `j/k` mapping; counted
   screen-line moves must still traverse the requested screen rows. Preserve
   operators, macros, undo, search wrapping, visual selections, and every accepted
   input event. Batching may reduce redundant presentation, not user actions.

   For caches, first improve existing ownership/invalidation. Record keys, byte
   budgets, eviction, and invalidation for edit, rename, syntax/theme change,
   diagnostics, resize, split, and buffer close/reopen. Shared-buffer windows can
   have different widths and visual state. Avoid adding a second broad surface
   cache without evidence that the existing structures cannot serve the need.

   If ordinary changes leave terminal output dominant, prototype terminal scroll
   regions/line shifting as a later experiment. Prove eligibility for splits,
   gutters, overlays, images, scrollback, resize, terminal capability differences,
   and previous-frame synchronization; retain the current diff fallback. If
   blocking writes dominate, measure a bounded output/scheduling prototype with
   explicit backpressure and recovery. An asynchronous writer alone does not
   make bytes cheaper or justify discarding state transitions.

   Consider compact cell/style storage, broader incremental layout, or build-time
   optimization experiments only after profiles show a meaningful remaining
   ceiling. Measure each independently across platforms and include memory,
   artifact size, and maintenance costs. Do not start a VM/compiler rewrite while
   the existing callback gate passes and profiles point elsewhere.

7. **Prove behavior and prevent regressions.**

   Extend the existing full-render oracle with seeded navigation/event sequences:
   compare all cells/styles and cursor/viewport state after each observable batch,
   then verify final document, selection, notifications, and input order. Cover
   shared buffers, relative gutters, wide glyphs, combining marks, overlays,
   diagnostics, wrap boundaries, theme/resize changes, and edit invalidation.
   A pure rendering change must not alter document or editing semantics.

   Validate captured ANSI output through a terminal-state oracle and native
   terminal smoke; in-memory frame equality cannot detect incorrect escape
   encoding. Test short writes, output failures, and synchronized-update cleanup.
   On Windows, include native key-repeat/modifier behavior, wheel/resize, and
   console-host output. On Linux, include direct/tmux/SSH boundaries. On macOS,
   include the user's normal terminal and tmux workflow.

   Keep deterministic work-count and correctness assertions in normal CI on all
   three OSes. The current performance workflow only gates the Husk callback on
   Ubuntu and its path filters omit much of editor rendering; extend coverage for
   rendering, layout, input, motion, and harness changes. Use controlled reference
   machines for strict timing comparisons and periodic extended runs. Diagnose a
   candidate regression above measured noise before accepting it; do not update a
   baseline merely to hide a regression.

   Before pushing Rust changes, run
   `cargo clippy --all-targets --all-features -- -D warnings` and appropriate
   focused tests. Validate changed platform-specific paths on their native OS.
   A generic optimization is ready only after its relevant base/candidate cases
   pass on macOS, Linux, and Windows, or its remaining platform limitation is
   explicitly recorded as incomplete.

8. **Deliver the campaign in reviewable stages.**

   | Stage | Deliverable | Exit condition |
   | --- | --- | --- |
   | 1: environment and harness | Verified host manifests, immutable base builds, portable replay and Windows adapter | The same scenario produces correct comparable records on all three OSes |
   | 2: attribution and baseline | Input/frame correlation, bounded metrics, baseline distributions, profiles | Known measurement overhead and a ranked list of costs in milliseconds |
   | 3: low-complexity improvements | Small changes from A–F, each with a paired report | End-to-end benefit exceeds noise and correctness/platform checks pass |
   | 4: remaining expensive paths | Selected structural experiments and explicit rejected alternatives | Each retained change beats the simpler implementation on its target workload without material regressions |
   | 5: release protection | CI assertions, reference-machine runbook, final three-platform scorecard | Repeatable evidence, documented residual limits, no hidden unvalidated platform |

   Each result bundle should contain source SHAs/patches, binary and fixture
   hashes, host/toolchain/terminal metadata, commands, raw JSON and traces,
   profiles with symbols, screen/ANSI evidence, correctness results, and paired
   summaries. Keep private host/account details in local run manifests rather
   than committed reports. Include rejected experiments and why they lost.

   Start with the existing keyboard comparison command below, once immutable
   binaries and a fixed fixture checkout exist. The named paths are placeholders;
   this command has not been run for this plan. Add the Windows adapter before
   treating it as portable.

   ```sh
   python3 scripts/compare_keyboard_scroll.py \
     --before-binary /path/to/frozen-base/red \
     --binary /path/to/candidate/red \
     --root /path/to/fixed-fixtures \
     --repetitions 7 \
     --group main \
     --output /path/to/new-result-directory
   ```

   Continue until the ordinary interaction budgets are met on each reference
   environment, stress behavior is bounded, and the highest remaining plausible
   improvements have been measured or ruled out. Report residual time by editor
   CPU, terminal work, blocked I/O, and remote presentation. There is no defensible
   promise of literally all possible performance; the deliverable is a measured
   limit and a clear record of which further tradeoffs would be required.
