# Helix performance plan

[Workstation results and reproduction](HELIX_PERFORMANCE_RESULTS.md)

Prepared against `81083077a3387d38dd856f2c3ad950500d84b6bb` on `fcoury/felix-core`.

The reported problem is slow ordinary typing and cursor movement. Improving those interactions with one selection is the first priority. Performance remains a Milestone 1 acceptance requirement, ahead of broad compatibility work.

This plan follows a source and benchmark audit. Fresh measurements must establish the size and causes of the reported slowdown before optimization begins.

## Findings that shape the work

- `Editor::execute_felix` ends ordinary input with `self.render(frame)`. Felix input also falls outside `action_is_pure_motion` and the navigation classification used for deferred rendering. Investigate the cost of rebuilding windows, panels and chrome for local cursor movements.
- `scripts/felix/performance.py` checks Vim undo/paste and Felix changes at 2, 16 and 128 selections. The Felix timing window covers deletion after selection setup. It leaves ordinary typing, held movement and queued input unmeasured.
- `interaction_bench.py` and `workspace_scroll_bench.py` still expect the older performance-log format. Current `PerfSpan` records include `end id=... t=...`; their startup matching and timing parsing need correction. `edit_replay_bench.py` already accepts that prefix.
- The typing runner validates `edit:replace_char` samples, which are specific to one editing path. The keyboard runner also uses Vim setup commands such as `M` and `G`. Both need mode-aware setup and validation of actual document/cursor outcomes.
- Existing event timers start after input reaches the editor handler. Deferred rendering can finish after that timer ends. Comparing handler duration alone would bias the comparison between editing models.
- Each Felix edit awaits change publication before its final render. Selection painting also repeats source-line conversion and width calculations. Newline indentation flattens a hypothetical document snapshot, and Markdown table parsing can rebuild after edits. Profile these paths separately to distinguish typing, newline and long-line costs.

These are verified code and measurement gaps. Their contribution to latency remains to be measured.

## 1. Establish a trustworthy reproduction and baseline

Record the executable actually used, its hash, source revision, build profile, effective configuration, terminal dimensions, file hash, line count, longest line, CPU, OS and Rust version. Capture the user's reported interaction on a disposable copy when the file is available. Begin with representative fixtures while that detail is pending.

Freeze comparable binaries and dependency inputs before timing. The existing local lockfile override can support builds on this machine. Record its hash with each result and use the same dependency resolution for each before/after pair. Keep the historical dev-profile calibration as its own comparison series.

Compare the following configurations on the same machine and corpus.

| Comparison | Purpose |
| --- | --- |
| Helix and Vim in the same Helix-enabled Red binary | Isolate editing-model overhead while retaining the same build and host features |
| The development build used for the report and an optimized release build | Establish how both the development workflow and shipping editor behave |
| Vim with Helix compiled out | Check feature-gating overhead at final acceptance |
| Released Helix on the shared plain-text subset | Provide an additional reference for typing and movement responsiveness |

Repair the existing runners before relying on them. Reuse their PTY driver, timing parser logic, frame capture and workload generation. Add regression checks for current and legacy timing records, startup detection and empty/incomplete samples. An absent sample set must fail the benchmark. Use mode-specific input to reach equivalent positions and verify the resulting text, caret, selection and mode.

Run performance experiments serially with builds and competing benchmark jobs finished. Alternate comparison order and retain five independent runs. Exclude a fixed 100-input warmup and verify the state where measurement starts. Use at least 1,000 measured inputs per run for cases with a p99 acceptance gate. Short diagnostic runs can report p95 and maximum. Record cold application startup separately from warmed editing. Confirm short instrumented investigations with tracing disabled.

**Deliverable** — a reproducible baseline report and validated runners that exhibit the ordinary-input problem, with raw results and binary/configuration identities.

## 2. Measure where each input spends time

Reuse `editor::perf` and the existing event, render, notification and terminal-output measurements. Add narrowly scoped spans only where they leave a gap.

| Measurement | What it resolves |
| --- | --- |
| PTY send to the output frame representing the input | Application responsiveness, including queueing and deferred rendering |
| Input receive/dequeue, command planning, edit application, selection mapping and cursor placement | Time spent in Felix and the Red adapter |
| Layout, highlighting, selection overlays, frame construction and terminal flush | Rendering and output costs |
| Plugin callbacks, LSP publication and subsequent decoration frames | Work triggered by the input after the command returns |
| CPU time, allocation bytes/counts, output bytes, frame count and queue drain time | Repeated work, memory churn, terminal traffic and backlog |

Introduce opt-in input sequence and frame-generation correlation where needed. A coalesced frame records the processed input range it represents. Record input ordering and completion independently from frame count, so batching cannot hide missing edits or motions. Use clock calibration or a common harness clock when relating process spans to PTY timestamps.

Report p50, p95, p99, maximum and stall counts for each interaction and run, along with event/sample counts. Keep handler time and input-to-output time distinct. PTY output is a proxy for display response. Confirm the improvement in the actual terminal as part of acceptance.

