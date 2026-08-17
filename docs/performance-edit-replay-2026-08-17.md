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
