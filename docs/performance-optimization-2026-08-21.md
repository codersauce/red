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
| Neo-tree repeated row selection | 99.98% |
| Crash-recovery buffer restoration | 99.77% |
| Repeated themed syntax highlighting | 99.61% |
| Embedded text-area ASCII typing | 99.29% |
| Agent streamed transcript updates | 99.18% |
| Shared modal editor word operators | 99.17% |
| Long-line Vim forward word motion | 99.05% |
| Plugin preference persistence | 99.38% |
| Husk completion ranking | 99.42% |
| Structured panel row selection | 99.91% |
| Inline-assistance answer streaming | 98.93% |
| Multi-file process startup loading | 98.92% |
| Husk unchanged document updates | 98.44% |
| Viewport cursor snapshot updates | 98.58% |
| Gutter namespace updates | 97.94% |
| Decoration namespace updates | 97.48% |
| Long transcript cursor lookup | 97.35% |
| Git workspace row navigation | 96.35% |
| Shared ASCII grapheme counting | 94.94% |
| LSP absolute-document routing | 94.68% |
| Workspace inline file discovery | 92.65% |
| Embedded text-area document loading | 91.26% |
| Shared modal editor backward word motion | 87.58% |
| Shared modal editor forward word motion | 87.01% |
| Printable ASCII frame rendering | 85.25% |
| Startup user-configuration loading | 84.04% |
| Long-line Vim word-end motion | 84.79% |
| Idle plugin timer polling | 84.29% |
| Default editor status-line rendering | 83.45% |
| Workspace inline content search | 82.60% |
| Long-line Vim backward word motion | 80.33% |
| Theme hexadecimal color parsing | 79.71% |
| Detached incremental frame serialization | 79.06% |
| Shared Vim sentence navigation | 74.63% |
| LSP incremental large-document changes | 74.61% |
| Shared Vim paragraph navigation | 72.60% |
| Undo history capacity pruning | 72.42% |
| LSP completion filtering | 67.58% |
| Structured picker ranking | 64.81% |
| Git workspace status directory indexing | 60.28% |
| Git repository discovery and branch refresh | 58.73% |
| Plugin cursor-event delivery | 56.35% |
| In-buffer search navigation | 55.98% |
| Owned Husk JSON boundary conversion | 54.49% |
| Bundled theme startup loading | 53.81% |
| Complete editor frame composition | 52.88% |
| Bundled plugin startup | 51.22% |

Complete frame composition was measured across 160 production `Editor::render`
calls at 160 columns by 48 rows. Eleven alternating release-build samples
reduced the median from 28,808 to 13,575 microseconds while retaining window
layout, gutters, source painting, syntax-style boundaries, chrome, overlays,
cursor composition, exact Unicode-aware frame differencing, and final flush.

Git repository discovery was measured over 512 expired-cache refreshes through
16 nested directories. Eleven alternating samples reduced the median from 51,601
to 21,297 microseconds while preserving nested repositories, linked worktrees,
detached HEADs, symlink retargeting, and renamed physical directories.

Git workspace status indexing was measured over 32 production directory-index
builds for 2,048 changed files across nested crate directories. Eleven alternating
samples reduced the median from 54,699 to 21,726 microseconds while preserving
conflict precedence, ignored-directory boundaries, tracked children, Windows
path separators, filesystem-root repositories, and out-of-repository filtering.

Multi-file startup uses the same production loader as the executable. Across
four startup passes over 128 distinct source files, seven alternating samples
reduced the median from 1,245,405 to 13,426 microseconds while preserving first
argument order, missing-file behavior, relative aliases, symlinks, and hard links.

Incremental LSP synchronization was measured across 256 Unicode edits in a
4,096-line source document. Eleven alternating samples reduced the median from
57,096 to 14,494 microseconds while retaining minimal ranges, UTF-16 positions,
multiline deletion coordinates, combining marks, and the existing CRLF fallback.

