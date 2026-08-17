# Red repeated-edit performance plan

## Evidence

Inspected on 2026-08-17 at `20f4cc2c36012a1399749737922b1faa65fc3506`.
The primary checkout stayed clean. Local `origin/main` was `311a72e`, eight
commits ahead; its syntax-indentation and signature-help changes were inspected.
They do not replace the replay/render paths below. No running editor was touched.

A one-off audit harness linked the inspected debug library and exercised the
production dispatcher with default keymaps, `RED_PERF=trace`, disabled
LSP/terminal output, and assertions on resulting text.
It does not measure a live terminal, real language server, or loaded plugins.
These are diagnostic samples, not release benchmarks or promised speedups.

| Operation | Fixture | Time | Full renders | Highlight misses |
| --- | --- | ---: | ---: | ---: |
| Dot-repeat, 32 characters | 1,487,538-byte Rust source | 143.4 ms | 34 | 32 |
| Bulk insert, 32 characters | Same source | 4.2 ms | 1 | 1 |
| Dot-repeat, 128 characters | Same source | 638.6 ms | 130 | 128 |
| Bulk insert, 128 characters | Same source | 5.5 ms | 1 | 1 |
| Macro inserting 128 characters | Same source | 637.8 ms | 131 | 129 |
| Undo that insertion | Same source | 9.6 ms | 1 | 1 |
| `128x` | 256-character line | 339.6 ms | 131 | 129 |
| Replicate 32 characters onto 15 more block rows | 16 short Rust lines | 3,441.1 ms | 481 | 480 |

An earlier dot/bulk run measured 654.0 ms versus 5.8 ms for 128 characters.
The second run spent 476.9 ms inside full-render spans and 9.6 ms inside
character-replacement spans. Nested spans must not be added together.

## Audit inventory

- **Dot, macros, counts, and chained actions:** the [replay drivers](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L11692)
  dispatch individual actions. Some operators already accept semantic counts,
  but the generic fallback still repeats the whole action. Highest priority.
- **Visual-block insert/change:** [execute_on_block](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L20699)
  defers notifications and suppresses terminal output, but still constructs
  scratch frames for each action on each row. Highest priority.
