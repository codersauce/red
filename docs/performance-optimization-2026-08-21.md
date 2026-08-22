# Cross-subsystem performance optimization plan

This pass treats Red as a collection of measurable runtime paths rather than one
overall benchmark. Every optimization must preserve editor behavior, Unicode and
terminal correctness, plugin isolation, owner-only persistence, recovery safety,
and the existing release gates in [performance.md](performance.md).

## Runtime areas

| Area | What to measure | Representative code or fixture |
| --- | --- | --- |
| Process and CLI startup | Argument parsing, executable load, configuration, themes, assets, and time to first paint | `src/main.rs`, `src/config.rs`, `scripts/interaction_bench.py` |
| Bundled plugin startup | Discovery, metadata, compilation, semantic checks, activation order, ready callbacks | `src/plugin/registry.rs`, `src/plugin/runtime.rs` |
| Interactive event loop | Input polling, event batching, dispatch, background servicing, and redraw decisions | `src/editor.rs`, `src/editor/edit_batch.rs` |
| Buffer storage | Rope reads, line access, insertions, replacements, large files, CRLF, and snapshots | `src/buffer.rs`, `scripts/edit_replay_bench.py` |
| Vim motions and editing | Word and paragraph movement, operators, visual mode, macros, repeat, and counted edits | `src/editing`, `src/editor/edit_batch.rs` |
| Undo and redo | Transaction construction, branching, history limits, attributed edits, and replay | `src/undo.rs`, `scripts/edit_replay_bench.py` |
| Frame composition | Window painting, chrome, overlays, dialogs, panels, cursor drawing, and terminal diffs | `src/editor/rendering.rs`, `scripts/interaction_bench.py` |
| Terminal cell writes | ASCII fast paths, graphemes, emoji, combining marks, width clipping, and style reuse | `src/editor/render_buffer.rs` |
| Layout and wrapping | Split geometry, viewport layout, soft wrapping, gutters, horizontal scrolling, and resize | `src/editor.rs`, `src/window.rs` |
| Syntax and highlighting | Tree-sitter parsing, incremental invalidation, visible-range highlighting, and brackets | `src/highlighter.rs`, `src/editor/rendering.rs` |
| In-buffer search | Regex matching, incremental query changes, next/previous navigation, and highlights | `src/buffer.rs`, `src/editor.rs` |
| Workspace search | File traversal, ignore handling, ripgrep processes, streamed results, and previews | `plugins/project_search.hk`, `src/plugin/process.rs` |
| File and structured pickers | Discovery, fuzzy ranking, monotonic refinement, parallel scoring, previews, and selection | `src/ui/file_picker.rs`, `src/ui/picker.rs` |
| Completion and signatures | LSP item filtering, ranking, documentation resolution, snippets, and signature help | `src/ui/completion.rs`, `src/editor/completion_resolve.rs` |
| Language-service lifecycle | Server startup, document synchronization, requests, cancellation, diagnostics, and progress | `src/lsp`, `src/editor/lsp_coordinator.rs` |
| Husk language analysis | Parsing, semantic analysis, document updates, config refresh, and completion ranking | `crates/husk-analysis`, `crates/husk-lsp` |
| Husk compilation and execution | Host declarations, type checking, module loading, callback dispatch, and instruction budgets | `crates/husk-runtime`, `examples/husk_cursor_bench.rs` |
| Plugin event delivery | Snapshot publication, owner isolation, subscriptions, request dispatch, and payload decoding | `src/plugin/runtime.rs`, `src/plugin/registry.rs` |
| Plugin timers and processes | Idle polling, deadline selection, process output, directory watches, and cleanup | `src/plugin/runtime.rs`, `src/plugin/process.rs` |
| Decorations and gutters | Namespace replacement, line indexing, layering, sign priority, and viewport updates | `src/plugin/decoration.rs`, `src/plugin/gutter.rs` |
| Panels and workspaces | Structured rows, text transcripts, cursor positioning, search, focus, and split rendering | `src/plugin/panel.rs`, `src/plugin/workspace.rs` |
| Neo-tree and file trees | Directory enumeration, tree virtualization, row selection, refresh, and memory growth | `src/plugin/tree.rs`, `scripts/neotree_bench.py` |
| Git integration | Repository discovery, status refresh, signs, hunk generation, diff panes, and subprocess budgets | `plugins/git.hk`, `scripts/git_workspace_bench.py` |
| Detached sessions | Owner startup, IPC, frame deltas, styled rows, paste, resize, reconnect, and idle work | `src/headless`, `scripts/detach_bench.py` |
| Crash recovery | Snapshot capture, serialization, undo trees, atomic rotation, disk divergence, and resume | `src/session.rs`, `src/editor/session_manager.rs` |
| Preferences and plugin storage | Histories, workspace layouts, Agent state, repeated writes, batching, and retry behavior | `src/preferences.rs` |
| Agent bridge and sessions | App-server startup, thread restore, streaming, permission prompts, tool calls, and cancellation | `src/codex`, `src/editor/agent_manager.rs` |
| Agent transcripts and composer | Streaming deltas, transcript layout, markdown, navigation, composer wrapping, and links | `src/agent_conversation.rs`, `src/plugin/panel.rs` |
| Inline assistance and history | Selection context, inline comments, request lifecycle, history rendering, and outcome storage | `src/editor/inline_history`, `src/inline_assist.rs` |
| JSON and boundary conversion | Host payload decoding, owned strings, nested values, immutable sharing, and allocations | `crates/husk-runtime/src/lib.rs`, `src/plugin/runtime.rs` |
| Memory and cache behavior | Resident memory, allocations, retained capacities, cache invalidation, and large-workspace growth | PTY benchmarks plus representative 2,048/8,192-entry workspaces |
| Platform-specific I/O | Unix sockets, terminal behavior, Windows handles, no-follow file access, and clipboard setup | `src/headless`, `src/session.rs`, `src/clipboard.rs` |
| Instrumentation and release gates | Benchmark noise, pinned baselines, p50/p95, trace overhead, deterministic tests, and CI budgets | `src/editor/perf.rs`, `docs/performance.md` |