Embedded agent-composer and panel editing was measured over 256 ASCII insertions
into a 32 KiB production `TextArea`. Eleven alternating samples reduced the median
from 132,870 to 950 microseconds while preserving multiline cursor snapshots,
undo/redo, byte limits, newline normalization, and Unicode grapheme transitions.

Startup configuration loading was measured across 24 production user-file loads
containing valid editor, search, completion, formatting, signature-help,
key-hint, and clipboard settings. Eleven alternating samples reduced the median
from 93,056 to 14,854 microseconds while preserving invalid-field diagnostics,
unknown-field recovery, strict overrides, plugin quarantine, agent boundaries,
and language-server capability validation.

Bundled-theme loading was measured across 256 complete parses of the embedded
default theme. Eleven alternating samples reduced the median from 16,878 to
7,796 microseconds while retaining VS Code line and block comments, literal
comment markers inside JSON strings, scoped token styles, and workbench-color
fallbacks. The underlying hexadecimal color parser separately improved from
488 to 99 microseconds over 16,384 six- and eight-digit colors while preserving
shorthand, named colors, transparency, alpha channels, and invalid-input errors.

Shared ASCII grapheme counting was measured over 1,024 large multiline source
strings through the same helper used by editor navigation, layout, prompts, and
composers. Eleven alternating samples reduced the median from 242,610 to 12,283
microseconds while retaining CRLF grapheme pairing, combining marks, family
emoji, regional-indicator flags, and mixed Unicode. Opening 128 real embedded
draft documents independently improved from 27,405 to 2,394 microseconds while
preserving normalized byte caps, cursor positions, and exact document contents.

Real detached-terminal coverage separately exercised editing, 32 KiB of Unicode
paste, repeated resizes, reattachment, and owner shutdown. Detached-frame median
serialization fell from 107 microseconds to below the trace timer's
one-microsecond resolution, and p95 fell from 118 to 73 microseconds.
Full-frame resize deltas remain intentionally more expensive.

Real Neo-tree PTY runs confirmed every entry remained reachable at both 2,048
and 8,192 files. In the single 8,192-entry before/after run, opening fell from
75.78 to 42.32 milliseconds, navigation p95 fell from 4,661 to 627 microseconds,
and resident-memory growth fell from 41,120 to 39,616 KiB. The optimization
retains one cached row position per tree rather than allocating a per-row index.

A real Git-dashboard PTY run with 120 changed files confirmed the optimized
workspace path also improves end-to-end behavior: median file-list input fell
from 516 to 368 microseconds, full-frame rendering fell from 373 to 264
microseconds, and diff navigation fell from 372 to 319 microseconds. File-list
churn spawned one Git process and core-owned diff navigation spawned none.

A real typing PTY run separately confirmed that status-line and editor chrome
fell from 80 to 24 microseconds per frame, while complete frame rendering fell
from 315 to 263 microseconds. The editor captures its fixed working directory
once, avoids resolving absolute paths against the current directory, and skips
diagnostic URI conversion when no diagnostics exist.

Five alternating real typing runs showed 6.04% faster median input events,
7.44% faster median full-frame rendering, 9.27% faster process-to-first-paint
time, 35.07% faster interactive startup, and 45.09% faster bundled plugin
startup. Full-frame p95 was 1.63% slower in that noisy sample. These end-to-end
paths remain useful regression checks, but do not meet the 50% per-area
objective.

## Remaining gaps

- Single-file process startup, real-terminal end-to-end typing, recovery snapshot writes,
  Git subprocess status refresh, broader Vim editing, platform-specific paths,
  and several other areas above still require dedicated before/after fixtures
  before claiming a 50% improvement.
- An eager Neo-tree row index was intentionally rejected because it increased
  memory and slowed opening; the retained single-position cache avoids both
  regressions in real 2,048- and 8,192-entry terminal runs.
- A SHA-256-verified recovery-generation cache was also rejected: although it
  preserved all corruption and no-follow safeguards, durable snapshot writes
  became 3.93% slower in the 24-buffer, 48-undo-node fixture.

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
