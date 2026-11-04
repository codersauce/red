# Drawing and navigation measurements, 4 November 2026

Cursor moves inside the viewport got faster at the median on every Mac comparison. In the final seven-pair run, median latency fell from 1.76 to 0.98 ms while p95 moved from 3.79 to 3.73 ms. On Linux, in-viewport p95 fell from 1.82 to 1.15 ms and downward scrolling from 2.47 to 2.20 ms. Window and buffer switching showed no consistent gain across runs.

## What changed

* The indentation-guide plugin reuses its decorations while adjacent cursor rows have the same effective indentation level, viewport and document state. When it does need to rebuild, it measures the visible source rows in one pass and resolves pending blank rows at their next nonblank neighbour.
* The cursor redraw path compares the affected rows with the previously flushed frame. This removes the per-move row snapshots. Resized and invalidated frames take the full render path. Wide glyph continuations use the same diff logic as a full frame.
* Short traces record dispatch and successful flush IDs, dispatch ages and cursor/viewport/document state. Timings distinguish cell comparison, encoding/queueing and flushing. Bounded recording stores fixed-size histograms for long sessions.
* Windows graphics detection now sends a terminal query only when the backend can consume its reply. The original Windows trace showed the device-attributes response arriving at Red as a sequence of normal keys.
* The new native replay drives Red through PTY or ConPTY, decodes terminal output and verifies the saved documents. The existing Unix workspace runner now recognizes completed spans in the current trace format. The proposed CI job runs the replay and full-frame navigation tests on Linux, Windows and macOS.

## Paired measurements

Each cell gives the median of seven per-run p95s, base → candidate, in milliseconds. The clock runs from the send of a navigation command through the driver's observation of the expected active position on the statusline. Each measured run also verifies the saved fixture. These timings cover guest-local terminal output. They exclude display scanout and SSH/RDP delivery.

| Platform and build | In viewport | Scroll down | Reverse | Switch window | Switch buffer |
| --- | ---: | ---: | ---: | ---: | ---: |
| macOS ARM64, full change | 1.074 → 0.528 | 1.799 → 1.681 | 1.408 → 1.348 | 2.135 → 0.711 | 0.588 → 0.601 |
| Linux x86-64 GNU, full change | 1.822 → 1.154 | 2.470 → 2.202 | 2.092 → 2.025 | 0.991 → 1.068 | 1.078 → 1.113 |
| Linux x86-64, plugin comparison | 1.848 → 1.116 | 2.781 → 2.557 | 2.213 → 2.028 | — | — |
| Windows x86-64 MSVC, plugin comparison | 16.272 → 16.315 | 16.362 → 16.376 | 16.333 → 16.324 | — | — |

The Mac comparison used 120 operations per run and the full Linux comparison used 64. Both ran at 25 ms spacing. An earlier Mac comparison using 200 operations measured 1.014 → 0.508 ms inside the viewport.

Mac tail latency was sensitive to system load. The main comparison measured 0.938 → 0.438 ms at p50 and 1.074 → 0.528 ms at p95 inside the viewport. In a repeat during other work, p50 improved from 1.802 to 0.841 ms while p95 worsened from 4.887 to 7.145 ms. A same-binary control also moved substantially. After the campaign's builds and soak finished, the final binary measured 1.757 → 0.984 ms at p50 and 3.789 → 3.728 ms at p95. The 16-logical-CPU Mac reported a one-minute load average of 31.65 during that comparison. The final window-switch p95 was 1.425 → 2.007 ms. The in-viewport median improvement reproduced. A reliable Mac tail-latency reduction needs a quieter reference host.

A second Linux window and buffer comparison reversed the direction of the first result. Window switching measured 1.050 → 0.967 ms and buffer switching 1.322 → 1.169 ms. A same-binary control measured 1.129 → 1.146 and 1.164 → 1.251 ms respectively. Treat Linux switching performance as unresolved.

The plugin comparisons kept the native CI binary fixed on each host and overlaid the candidate indentation-guide plugin. They used 120 operations per run at 5 ms spacing. On Windows both variants disabled Markdown images to keep capability replies out of the old binary's input queue. The guest lacked Git, so its bundled Git plugin stopped at startup.

The Windows native candidate also completed a 40-key default-settings ConPTY replay. A full five-scenario comparison stopped during the first scenario when Defender quarantined the newly built GNU replay driver. Direct Windows Terminal verification exercised scrolling, splits, shared-buffer views, switching, editing and saving with the complete candidate. Both 100-line files matched their expected bytes with markers on lines 6 and 76. The interrupted comparison cannot establish a Windows host-renderer speedup. A native MSVC run of the checked-in matrix is still needed.