Take short sampling profiles of the slowest ordinary typing and movement cases. Use allocation profiling only when timings or scaling indicate allocation pressure. Preserve raw profiles and summarize the dominant stacks before choosing a fix.

**Deliverable** — an attributed latency breakdown and a ranked list of measured bottlenecks.

## 3. Workload order

Start with a compact everyday matrix. Expand the dimension associated with a slowdown instead of running every combination.

| Priority | Workloads | Dimensions |
| --- | --- | --- |
| First | Insert letters, spaces, backspace and newline with one cursor | Plain text and highlighted Rust, 100 / 2,000 / 20,000 lines, near beginning/middle/end |
| First | `h/j/k/l`, arrow keys, word movement, reversal and viewport-edge movement | Normal and Select modes, one selection, wrap on/off |
| First | Paced typing and held-key sequences | 25 ms and 10 ms spacing, then a bounded burst followed by reversal or Escape; measure drain time and final state |
| Second | Long lines, tabs, combining marks, joined emoji and CRLF | Fixed file size with varied line length, then fixed line length with varied file size |
| Second | Same-document splits, Markdown tables and inline comments | Selection/cursor correctness and layout reuse |
| Second | Completion and normal background services | Default plugins enabled; controlled LSP off/incremental/full; delayed service replies |
| Third | Multi-selection typing, deletion, undo, paste and repeat | 2 / 16 / 128 selections, visible and offscreen selections, existing historical workloads |

Use the normal plugin configuration for the initial reproduction. Disable plugins, syntax or LSP individually as diagnostic controls when necessary. After attribution, verify improvements with the normal features enabled. Final acceptance includes a populated workspace with panels as well as a single editor window.

## 4. Improve ordinary movement and typing

Implement one measured bottleneck at a time and rerun its paired baseline workload. Choose the order of changes within this phase from the profiles.

For movement, first investigate integrating Felix with Red's existing deferred and partial rendering. Classify the resolved command's effects using mode, pending input, selection changes, viewport changes and document edits. The raw key alone is insufficient because `j` can be a motion, inserted text or a pending command argument.

Partial rendering must invalidate the old and new selection overlays, primary and secondary carets, affected rows and status. Viewport changes, projected Markdown, inline comments, splits, dialogs and panel focus need explicit handling. Coalescing must preserve input order, direction changes and the final selection. Reuse Red's scheduling and rendering mechanisms where they support these requirements.

For typing, profile planning, rope edits, position conversion, undo/repeat recording, view rebasing, layout invalidation and change publication. Investigate full-document or repeated line scans, copying growing Insert history, redundant selection normalization and duplicate frames. Optimize only costs present in the measurements. Preserve the ordinary single-selection path through the same tested edit and history semantics.

Keep caches bounded and tied to the document revision, view and layout inputs that determine their contents. Reuse existing document snapshots and layout caches before introducing new retained state. Batch notifications or view rebasing only at valid transaction boundaries, preserving asynchronous completion and LSP ordering.

**First user-testable checkpoint** — one-cursor typing and movement show a measured improvement in the normal configuration, with focused behavior and frame tests passing. Provide the exact launch command and a short before/after comparison for the user's terminal.

## 5. Close scaling and regression coverage

Once ordinary editing passes its budgets, address remaining costs as selection count, document length and split count grow. Use profiles to distinguish work proportional to changed text or visible rows from repeated whole-document or per-view work. Rerun the frozen multi-selection workloads and the Vim comparisons.

Each fix runs the relevant pinned Helix differential cases and Red integration/frame tests. Add targeted regressions for any new fast path, especially selection direction, Unicode boundaries, counts, pending arguments, undo grouping, snippets, completion preview, split ownership and input reversal. Use deterministic counters to catch unnecessary full renders, duplicate publication or repeated scans where the count is a stable contract.

The completed candidate runs the full conformance suite, feature-on and feature-off Vim checks, and `cargo clippy --all-targets --all-features -- -D warnings`.

## 6. Freeze gates and hand off the result

Use the existing Red release motion-render target of 16 ms at p95 as the initial responsiveness objective for the matching release render metrics. Calibrate additional typing and input-to-output budgets from the first valid baseline and observed variability, then freeze them before optimization. Give development builds separate calibrated limits. Include absolute responsiveness limits and comparison with Vim so a uniformly slow pair cannot satisfy the goal through parity alone. Preserve the existing historical workload limits as a separate gate.

Acceptance requires the ordinary typing and movement budgets to pass in the normal configuration, acceptable queue drain under the agreed held-key workload, correct final state, and passing historical scaling and Vim regression checks. Review outliers as well as medians. A performance gain must retain the agreed Helix behavior and normal editor features.

Keep deterministic behavior and work-count checks in regular CI. Run tight timing comparisons on the recorded reference machine or a controlled performance runner. Archive per-run distributions, profiles, source/binary/configuration hashes and commands in one before/after report. The final terminal check confirms the user's original slow interaction is resolved.

The implementation can proceed in three reviewable groups — measurement and baseline, ordinary movement/typing improvements, then scaling and acceptance. Commit locally as each group is verified and batch the push handoff as already agreed.
