# Helix typing and movement performance

Measured on November 4, 2026. The initial series compared `81083077` with the editor changes through `9ff7afd5`. The [illustrated README follow-up](#follow-up-with-the-readme-images-present) measures the development-build issue reported afterward. The runs used an Apple M4 Max with 128 GiB RAM, macOS 26.7.2 arm64 and rustc 1.98.1. Both revisions used the same local Cargo lockfile, SHA-256 `2b9e8afc117389f96a2499f14266f6ef9e14b4b715b8538ead281dca90bd90a9`. That lockfile resolves pulldown-cmark 0.13.5 and wasmtime 48.0.7. Development used the normal unoptimized profile with debug info disabled. Release used Cargo's optimized profile. Incremental compilation was disabled.

## Ordinary input

The runner writes one key, then waits for a completed synchronized terminal frame showing its expected source position. Typing alternates a letter, a space and two backspaces. Movement alternates direction. A final marker and byte-for-byte saved-file check verify the destination and all edits. Elapsed time includes queueing, application work and PTY output. Physical display presentation is outside this measurement.

Each cell below gives **baseline → final** milliseconds. Each value is the median of five independent run percentiles. Each run used a fresh 2,000-line Rust document, 120 × 40 terminal, 100 warmup inputs and 1,000 measured inputs. Tracing, LSP and AI were disabled. Normal bundled plugins and syntax highlighting were enabled. Helix and Vim ran in the same executable. The execution order alternated across repetitions.

| Profile | Input | Helix p95 | Helix p99 | Vim p95 | Vim p99 |
| --- | --- | ---: | ---: | ---: | ---: |
| dev | typing | 3.158 → 2.891 | 4.545 → 3.016 | 4.331 → 4.066 | 5.797 → 4.432 |
| dev | horizontal | 1.508 → 0.549 | 2.808 → 0.601 | 1.190 → 1.062 | 1.271 → 1.120 |
| dev | vertical | 2.039 → 0.605 | 3.352 → 0.672 | 4.942 → 4.667 | 5.458 → 5.263 |
| release | typing | 0.430 → 0.338 | 0.835 → 0.378 | 0.574 → 0.492 | 0.663 → 0.589 |
| release | horizontal | 0.117 → 0.075 | 0.141 → 0.091 | 0.164 → 0.153 | 0.220 → 0.189 |
| release | vertical | 0.124 → 0.085 | 0.152 → 0.107 | 0.692 → 0.597 | 0.766 → 0.659 |

Across 15,000 measured release Helix inputs, the slowest frame took 0.73 ms. In development, two of 15,000 final inputs exceeded 16 ms and the slowest took 28.82 ms. The development baseline had two above 16 ms, with a maximum of 40.04 ms. All 24 p95/p99 comparisons passed.

The pass condition was frozen in `a9844918` before the editor changes. For each model and workload, the p95 limit is the lowest of 16 ms, the slowest baseline p95 plus 1 ms, and the slowest Vim baseline p95 plus 1 ms. At p99 the absolute cap is 33 ms. The [calibration](../scripts/felix/input-baseline.json) contains the five measurements for each comparison. [Runner commands](../scripts/felix/README.md#ordinary-typing-and-cursor-latency) reproduce the series and check the budgets.

## Real files

The 200 × 60 terminal opened disposable copies from the baseline revision. The README has 405 lines and a maximum line length of 174 bytes. The 2.12 MB editor source has 55,268 lines and a maximum line length of 406 bytes. The disposable README had no sibling image assets, so those runs measured Markdown with image placeholders. Typing started at README line 4 and source line 35,307. Development diagnostics used 40 measured inputs with tracing enabled. Release used 200 measured inputs with tracing disabled. These p95s describe the two files and positions.

| File | Profile | Typing p95 | Horizontal p95 | Vertical p95 |
| --- | --- | ---: | ---: | ---: |
| README.md | dev | 14.003 → 9.421 | 3.620 → 1.360 | 2.698 → 2.622 |
| README.md | release | 1.896 → 1.291 | 0.266 → 0.168 | 0.296 → 0.312 |
| editor.rs | dev | 6.557 → 4.651 | 1.939 → 0.902 | 1.966 → 1.000 |
| editor.rs | release | 0.667 → 0.701 | 0.161 → 0.090 | 0.164 → 0.101 |

Two release diagnostics landed slightly above their baseline. Five paired runs gave median p95s of 0.660 → 0.691 ms for typing in the large Rust file and 0.296 → 0.312 ms for vertical movement in the README.

Fresh-process startup was measured separately from editing. In the fixed PTY, launch through the first decoded cursor-bearing status frame took 2.151 → 2.149 s at the median in development and 2.094 → 2.092 s in release. Each side had five fresh processes and a warm filesystem cache.

These commands reconstruct the real-file corpus.

```sh
mkdir -p target/felix-perf/corpus
git show 81083077:README.md > target/felix-perf/corpus/README.md
git show 81083077:src/editor.rs > target/felix-perf/corpus/editor.rs
```

## Where the time went

The original README typing trace recorded 41 full renders for 40 inputs. A syntax highlight miss took 5.59 ms at p50. The first redraw change brought horizontal motion onto the cursor-row path, but README typing still spent about 3.8 ms positioning the selection after an edit and 5.6 ms recalculating highlighting. Felix command planning took about 0.01 ms. Two changes followed from these measurements.

- Local edits repaint the affected document windows, including other views of the same buffer. Ordinary one-caret movement repaints the old and new cursor rows when the viewport, selection and overlays permit it. The deterministic test compares each presented frame with a full repaint and checks the full-render count.
- Markdown highlighting can reuse the relative spans of identical inline and fenced regions. Parent styles and positions are applied at the new location. The cache is bounded by region count, bytes and span count. Table parsing still observes each revision, and equal parsed tables retain their existing window layouts. Tests cover a heading change, a new fence language, an ordinary prose edit and a fence that hides a table.

In the final README trace, each of 40 typed inputs repainted the document windows. The median highlight miss fell to 3.60 ms and selection positioning took 1.77 ms. All 40 horizontal inputs used the cursor-row path. All 40 vertical inputs at that location took the more expensive active-window path. The macOS process samples corroborate the trace. Baseline typing stacks include full window drawing and Tree-sitter parsing, and final horizontal movement goes through `render_cursor_motion_delta` and `render_window_rows`.

## Other workloads and checks

| Historical development workload | Final median, ms | Frozen limit, ms |
| --- | ---: | ---: |
| Vim undo | 4.062 | 5.281 |
| Vim paste | 5.565 | 7.401 |
| Felix delete, 2 selections | 4.064 | 4.486 |
| Felix delete, 16 selections | 5.804 | 7.051 |
| Felix delete, 128 selections | 14.333 | 18.789 |

On the feature-compiled-out Vim release, the typing, horizontal and vertical p95s were 0.503, 0.144 and 0.592 ms. Relative to that control, compiling Felix in changed Vim p95 by −0.011, +0.009 and +0.005 ms. All six feature-off comparisons passed the frozen reference limits.

- Local Red tests passed with the feature enabled (2,940 library and 49 completion tests) and disabled (2,912 library and 49 completion tests). Each configuration had three ignored tests. Both Clippy configurations passed with warnings denied. The ordinary build rejects `editing_model=helix`, and `felix-core` appears only in the feature-enabled dependency tree.
- The release binary passed the live tmux suite against Helix 25.07.1, including 170 Red checkpoints. The reference observer reproduced the stored 114-case, 3,054-input fixture byte for byte. The extended 3,682-input seed passed against both the kernel and the editor.

With the file tree, populated Agent pane and same-project editor splits on screen, all 80 ordinary movement keys per phase reached the Felix document. Inside the viewport, handler p95 fell from 1.19 to 0.56 ms, and the 80 full renders became 80 cursor-row updates. Downward and upward scrolling fell from 0.78 to 0.47 ms and from 0.72 to 0.47 ms. At the beginning and end of the file, further attempts to move still used full renders, with final handler p95s of 0.58 and 0.59 ms. These phase timings measure handlers. The ordinary-input table measures completed output.

The Unicode/CRLF fixtures verified split views, newline/backspace, arrows, selection reversal and movement in 20,000 lines. A second fixture put the cursor on joined emoji and combining marks at the start of a wrapped line. In both full and incremental LSP replay, the fixture server received exactly one document change for the eight-selection edit, its document matched the saved bytes, and Red recorded zero full renders.

### tmux replay

In tmux 3.7c at 200 × 60, Red opened a disposable copy of the README inside this worktree using a temporary copy of the user's `config.toml`. The replay omitted `preferences.json`, installed plugins and the README's local images. First-launch release notes were disabled. Each replay verified entry into Insert mode on line 4, sent 32 letter/backspace pairs and a five-letter marker, and checked the complete saved file. The table shows time from sending the 69-key burst to seeing its final marker via `tmux capture-pane`.

| Build | Median of five, ms | Range, ms |
| --- | ---: | ---: |
| Baseline development | 951 | 940–961 |
| Final development | 653 | 643–1,126 |
| Final release | 113 | 107–147 |

In a separate 10 ms-paced project replay, the final marker appeared 270 ms after the last send on baseline development, 29 ms on final development and 6 ms on final release. At 25 ms spacing those observations were 24, 25 and 6 ms. tmux client scheduling and polling contribute to these times. Final development had one unpaced run slower than the baseline range.

A shared 130-key plain-text tmux replay produced the expected saved bytes in released Helix 25.07.1, baseline Red and final Red. A single observed burst took 41, 32 and 28 ms respectively. In the separate Red PTY replay, the unpaced 16-character typing burst drained in 3.14 ms and 18 motion inputs with reversal drained in 1.74 ms. The largest final-write-to-frame observation for those workloads at 10 or 25 ms spacing was 0.56 ms. Each replay verified the final cursor with a saved marker.

## Evidence

| Executable | SHA-256 |
| --- | --- |
| Baseline dev | `3ac5d378e78be43f53029c3328ec081a9d29f66174d615d23e44d1051a184c71` |
| Final dev | `662df2ad9838447293446a8eb074bb92ff26454efca3a794b2a6bd6f6466a2d5` |
| Baseline release | `ff4fdb09c63bc2474750db49ddad0b7b42f548b06f6ead961bc7eea833b42229` |
| Final release | `39afce69bc9aad7566580ccb1dd06bd37a21145e6eadd4bb4f9d4c9f290fa98a` |
| Final release, Vim-only | `9f5644d41d5a3d5a71390c0f986a07ffa5c25b047b006ca746f50682bec03cb2` |

The raw evidence archive is `target/felix-perf/evidence.tar.gz`. The local run directory is `target/felix-perf/`. It contains before/after executables, the input corpus, per-run input latencies, trace summaries, process samples, tmux observations, conformance output and the build and test logs. The reproducible runners, frozen calibration and this report are versioned. The worktree's ignored lockfile and raw workstation evidence are local.

The numeric Red command used during fixture setup revealed a separate bug. After `:1000`, the next Felix edit could return to the previous selection. The fix makes host jumps update the active document selection. Both a native jump and a position with a nonzero Unicode column have a regression test. Timed runs use the same counted `G` setup on both revisions.

The separate signature-help fixture finds that an automatic LSP signature request is missing on a typed trigger in Helix mode on both the baseline and the candidate. Vim opens it in the control run. The ordinary-input comparison uses LSP disabled in both models. The controlled language-server and completion replays exercise their own paths.

## Follow-up with the README images present

The reported development command ran in a 152 × 94 pane. A new replay copied the three local screenshots along with the README and ran under copies of the user's configuration, preferences and plugins. It selected Kitty graphics explicitly to reproduce the negotiated graphics path in a PTY. Each sample waited for the expected cursor position in a completed terminal frame. A saved marker verified the final document.

| Development workload | Before, median per key | After, median per key |
| --- | ---: | ---: |
| Helix typing, screenshot visible | 734 ms | 10.8 ms |
| Helix alternating `j` and `k`, screenshot visible | 385 ms | 3.0 ms |
| Helix typing, two screenshots shown as text blocks | 626 ms | 6.2 ms |
| Vim typing on remote main `c42adfff`, screenshot visible | 766 ms | 23.6 ms |

Each Kitty comparison used 12 measured inputs with tracing off. The text-block comparison used eight inputs after four warmups in a nine-line document containing two README screenshots. These are individual workstation runs. In a separate private tmux replay, 13 inputs spaced 10 ms apart took 12.95 s from the first send to the visible final marker before the fix and 0.17 s afterward. Both runs saved the expected bytes.

The graphics renderer now encodes pixels when it sends them. Kitty keeps the pixels when a placement moves, disappears, or returns. The text fallback caches resized grids for multiple immutable images and sizes. The viewport also scrolls toward a visible cursor in the top margin. Previously it could scroll in the opposite direction for an entire screen before repositioning.

In a 220-key `j`/`k` traversal of the illustrated README, the fixed development build took 6.6 ms at p95 and 17.1 ms at p99. The slowest key took 654 ms when the second screenshot appeared for the first time. The trace recorded one image encode during the traversal. First-time image preparation and transfer still run on the input thread.

The development and release builds passed all 24 frozen input-latency checks and all five historical replay budgets. On the standalone Vim backport, all 36 Markdown image tests and 17 navigation tests passed.

Commit `7733e37c` contains the shared Rust fix and its regression tests. It applies cleanly to `c42adfff` and builds with the default Vim editing model. The worktree development binary for the follow-up has SHA-256 `d58b037498ac55d191c74c5d6c0074748ec4e48d007962c7ae9e5cccbc51ade1`. The patched main binary has SHA-256 `8cfca38c3a83d220d22b584d37b62bde0dc1bcec402c350a3a160955a0cfa7d0`. The [replay instructions](../scripts/felix/README.md#ordinary-typing-and-cursor-latency) exercise unchanged-image reuse in the development build.