- **Ordinary typing/deletion and split windows:** [render_edited_window_rows](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor/rendering.rs#L1582)
  calls the full renderer. The [dispatcher tail](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L20265)
  can render again for bounds changes or multiple windows.
  [Highlight caches](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L6237) invalidate
  on every content revision.
- **LSP, plugins, inline summaries, and completion:** [notify_change](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L21294)
  materializes full document text for LSP, awaits `buffer:changed`, refreshes
  summaries, and schedules inline completion. Incremental LSP sync still
  [compares old/new full strings](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/lsp/client.rs#L750).
  Diagnostics are already debounced; document synchronization is not.
- **Substitute-all, indentation, comments, completion edits, and plugin document
  transactions:** [substitution](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L14558),
  [indentation](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L16732),
  [comments](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L22774), and
  [plugin transactions](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editor.rs#L21599) are
  already partly batched. Profile their remaining per-replacement anchor,
  history, annotation, and dirty-state work before changing them.
- **Undo/redo and whole-document replacements:** [undo history](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/undo.rs#L356)
  retains individual replacements. Undo already renders once in this sample.
  Visual paste, agent whole-file edits, and prepared LSP workspace edits can
  replace a whole document; smaller edits may reduce copies and undo memory.
- **Embedded text areas:** [TextArea replay](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editing/textarea.rs#L1530)
  has a separate key-replay implementation and
  [whole-text coordinate calculations](https://github.com/codersauce/red/blob/20f4cc2c36012a1399749737922b1faa65fc3506/src/editing/textarea.rs#L1724).
  Benchmark large composer text separately; this component has no internal LSP
  or terminal work.
- **Newer main:** include syntax indentation and signature help in the baseline.
  Keep indentation decisions that affect later edits synchronous; defer only
  speculative UI work when safe.

## Implementation order

### 1. Permanent baselines

Refresh main and use branch `fcoury/edit-replay-performance` in durable sibling
worktree `red.fcoury-edit-replay-performance`. Extend
`scripts/interaction_bench.py` or add a focused replay benchmark. Add counters
for renders, highlight misses, replacements, LSP calls/bytes, plugin events,
and inline-summary refreshes. Measure short/long inserts, large files, LSP and
plugins off/on, shared-buffer splits, wrapping, and retained inline history.

### 2. Batch replay rendering

Add a small nested execution-batch boundary outside the large recursive
dispatcher. Keep mutations, marks, revisions, selections, modes, and undo
transactions exact. Accumulate render invalidation and commit one correct final
frame for short commands. Apply it to dot, macros, generic counted/chained
actions, and visual-block replay. Reuse existing navigation/block deferral where
their semantics agree. Long macros need bounded work slices, cancellation, and
background-service points. Preserve recursion limits and restore batch state on
errors. Do not blindly convert arbitrary recorded keys into a paste.

### 3. Batch notifications safely

Track dirty buffers by stable `BufferId` and latest revision. Flush before LSP
requests requiring current text, saves, document switches/closure, and other
observable barriers. Define plugin event semantics before coalescing events:
macros/plugins that depend on intermediate state must still see it. Preserve
notification retry behavior. A plain single-buffer insertion replay should need
one final change notification and one completion scheduling pass.

### 4. Make individual edits cheaper

Feed canonical replacements to incremental LSP synchronization, retaining
full-sync support and correct UTF-16/CRLF conversion. Add safe changed-region
rendering for interactive edits, with full-redraw fallbacks for syntax context,
wrapping/layout changes, overlays, and shared-buffer windows. Preserve the
existing committed-frame and document-aware highlighting invariants.

### 5. Profile and optimize the remaining bulk paths

Candidates: compact provably adjacent undo edits; defer derived history and
annotation refreshes while transforming live anchors in order; implement more
direct counted operations; retain smaller edit ranges for whole-document plans;
cache or use rope-based coordinates in embedded text areas. Make these separate,
measured changes rather than one editor rewrite.

## Acceptance and validation

- Compare final text, cursor, mode, registers, marks, selection, dirty state,
  undo/redo, and rendered cells against the existing behavior.
- Preserve location-relative dot semantics, literal periods, Unicode/emoji,
  CRLF, tabs, EOF/final-newline edits, autoindent, snippets, visual-block padding,
  nested macros, error recovery, and multi-buffer save/LSP/plugin barriers.
- Assert work counts instead of machine-dependent timing thresholds. A short
  plain-text repeat must not do one full render/highlight per character.
- Keep the explicit 2 MiB-stack visual-block/dot regression. Avoid increasing
  the recursive dispatcher frame.
- Run focused tests, the full suite serially (`--test-threads=1`), formatting,
  and `cargo clippy --all-targets --all-features -- -D warnings` before pushing.
- Capture release-build PTY before/after evidence for dot, macros, block change,
  cancellation, undo, and shared-buffer splits before claiming completion.

Audit validation: `cargo build --locked --lib -p red` passed; all harness
assertions passed; `cargo test --locked --test editing dot_ -- --test-threads=1`
passed 12 tests at the inspected commit. The audit changed no production source
and did not push or open a PR.


## Implementation results

Implemented on `fcoury/edit-replay-performance`, based on `122c0b8`. The
original audit above remains a debug-build diagnosis; the table below is a
separate release-build comparison against that exact implementation base.

The shared edit batch keeps canonical mutations and undo boundaries synchronous,
coalesces derived notifications and rendering, and flushes before external
operations. Short repeats publish one final frame; long repeats use 16 ms / 512
step checkpoints and accept Ctrl-C without dropping ordinary queued input.
Visual-block replay no longer constructs hidden scratch frames. Ordinary edits
reuse the document-aware editor-window renderer, including all shared-buffer
windows, with existing full-frame safety fallbacks.

Canonical edits now carry UTF-16 ranges and shared Rope snapshots to incremental
LSP clients. Exact-preimage checks, revision gaps, full-sync servers, and unusual
line separators retain the full-text fallback. Adjacent insert undo records are
compacted within their existing transaction. Embedded text areas batch safe
ASCII insert runs and retain per-key behavior at Unicode grapheme boundaries.

### Release PTY comparison

Measured on macOS arm64 with Rust 1.94.1, release builds, a 120 × 40 PTY,
2,000 Rust lines, `RED_PERF=trace`, and isolated configuration. These are single
local diagnostic samples, not CI timing thresholds. The benchmark verifies the
saved text for every case.

| Operation | Before | After | Before / after notifications |
| --- | ---: | ---: | ---: |
| Dot, 128 characters | 68.453 ms | 7.154 ms | 128 / 1 |
| Macro, 128 characters | 69.171 ms | 6.309 ms | 128 / 1 |
| `128x` | 74.732 ms | 23.243 ms | 128 / 2 |
| Block insert, 16 rows × 128 characters | 1,593.209 ms | 91.423 ms | 1 / 1 |
| Bracketed paste, 128 characters | 1.342 ms | 1.006 ms | 1 / 1 |
| Substitute across 2,000 lines | 7.649 ms | 7.977 ms | 1 / 1 |
| Indent 16 lines | 0.932 ms | 0.924 ms | 1 / 1 |
| Undo insertion | 1.092 ms | 0.799 ms | 1 / 1 |

Dot and macro full renders fell from 130 to 1. Block full renders fell from
1,921 to 1. Counted deletion used two bounded publication slices in this run.
With bundled plugins and a shared split, 64-character dot fell from 95.954 ms
to 4.879 ms; the corresponding block replay fell from 1,604.028 ms to 40.059 ms.
For a 32 KiB embedded text area, dot-128 fell from 65.100 ms to 0.764 ms with
ASCII context, and from 135.057 ms to 2.349 ms with Unicode context.

The local protocol fixture reconstructed the final document for dot, macro,
counted delete, block, paste, substitute, indent, and undo using an incremental
server, emoji, and CRLF. Dot used one range/notification instead of 128 and
measured 10.009 ms versus 113.038 ms. With full synchronization, dot sent
37,146 text bytes in one notification instead of 4,746,560 bytes in 128.
The fixture also checks strictly increasing document versions.

Reproduce with `scripts/edit_replay_bench.py`; use `--plugins --split`,
`--lsp incremental --unicode --crlf`, or `--lsp full` for the additional matrices.
Use `cargo run --locked --release --example textarea_replay_bench` for the
embedded text-area comparison. Deterministic tests assert work counts and
semantic results instead of machine-dependent timing limits.

### Scope of the remaining candidates

Substitute, indentation, paste, and undo already publish efficiently in the
release baseline. Their existing transaction paths were retained and are covered
by the PTY/protocol matrix. Anchor transformations still run in edit order;
rewriting them or whole-document edit plans is not justified by these samples.
Counted deletion retains its existing register and undo semantics rather than
silently replacing repeated commands with a different bulk operation. A future
profile can target those remaining mutation costs independently.

Validation: `cargo test --locked --all-targets --all-features -- --test-threads=1`
passed 2,784 tests with one ignored. Strict all-target/all-feature Clippy,
`cargo fmt --all -- --check`, and `git diff --check` passed.