## Correctness, pressure and remaining costs

On macOS the separate 10,000-key reverse run measured p95 1.732 ms, p99 2.583 ms and max 15.571 ms. A separate 512-key zero-delay burst confirmed eight batch boundaries and the saved document. Direct Terminal navigation restored the cursor to line 61 after switching splits and to line 1 after switching buffers. The 200 × 60 tmux workspace included an editor split, a populated deterministic Agent panel and NeoTree. Four retained captures cover scrolling both ways and resizing back to the original dimensions.

The 30-minute Mac reverse-navigation soak sent 72,000 operations into a 40,000-line Unicode fixture at 25 ms spacing. All 1,126 observed batch boundaries and the saved file verified. The stream carried 66.5 MB. Last-send-to-boundary latency was 4.41 ms p95, 11.97 ms p99 and 73.21 ms maximum. In 118 process samples, RSS rose from 100.5 MiB to 119.8 MiB, also the sampled peak. This single candidate run leaves the long-term retention ceiling open. It exercised sustained navigation with an edit at the end.

A 30-second idle run emitted zero bytes on each build and used 0.50 seconds of Red CPU on the baseline and 0.38 on the candidate. The candidate accounted for all 60 hover events, 60 wheel events and 240 mixed events in the mouse replay.

Both Mac builds preserved the exact final document on 10k, 100k and one million Unicode lines, on 10 KiB, 100 KiB and 1 MiB unwrapped JSON lines, and on a wrapped CRLF fixture with tabs and relative numbers. These were single stress runs. The existing bundled Husk callback gate passed at 0.995 ms p95 for 2,000 callbacks against its 4 ms limit.

In a 200-command Mac trace, the candidate's in-viewport p95s were 108 µs in the cursor notification, 112 µs for cursor redraw, 4 µs for row comparison, 6 µs for encoding/queueing and 12 µs for flush. For downward scrolling the cursor notification took 939 µs, the motion frame 304 µs, encoding/queueing 39 µs and flush 67 µs. Across 220 dispatch-to-flush observations, including setup and teardown, oldest-dispatch p95 was 0.606 ms in the in-viewport run and 1.831 ms in the scrolling run. These are nested, instrumented samples. The notification on a changed viewport remains the larger opportunity. A four-second macOS process sample of a wider scrolling viewport also showed time in terminal writes. Backpressure deserves separate measurement before changing the output protocol.

## Reproduction and evidence

Base source `6706f4194acbcc71eaa00b9c58ce756ab911adcb`. The real Rust fixture was a frozen copy of that revision's `src/editor.rs`, SHA-256 `275434b6dd898048d975e7fdb428521e4819e6b243d927b881a34f20460760b3`. The local binary/fixture manifests and raw data are indexed in the evidence bundle. The native CI artifacts used in the plugin comparisons came from GitHub Actions run `41858505923`. Candidate overlay SHA-256 `90d27b97467f8780de74d128d80045799ca63e5aabb9bfed5af89a1dd36c06ec`.

The full builds used Rust 1.98.1 and the temporary dependency mirror. Both sides used lockfile SHA-256 `3f2c5f10f1ed50537ad7167c628040ca8a3314a17678ba16d35ef744bac89df6`, including pulldown-cmark 0.13.5 and Wasmtime 48.0.7. The override lives in the temporary build directory. Linux and Windows full builds were cross-compiled for x86-64 GNU and exercised in native guests. The last recorder edits corrected trace coordinates and made counter and gauge names static. The timed runs used instrumentation off.

The local reference machine was an Apple M4 Max with 16 logical CPUs and 128 GiB RAM on macOS 26.7.2, with Terminal and tmux 3.7c for interaction checks. The fallback guests were Ubuntu 24.04.5 with four available CPUs and Windows 11 Pro build 26200 with seven logical CPUs. Both reported an Intel Xeon 6975P-C and approximately 8 GiB RAM. Devbox authentication and the Cloud PC security-key prompt prevented testing the usual SSH and RDP routes.

The runbook contains build and replay commands. All 2,913 enabled library tests passed serially on the Mac. Three tests were ignored. All-target, all-feature Clippy passed with warnings denied. Individual logs, hashes, JSON results and macOS samples are in this worktree's ignored `target/navigation/` directory. The release-lock CI matrix, an Intel Mac performance reference, Windows release-build tail latency and remote presentation timing remain open. [Native replay runbook](../tools/navigation-bench/README.md) · [Campaign plan](EDITOR_DRAWING_NAVIGATION_PERFORMANCE_PLAN.md)
