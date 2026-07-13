# Red performance baseline — 2026-07-13

This study compares the unmodified `a5002b9f861c57db6b9d73c7710f78c988dcdc0f`
checkout with the `perf/optimization` working tree on the same workstation and
release profile. The goal is the latency a user experiences while opening,
moving, typing, searching, and filtering files, not an isolated microbenchmark.

## Environment and fixtures

- macOS 26.5.2 (25F84), Darwin 25.5, arm64; MacBook Pro `Mac16,5`, Apple M4 Max,
  128 GiB memory.
- `rustc 1.94.1 (e408947bf 2026-03-25)` and Cargo 1.94.1; `--locked --release`.
- Red fixture: `src/editor.rs`, 23,765 lines/888,319 bytes before and
  24,038 lines/898,849 bytes after the added implementation and tests.
- Large-repository picker fixture: 5,382 Codex files with the 444,610-byte
  `chat_composer.rs` preview.
- Pathological fixture: a real 3,253,957-byte, single-line JSON containing
  emoji. LSP was disabled for the PTY runs, as in the release runbook.

## Before/after comparison

All interaction figures are p95 milliseconds; lower is better. Standard-scroll
before values are the median of two runs and after values are the two observed
runs (the final run is shown). Terminal volumes use the scripts' reported KiB.

| Scenario / metric | Before | After | Change |
|---|---:|---:|---:|
| Husk `indent_guides` cursor callback | 1.659 | 1.549 | -7% |
| Scroll 50x120 startup | 35.3 | 30.1 | -15% |
| Scroll 50x120 plugin startup | 24.0 | 19.3 | -20% |
| Scroll 50x120 key event | 1.82 | 1.60 | -12% |
| Scroll 50x120 full render | 0.76 | 0.55 | -27% |
| Burst scroll 80x200 key event | 5.12 | 1.38 | -73% |
| Burst scroll cursor callback | 3.04 | 0.87 | -71% |
| Burst scroll full render | 2.90 | 0.47 | -84% |
| Burst scroll motion frame | 2.52 | 0.18 | -93% |
| Burst scroll terminal output | 7,323 KiB | 1,803 KiB | -75% |
| Typing, alternating ASCII/Unicode | 5.20 | 1.47 | -72% |
| Typing full render | 2.86 | 0.64 | -78% |
| Incremental `/self` search | 38.3 | 2.87 | -92% |
| Incremental-search full render | 24.1 | 1.58 | -93% |
| Red file picker/filter | 128.4 | 0.72 | -99% |
| Red picker full render | 115.4 | 0.63 | -99% |
| Codex file picker/filter | 56.3 | 4.79 | -91% |
| Codex picker full render | 54.6 | 2.53 | -95% |

The standard 200-key scroll produced 634 KiB after versus 628 KiB before. Husk
was run three times after the change: p95 was 1.549, 3.213, and 1.541 ms; all
passed the 4 ms gate and the median was 1.549 ms. The middle run had a 48 ms
host-scheduling outlier.

The single-line fixture did not produce a first frame within 12 seconds with
wrapping before these changes. With `nowrap`, its first paint was about 338 ms,
events were about 268 ms, and an emoji debug statement generated 414,557 KiB of
log output. With wrapping enabled after the change, a repeated run produced a
first paint in 79 ms, typing p95 of 8.59 ms (20 keys), common-match search p95
of 1.73 ms, and 48/30 KiB of logs respectively. A cold first launch was
occasionally about 1.2 s even though the measured interactive frame remained
under 30 ms; subsequent launches were 52-79 ms. The pathological case is now
usable and below the 16 ms interaction budget instead of failing to paint.

The detached-owner run with 120 edits, resize/mouse interaction, reattach, and
a 1.5 MiB Unicode bracketed paste completed in 3.53 s with 116 KiB of output;
`detach:serialize_frame` was 44 µs p95/81 µs max, 390 idle polls skipped
serialization, and the largest delta was 23 rows. The paste completed without
the pre-change intermittent PTY-write stall.

## What changed

- Long physical lines now lay out and render only visible graphemes. Highlight,
  legacy line redraw, plugin viewport snapshots, and diagnostic work are
  bounded; wrapped continuation rows no longer copy a complete physical line.
  Full-line emoji/debug logging was removed.
- Search caches matches per buffer/revision/query, converts regex byte offsets
  in one pass, draws only visible matches, and finds a single directional match
  when a visible physical line is oversized.
- Picker previews are lazy, bounded, and cached with an eight-entry MRU plus
  metadata invalidation; visible-window highlighting is capped. Filtering
  retains item indices instead of repeatedly cloning complete dynamic items.
- Edits avoid full-line position scans and full-buffer anchor scans. Disabled
  LSP no longer materializes the buffer; active LSP reuses one resolved client,
  moves full-sync payloads, and avoids redundant serialization/copies. Recovery
  snapshots share the Rope and flatten on the writer thread.
- Plugin validation preserves both host-API and semantic diagnostics while
  reducing startup parsing from three passes to two and avoiding successful-path
  source copies. Timer polling is linear; Git, project search, inlay hints,
  barbecue, and fidget cancel stale work and deduplicate split-window requests.

## Reproduction and validation

```shell
cargo fmt --all -- --check
cargo test --all-targets --all-features -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
cargo build --locked --release
cargo run --locked --release --example husk_cursor_bench -- --assert
target/release/red --self-check
python3 scripts/scroll_bench.py 50 120 200 25
python3 scripts/scroll_bench.py 80 200 500 5
python3 scripts/detach_bench.py 50 120 120 1536
python3 scripts/interaction_bench.py typing
python3 scripts/interaction_bench.py search --query self
python3 scripts/interaction_bench.py picker --query src/editor.rs
python3 scripts/interaction_bench.py picker \
  --root ../codex \
  --file ../codex/codex-rs/tui/src/bottom_pane/chat_composer.rs \
  --query chat_composer.rs
```

The complete suite passes serialized. Two ACP conformance tests can exceed
their existing 500 ms deadline when the suite is run in parallel on a busy
host; both pass serialized and are unrelated to the interaction changes.
