# Red performance baseline — 2026-07-19

This pass compares the clean `8e20896a45d71bc7396ad493d044d34979ef447e`
release build with the rendering and editor-hot-path working tree on the same
macOS arm64 workstation. LSP was disabled for the PTY scenarios. All timings
below are p95 microseconds unless noted; lower is better.

Environment: Darwin 25.5.0 arm64, `rustc 1.94.1 (e408947bf 2026-03-25)`,
Cargo 1.94.1, `--locked --release`.

| Scenario / metric | Before | After | Change |
|---|---:|---:|---:|
| Husk `indent_guides` cursor callback | 1,947 | 922 | -53% |
| Scroll 80x200 key event | 2,623 | 1,467 | -44% |
| Scroll cursor callback | 998 | 714 | -28% |
| Scroll full render | 1,232 | 655 | -47% |
| Scroll diff and flush | 731 | 158 | -78% |
| Scroll motion frame | 347 | 303 | -13% |
| Typing key event | 2,646 | 1,605 | -39% |
| Typing full render | 1,246 | 789 | -37% |
| Typing highlight miss | 881 | 568 | -36% |
| Incremental-search key event | 5,752 | 3,341 | -42% |
| Incremental-search full render | 1,027 | 538 | -48% |
| Incremental-search diff and flush | 500 | 98 | -80% |
| Picker full render | 4,077 | 3,557 | -13% |
| Picker chrome render | 3,841 | 2,145 | -44% |
| Detached frame serialization | 48 | 44 | -8% |

The Husk result was stable across four post-change runs (p95 922, 921, 904,
and 927 us). The PTY numbers are single runs and include scheduling noise.
Two noisy tails moved in the opposite direction: search action resolution was
1,800 to 2,638 us p95 (p50 1,079 to 1,152 us), and picker key events were 988
to 3,791 us p95 while p50 improved from 392 to 344 us. Picker output stayed at
16 KiB. Scroll output was 2,023 to 2,105 KiB and the post-change run processed
all 500 cursor callbacks versus 480 before. Detached output stayed at 101 KiB;
serialization max improved from 113 to 62 us.

Targeted release-dependency microbenchmarks also confirmed the algorithmic
changes: the linear word-start scan improved approximately 68x on an 8 KiB
identifier, tab-aware grapheme-column conversion improved 35-42%, boundary
lookup improved 31-47%, display-width truncation improved 17-26%, and
display-width fitting improved 12-24%. A proposed `grapheme_to_char` rewrite
regressed long ASCII lines by about 15% and was intentionally discarded.

## What changed

- Rendering now indexes layout rows directly, reuses previous-frame cell text,
  bulk-fills rows and dialog rectangles with one RGBA blend, bounds diagnostic
  grouping to visible lines, and draws split separators once with linear edge
  discovery. Splitting a non-final window now advances traversal correctly,
  preventing unintended duplicate splits and stable-window-id collisions.
- Cursor motion, backspace, word scanning, and Unicode-width helpers avoid
  redundant full-line passes and temporary grapheme collections. Undo/redo no
  longer clones transaction text, selective revert compares Rope slices, and
  whole-buffer replacements avoid flattening merely to find their end.
- Syntax highlighting caches capture and Husk styles. Completion filtering
  retains indices and uses allocation-free ASCII matching. LSP document
  selectors, active clients, and polling order are cached instead of being
  rebuilt on every request or poll.
- Dynamic pickers no longer materialize and truncate every item's display
  string. Command-column maxima are cached per filtered result set, retaining
  stable alignment while scrolling. The indent-guide plugin preserves host
  widths and processes blank runs and active scopes linearly.

## Reproduction and validation

```shell
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features -- --test-threads=1
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked --release
cargo run --locked --release --example husk_cursor_bench -- --assert
target/release/red --self-check
python3 scripts/scroll_bench.py 80 200 500 5
python3 scripts/interaction_bench.py typing --cycles 160 --delay-ms 5
python3 scripts/interaction_bench.py search --query self --cycles 16 --delay-ms 5
python3 scripts/interaction_bench.py picker --query src/editor.rs --cycles 12 --delay-ms 5
python3 scripts/detach_bench.py 50 120 100 512
```

All 1,382 Rust tests passed serialized; the two home-directory expansion tests
require filesystem access outside the workspace sandbox. Clippy passed with
warnings denied, all bundled plugins passed self-check, and the Husk callback
remained well below the 4 ms release gate.