## Verified hotspot results

Measurements use release builds, alternating before/after executions, equal
iteration counts, and the median of at least five samples. Each baseline is
frozen before its corresponding implementation changes. Original cross-project
baselines were captured at `f30f0b0`; detached-session and startup-specific
baselines were frozen after rebasing to `c7408b2`, and directional Vim word
navigation baselines were frozen after rebasing to `d92777c`.

| Runtime path | Median improvement |
| --- | ---: |
| Husk unchanged configuration refresh | 99.99% |
| Repeated themed syntax highlighting | 99.61% |
| Agent streamed transcript updates | 99.18% |
| Shared modal editor word operators | 99.17% |
| Long-line Vim forward word motion | 99.05% |
| Plugin preference persistence | 99.38% |
| Husk completion ranking | 99.42% |
| Structured panel row selection | 99.91% |
| Husk unchanged document updates | 98.44% |
| Viewport cursor snapshot updates | 98.58% |
| Gutter namespace updates | 97.94% |
| Decoration namespace updates | 97.48% |
| Long transcript cursor lookup | 97.35% |
| Shared modal editor backward word motion | 87.58% |
| Shared modal editor forward word motion | 87.01% |
| Printable ASCII frame rendering | 85.25% |
| Long-line Vim word-end motion | 84.79% |
| Idle plugin timer polling | 84.29% |
| Long-line Vim backward word motion | 80.33% |
| Detached incremental frame serialization | 79.06% |
| Shared Vim sentence navigation | 74.63% |
| Shared Vim paragraph navigation | 72.60% |
| Undo history capacity pruning | 72.42% |
| LSP completion filtering | 67.58% |
| Structured picker ranking | 64.81% |
| In-buffer search navigation | 55.98% |
| Bundled plugin startup | 51.22% |

Real detached-terminal coverage separately exercised editing, 32 KiB of Unicode
paste, repeated resizes, reattachment, and owner shutdown. Detached-frame median
serialization fell from 107 microseconds to below the trace timer's
one-microsecond resolution, and p95 fell from 118 to 73 microseconds.
Full-frame resize deltas remain intentionally more expensive.

Five alternating real typing runs showed 6.04% faster median input events,
7.44% faster median full-frame rendering, 9.27% faster process-to-first-paint
time, 35.07% faster interactive startup, and 45.09% faster bundled plugin
startup. Full-frame p95 was 1.63% slower in that noisy sample. These end-to-end
paths remain useful regression checks, but do not meet the 50% per-area
objective.

## Remaining gaps

- Owned Husk JSON conversion improves by approximately 42%. Moving strings and
  inserting sorted object fields directly avoids duplicate string clones and
  temporary sort buffers, but the runtime object representation still allocates.
- Broad process startup, full-frame typing, crash recovery, Git integration,
  Neo-tree memory, broader Vim editing, platform-specific paths,
  and several other areas above still require dedicated before/after fixtures
  before claiming a 50% improvement.
- An eager Neo-tree row index was intentionally rejected: real 2,048- and
  8,192-entry terminal runs showed greater memory use and slightly slower open
  times despite a favorable isolated lookup benchmark.

## Reproducing measurements

```shell
CARGO_TARGET_DIR=/Users/felipe.coury/code/red/target \
  cargo build --locked --release --example performance_hotspots

python3 scripts/compare_performance_hotspots.py \
  --before /path/to/frozen-baseline \
  --after /Users/felipe.coury/code/red/target/release/examples/performance_hotspots \
  --samples 7 \
  --scenarios picker preferences detached \
  --minimum-improvement 50

CARGO_TARGET_DIR=/Users/felipe.coury/code/red/target \
  cargo clippy --all-targets --all-features -- -D warnings
```

Use a baseline that actually contains the requested scenario. A later frozen
binary may already contain an earlier optimization and therefore cannot measure
that earlier change. Plugin startup uses a separately pinned `c7408b2` baseline
and passed the minimum-improvement gate over 21 alternating samples.
