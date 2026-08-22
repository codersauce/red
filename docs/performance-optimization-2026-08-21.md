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
| Plugin/window LSP cursor snapshot coordinates | 99.89% |
| Shared Vim keyword word text objects | 99.89% |
| Shared Vim ASCII change-word operators | 99.88% |
| Shared Vim ASCII delete-word operators | 99.87% |
| Editor next-word search cursor adjustment | 99.87% |
| Shared Vim counted change-word operators | 99.85% |
| Shared Vim counted delete-word operators | 99.85% |
| Shared Vim whitespace-delimited WORD objects | 99.80% |
| Crash-recovery buffer restoration | 99.77% |
| Real-terminal incremental search query resolution | 99.74% |
| Repeated themed syntax highlighting | 99.61% |
| Git status requests outside repositories | 99.58% |
| Shared display-column-to-grapheme conversion | 99.46% |
| Shared grapheme-to-display-column conversion | 99.45% |
| Embedded text-area ASCII typing | 99.29% |
| Agent streamed transcript updates | 99.18% |
| Shared modal editor word operators | 99.17% |
| Long-line Vim forward word motion | 99.05% |
| Plugin preference persistence | 99.38% |
| Husk completion ranking | 99.42% |
| Structured panel row selection | 99.91% |
| Inline-assistance answer streaming | 98.93% |
| Shared Vim nested delimiter text objects | 98.93% |
| Multi-file process startup loading | 98.92% |
| YAML line-comment punctuation and Unicode highlighting | 98.78% |
| YAML string punctuation and Unicode highlighting | 98.73% |
| Husk unchanged document updates | 98.44% |
| Viewport cursor snapshot updates | 98.58% |
| Embedded Vim line-boundary motions | 98.48% |
| PowerShell line-comment punctuation and Unicode highlighting | 98.37% |
| Editor LSP request cursor coordinates | 98.29% |
| PowerShell string punctuation and Unicode highlighting | 98.21% |
| Shared Vim final-sentence operators | 98.20% |
| Real-terminal Unicode and CRLF visual-block insertion | 98.01% |
| Real-terminal Unicode visual-block insertion | 97.99% |
| Real-terminal visual-block insertion | 97.97% |
| Gutter namespace updates | 97.94% |
| Shared Vim around final-sentence text objects | 97.88% |
| Embedded Vim word motions | 97.88% |
| Shared Vim inner final-sentence text objects | 97.87% |
| Lua line-comment punctuation and Unicode highlighting | 97.83% |
| Real-terminal CRLF visual-block insertion | 97.82% |
| Markdown H2 heading punctuation and Unicode highlighting | 97.81% |
| Bash string punctuation and Unicode highlighting | 97.80% |
| Fish line-comment punctuation and Unicode highlighting | 97.79% |
| Markdown CRLF heading punctuation and Unicode highlighting | 97.79% |
| Real-terminal incremental-LSP visual-block insertion | 97.73% |
| Markdown H3 heading punctuation and Unicode highlighting | 97.73% |
| Markdown H1 heading punctuation and Unicode highlighting | 97.72% |
| Markdown Unicode heading punctuation and Unicode highlighting | 97.72% |
| Real-terminal full-document-LSP visual-block insertion | 97.71% |
| Real-terminal split-window visual-block insertion | 97.65% |
| Lua string punctuation and Unicode highlighting | 97.64% |
| Bash line-comment punctuation and Unicode highlighting | 97.55% |
| Decoration namespace updates | 97.48% |
| Rust string punctuation highlighting | 97.46% |
| Rust line-comment punctuation highlighting | 97.38% |
| Long transcript cursor lookup | 97.35% |
| Rust Unicode line-comment highlighting | 97.33% |
| Fish string punctuation and Unicode highlighting | 97.31% |
| Character-wrapped layout cursor lookup | 97.29% |
| Real-terminal bundled-plugin visual-block insertion | 97.26% |
| Rust CRLF line-comment highlighting | 97.21% |
| Rust CRLF string highlighting | 97.15% |
| Rust Unicode string highlighting | 97.13% |
| JSON string punctuation and Unicode highlighting | 96.97% |
| Markdown fenced-injection heading highlighting | 96.97% |
| Embedded Home/End document navigation | 96.88% |
| TypeScript line-comment punctuation and Unicode highlighting | 96.86% |
| TSX line-comment punctuation and Unicode highlighting | 96.85% |
| Editor cursor display-column positioning | 96.83% |
| JSX line-comment punctuation and Unicode highlighting | 96.80% |
| TypeScript string punctuation and Unicode highlighting | 96.77% |
| TSX string punctuation and Unicode highlighting | 96.69% |
| JSX string punctuation and Unicode highlighting | 96.64% |
| Word-wrapped layout cursor lookup | 96.61% |
| JavaScript line-comment punctuation and Unicode highlighting | 96.54% |
| TOML line-comment punctuation and Unicode highlighting | 96.51% |
| Git workspace row navigation | 96.35% |
| Editor logical line-length lookup | 96.33% |
| JavaScript string punctuation and Unicode highlighting | 96.28% |
| Editor final-cell boundary lookup | 96.24% |
| TOML string punctuation and Unicode highlighting | 95.93% |
| Real-terminal cursor-moved plugin delivery | 95.05% |
| Real-terminal file-picker query resolution | 95.00% |
| Shared ASCII grapheme counting | 94.94% |
| Real-repository Git subprocess status refresh | 94.93% |
| Real-terminal JSON string highlighting | 94.74% |
| ASCII LSP rename-symbol extraction | 94.70% |
| LSP absolute-document routing | 94.68% |
| Real-terminal redundant full-frame publication | 93.65% |
| Husk CRLF line-comment highlighting | 93.65% |
| Real-terminal edit-invalidated Rust highlighting | 93.42% |
| Shared display-column-to-scalar conversion | 93.33% |
| Husk line-comment highlighting | 93.26% |
| Unicode Vim line-end operators | 93.21% |
| Husk Unicode line-comment highlighting | 93.02% |
| Editor scalar-to-grapheme cursor conversion | 92.82% |
| Workspace inline file discovery | 92.65% |
| Editor backward word-end boundary operators | 92.63% |
| Husk string highlighting | 92.52% |
| Real-terminal PowerShell line-comment highlighting | 91.97% |
| Shared buffer final-line lookup | 91.46% |
| Real-terminal PowerShell string highlighting | 91.43% |
| Shared buffer navigable-line counting | 91.38% |
| Embedded redo cursor restoration | 91.33% |
| Embedded text-area document loading | 91.26% |
| Real-terminal incremental search input events | 90.89% |
| Sparse ASCII full-buffer regex searching | 90.87% |
| Real-terminal Fish line-comment highlighting | 90.72% |
| Real-terminal CRLF line-comment highlighting | 90.56% |
| Real-terminal line-comment highlighting | 90.48% |
| Real-terminal Bash line-comment highlighting | 90.48% |
| Real-terminal CRLF string highlighting | 90.37% |
| Embedded Vim nested delimiter matching | 90.03% |
| Real-terminal Bash string highlighting | 89.91% |
| ASCII Vim line-end operators | 89.83% |
| ASCII Unicode-scalar line boundaries | 89.78% |
| Real-terminal Unicode line-comment highlighting | 89.62% |
| Real-terminal string highlighting | 89.62% |
| Real-terminal TSX line-comment highlighting | 89.38% |
| Shared scalar-to-display-column conversion | 89.31% |
| Real-terminal YAML line-comment highlighting | 89.29% |
| Real-terminal Markdown CRLF heading highlighting | 89.29% |
| Real-terminal Lua line-comment highlighting | 89.23% |
| Unicode scalar line boundaries | 89.12% |
| Sparse Unicode full-buffer regex searching | 89.12% |
| Real-terminal Markdown H3 heading highlighting | 89.01% |
| Real-terminal Unicode string highlighting | 88.89% |
| Real-terminal Fish string highlighting | 88.89% |
| Shared Vim escaped-quote text objects | 88.86% |
| Shared Vim ASCII backward character search | 88.82% |
| Real-terminal YAML string highlighting | 88.78% |
| Real-terminal Markdown H1 heading highlighting | 88.76% |
| Real-terminal JSX line-comment highlighting | 88.44% |
| Real-terminal Markdown Unicode heading highlighting | 88.24% |
| Shared Vim long-line paragraph operators | 88.15% |
| Real-terminal TypeScript string highlighting | 88.05% |
| Real-terminal TypeScript line-comment highlighting | 87.97% |
| Real-terminal TSX string highlighting | 87.84% |
| Real-terminal Lua string highlighting | 87.79% |
| Real-terminal JSX string highlighting | 87.59% |
| Shared modal editor backward word motion | 87.58% |
| Real-terminal JavaScript string highlighting | 87.41% |
| Real-terminal JavaScript line-comment highlighting | 87.14% |
| Shared modal editor forward word motion | 87.01% |
| Real-terminal Markdown H2 heading highlighting | 86.96% |
| Printable ASCII frame rendering | 85.25% |
| Startup user-configuration loading | 84.04% |
| Embedded undo cursor restoration | 84.17% |
| Long-line Vim word-end motion | 84.79% |
| Shared Vim ASCII forward character search | 84.55% |
| Real-terminal incremental search chrome | 84.44% |
| Idle plugin timer polling | 84.29% |
| Default editor status-line rendering | 83.45% |
| Workspace inline content search | 82.60% |
| Real-terminal file-picker chrome | 82.50% |
| Unicode LSP rename-symbol extraction | 82.49% |
| Editor forward word-end boundary operators | 81.94% |
| Real-terminal editor chrome rendering | 81.18% |
| Real-terminal TOML line-comment highlighting | 81.18% |
| Real-terminal TOML string highlighting | 81.18% |
| Long-line Vim backward word motion | 80.33% |
| Theme hexadecimal color parsing | 79.71% |
| Detached incremental frame serialization | 79.06% |
| Real-terminal completion-aware edit frames | 78.19% |
| Shared Vim final sentence cursor boundary | 75.93% |
| Real-terminal typing action handling | 75.81% |
| Embedded forward Delete-key editing | 75.72% |
| Shared Vim final paragraph cursor boundary | 75.63% |
| Shared Vim Unicode forward character search | 75.52% |
| Real-terminal Husk line-comment highlighting | 75.51% |
| Shared Vim Unicode backward character search | 75.26% |
| Shared Vim Unicode around quoted text objects | 75.15% |
| Shared Vim ASCII inner quoted text objects | 75.00% |
| Real-terminal Husk Unicode line-comment highlighting | 75.00% |
| Embedded Ctrl-Backspace word deletion | 74.86% |
| Shared Vim ASCII around quoted text objects | 74.66% |
| Shared Vim sentence navigation | 74.63% |
| LSP incremental large-document changes | 74.61% |
| Real-terminal text-insertion events | 74.32% |
| Shared Vim Unicode inner quoted text objects | 74.07% |
| Real-terminal Husk CRLF line-comment highlighting | 73.33% |
| Real-terminal Husk string highlighting | 72.92% |
| Embedded Vim long-line end motions | 72.63% |
| Real-terminal Unicode counted character deletion | 72.58% |
| Shared Vim paragraph navigation | 72.60% |
| Undo history capacity pruning | 72.42% |
| ASCII automatic indentation columns | 71.81% |
| Unicode automatic indentation columns | 71.42% |
| Real-terminal Unicode and CRLF counted character deletion | 71.09% |
| Real-terminal YAML typing action handling | 71.04% |
| Real-terminal CRLF counted character deletion | 70.54% |
| Real-terminal full-document-LSP substitution | 70.47% |
| Real-terminal full-document-LSP counted character deletion | 70.18% |
| Real-terminal YAML text-insertion events | 69.78% |
| Real-terminal edited-window painting | 69.23% |
| Real-terminal counted character deletion | 68.43% |
| Real-terminal split-window counted character deletion | 67.94% |
| Git commit line-comment highlighting | 67.66% |
| LSP completion filtering | 67.58% |
| Real-terminal full-document-LSP macro playback | 66.77% |
| Git commit diff highlighting | 66.62% |
| Real-terminal split-window macro playback | 66.25% |
| Real-terminal CRLF macro playback | 66.13% |
| Real-terminal bundled-plugin counted character deletion | 65.77% |
| Git commit CRLF highlighting | 65.26% |
| Real-terminal split-window substitution | 64.83% |
| Vim first-nonblank line-start operators | 64.81% |
| Structured picker ranking | 64.81% |
| Real-terminal incremental-LSP counted character deletion | 64.61% |
| Real-terminal macro playback | 64.59% |
| Real-terminal Markdown typing action handling | 64.30% |
| Real-terminal incremental search window painting | 64.00% |
| Real-terminal large-document substitution | 63.93% |
| Shared Vim around paragraph text objects | 63.73% |
| Shared Vim paragraph boundary operators | 63.42% |
| Real-terminal CRLF substitution | 63.42% |
| Real-terminal incremental-LSP macro playback | 63.12% |
| Shared Vim inner paragraph text objects | 63.01% |
| Git commit Unicode highlighting | 62.50% |
| Real-terminal Markdown text-insertion events | 62.37% |
| Git commit branch highlighting | 61.81% |
| Real-terminal JSON string edit frames | 61.51% |
| Git commit subject highlighting | 61.47% |
| Real-terminal Unicode and CRLF substitution | 61.37% |
| Complete real-terminal interactive startup | 60.80% |
| Real-terminal bundled-plugin macro playback | 60.71% |
| Real-terminal whole-document substitution | 60.66% |
| Real-terminal bundled-plugin substitution | 60.34% |
| Git workspace status directory indexing | 60.28% |
| Real-terminal overlay and cursor composition | 60.00% |
| Real-terminal YAML line-comment edit frames | 60.00% |
| Real-terminal CRLF string edit frames | 59.66% |
| Real-terminal YAML string edit frames | 59.31% |
| Real-terminal Unicode substitution | 59.09% |
| Real-terminal CRLF line-comment edit frames | 58.74% |
| Git repository discovery and branch refresh | 58.73% |
| Git commit path highlighting | 58.73% |
| Real-terminal string edit frames | 58.42% |
| Real-terminal Unicode macro playback | 58.36% |
| Real-terminal line-comment edit frames | 58.19% |
| Real-terminal YAML completion-aware edit frames | 58.12% |
| Real-terminal Unicode and CRLF macro playback | 57.84% |
| Real-terminal Unicode line-comment edit frames | 57.34% |
| Real-terminal Markdown completion-aware edit frames | 56.44% |
| Real-terminal Unicode string edit frames | 56.35% |
| Plugin cursor-event delivery | 56.35% |
| Real-terminal file-picker dialog and preview composition | 56.35% |
| Complete real-terminal Markdown interactive startup | 56.22% |
| In-buffer search navigation | 55.98% |
| Real-terminal edit-invalidated YAML highlighting | 55.94% |
| Real-terminal file-picker window painting | 55.56% |
| Complete real-terminal file-picker frames | 55.42% |
| Real-terminal incremental-LSP substitution | 54.67% |
| Real-terminal PowerShell line-comment edit frames | 54.51% |
| Owned Husk JSON boundary conversion | 54.49% |
| Real-terminal TSX line-comment edit frames | 54.14% |
| Complete real-terminal YAML interactive startup | 54.08% |
| Bundled theme startup loading | 53.81% |
| Complete real-terminal file-picker input events | 53.41% |
| Complete real-terminal incremental search frames | 53.11% |
| Complete editor frame composition | 52.88% |
| Real-terminal PowerShell string edit frames | 52.72% |
| Real-terminal edit-invalidated Markdown highlighting | 52.65% |
| Real-terminal TypeScript string edit frames | 52.65% |
| Real-terminal TSX string edit frames | 52.65% |
| Real-terminal bundled plugin startup | 51.88% |
| Bundled plugin startup | 51.22% |
| Real-terminal JSX line-comment edit frames | 50.79% |
| Real-terminal TypeScript line-comment edit frames | 50.57% |
| Real-terminal Lua line-comment edit frames | 50.21% |
| Real-terminal JavaScript string edit frames | 50.00% |

Complete frame composition was measured across 160 production `Editor::render`
calls at 160 columns by 48 rows. Eleven alternating release-build samples
reduced the median from 28,808 to 13,575 microseconds while retaining window
layout, gutters, source painting, syntax-style boundaries, chrome, overlays,
cursor composition, exact Unicode-aware frame differencing, and final flush.

Git repository discovery was measured over 512 expired-cache refreshes through
16 nested directories. Eleven alternating samples reduced the median from 51,601
to 21,297 microseconds while preserving nested repositories, linked worktrees,
detached HEADs, symlink retargeting, and renamed physical directories.

Git status requests outside repositories were measured across 32 production
refreshes from nested workspace directories. Eleven alternating release-build
samples reduced the median from 172,090 to 731 microseconds by avoiding Git
subprocesses when no repository exists. Real repositories retain modified,
untracked, and ignored status entries, nested repository precedence, linked
worktrees, canonical roots, and retargeted directory symlinks.

Real-repository Git status requests were independently measured across 32
production refreshes containing modified, untracked, and ignored files.
Eleven alternating release-build samples reduced the median from 224,319 to
11,371 microseconds. A 250-millisecond burst cache retains at most 16
repository listings and fingerprints at most 512 working-tree entries before
reusing any subprocess result. Stable paths, file types, permissions, lengths,
nanosecond modification/change times, Git index, HEAD, current ref, packed
refs, local configuration, exclude rules, and linked-worktree common
directories must all remain unchanged. Oversized worktrees, inaccessible
metadata, changed files or indexes, expired entries, nested repository changes,
and retargeted symlinks retain the existing fresh Git subprocess. A real
120-file Git dashboard still starts one Git process during file-list churn
and none while navigating the diff pane.

Git workspace status indexing was measured over 32 production directory-index
builds for 2,048 changed files across nested crate directories. Eleven alternating
samples reduced the median from 54,699 to 21,726 microseconds while preserving
conflict precedence, ignored-directory boundaries, tracked children, Windows
path separators, filesystem-root repositories, and out-of-repository filtering.

Shared buffer line boundaries were independently measured across 4,096
production final-line lookups and navigable-line counts on a 53 KiB
unterminated source line. Eleven alternating release-build samples reduced
final-line medians from 12,954 to 1,106 microseconds and line-count medians from
12,798 to 1,103 microseconds while preserving empty named and unnamed buffers,
trailing LF/CRLF separators, repeated blank lines, and Unicode scalar contents.

Sparse full-buffer search was independently measured across 128 production
regex searches for widely separated matches within 2,049-line ASCII and
Unicode/CRLF documents. Eleven alternating release-build samples reduced ASCII
medians from 10,341 to 944 microseconds and Unicode medians from 8,884 to 967
microseconds while retaining dense-match behavior, multiline matches, Unicode
line separators, scalar coordinates, and zero-width match filtering.

Wrapped cursor hit-testing was measured over 2,048 production lookups across
more than 256 visual rows. Eleven alternating release-build samples reduced
character-wrapped lookup medians from 13,148 to 356 microseconds and word-wrapped
lookup medians from 12,941 to 439 microseconds. Both paths retain existing
duplicate-position and equidistant-column tie-breaking, omitted soft-wrap
separators, Unicode graphemes, wide characters, tabs, hard breaks, blank rows,
and zero-width view behavior.

Embedded Vim motions were measured over 2,048 real Normal-mode input events
against a 512-line production `TextArea`. Eleven alternating release-build
samples reduced `w`/`b` word-motion medians from 186,095 to 3,950 microseconds
and `$`/`0` line-boundary medians from 116,150 to 1,760 microseconds. Indexed
ASCII cursor conversion retains empty and trailing lines, normalized CRLF,
bounded positions, combining marks, emoji, and the unchanged Unicode fallback.

Embedded Vim delimiter matching was measured across 1,024 real `%` input events
against a document containing nested parentheses, brackets, and braces. Eleven
alternating samples reduced the median from 29,496 to 2,942 microseconds while
preserving reverse searches, unmatched delimiters, multiline nesting, combining
marks, emoji, Unicode cursor positions, and the editor's existing mode behavior.

Embedded draft deletion was measured over 256 real editing events against a
36 KiB ASCII document. Eleven alternating release-build samples reduced forward
Delete-key medians from 4,177 to 1,014 microseconds and Ctrl-Backspace word
deletion medians from 4,794 to 1,205 microseconds. Rope-indexed ASCII paths
preserve whitespace-delimited words, empty buffers, tabs, multiline drafts,
combining marks, emoji, CJK text, cursor boundaries, and undo restoration.

Embedded Home/End navigation was measured over 2,048 real editing events across
a 1,024-line draft. Eleven alternating release-build samples reduced the median
from 8,938 to 279 microseconds while preserving normalized CRLF, empty drafts,
multiline boundaries, combining marks, family emoji, regional-indicator flags,
out-of-range cursor clamping, and the original Unicode grapheme-counting path.

Shared Vim operator boundaries were measured over 2,048 production paragraph
and sentence range resolutions ending at a large document's final line. Eleven
alternating release-build samples reduced paragraph medians from 7,014 to 2,566
microseconds and final-sentence medians from 158,573 to 2,847 microseconds while
preserving punctuation boundaries, leading whitespace, linewise register shape,
Unicode scalars, and empty or unterminated final sentences.

Long-line Vim editing was measured over 2,048 real `$`/`0` input events and
2,048 shared paragraph operator resolutions against 40 KiB source lines. Eleven
alternating release-build samples reduced line-end motions from 5,989 to 1,639
microseconds and paragraph operators from 16,873 to 1,999 microseconds while
preserving Unicode scalar counts, CRLF, tabs, empty lines, first-nonblank
columns, and linewise versus characterwise selections.

Embedded undo and redo were independently measured across 128 production
restorations near the end of 768-line drafts. Eleven alternating release-build
samples reduced undo medians from 2,628 to 416 microseconds and redo medians from
2,513 to 218 microseconds while preserving empty and trailing lines, invalid
snapshot coordinates, tabs, combining marks, emoji, Unicode cursor positions,
and exact undo/redo history restoration.

Shared Vim text objects were independently measured across 2,048 nested
delimiter selections and 2,048 escaped-quote selections on large source lines.
Eleven alternating release-build samples reduced delimiter medians from 100,937
to 1,077 microseconds and quote medians from 60,761 to 6,771 microseconds while
preserving innermost pairs, unmatched delimiters, odd/even backslash escaping,
Unicode scalar positions, and inner versus around selection boundaries.

Shared Vim keyword and whitespace-delimited WORD objects were independently
measured across 2,048 production selections on long ASCII source lines. Eleven
alternating release-build samples reduced keyword-object medians from 1,698,623
to 1,816 microseconds and WORD-object medians from 1,181,118 to 2,416
microseconds while preserving punctuation groups, leading/trailing whitespace,
tabs, out-of-range cursors, inner/around semantics, and Unicode grapheme rules.

Shared Vim change-word and delete-word operators were independently measured
across 512 production selections before a long untouched ASCII document tail.
Eleven alternating release-build samples reduced change medians from 134,874 to
167 microseconds and delete medians from 134,737 to 172 microseconds while
preserving keyword versus WORD groups, punctuation, leading and trailing
whitespace, CRLF, counts, out-of-range cursors, and Unicode grapheme fallback.

Counted Vim change-word and delete-word operators were independently measured
across 512 four-word production selections before a long untouched document
tail. Eleven alternating release-build samples reduced counted change medians
from 132,566 to 196 microseconds and counted delete medians from 132,823 to 201
microseconds while preserving multiline traversal, CRLF, punctuation, WORD
groups, whitespace retention, exhaustion, and Unicode grapheme fallback.

Editor word-end boundary operators were independently measured across 512 real
backward first-word and forward final-word selections on 20 KiB documents.
Eleven alternating release-build samples reduced backward medians from 8,245
to 608 microseconds and forward medians from 3,849 to 695 microseconds while
preserving pending-operator ranges, ASCII cursor bounds, CRLF, keyword versus
WORD grouping, Unicode scalar positions, and empty or whitespace boundaries.

Editor cursor conversions were independently measured across 2,048 production
scalar-to-grapheme conversions and next-word whitespace adjustments on 20 KiB
ASCII source lines. Eleven alternating release-build samples reduced reverse
cursor medians from 5,305 to 381 microseconds and next-word adjustment medians
from 302,008 to 407 microseconds while preserving missing lines, cursor clamps,
CRLF, tabs, combining marks, emoji, and unchanged Unicode grapheme behavior.

Editor line-boundary primitives were independently measured across 2,048
logical line-length and final-cell production lookups on 20 KiB ASCII source
lines. Eleven alternating release-build samples reduced line-length medians from
9,688 to 356 microseconds and final-cell medians from 9,611 to 361 microseconds
while preserving viewport offsets, missing lines, LF/CRLF endings, empty rows,
combining marks, emoji, CJK, and Unicode grapheme counts.

Editor cursor positioning was independently measured across 2,048 production
display-column, LSP-request, and plugin/window LSP-snapshot lookups on 20 KiB
ASCII source lines. Eleven alternating release-build samples reduced display
medians from 401,011 to 12,717 microseconds, request coordinates from 22,153
to 378 microseconds, and window-snapshot coordinates from 325,746 to 356
microseconds while preserving tabs, ASCII control characters, LF/CRLF endings,
missing lines and buffers, viewport offsets, cursor saturation, combining
marks, emoji, CJK, Unicode grapheme boundaries, and UTF-16 positions.

Editor Unicode-scalar boundaries and real Vim line-end operators were
independently measured across 2,048 ASCII and Unicode source-line lookups.
Eleven alternating release-build samples reduced ASCII scalar medians from
3,847 to 393 microseconds, Unicode scalar medians from 3,723 to 405
microseconds, ASCII Vim line-end ranges from 4,160 to 423 microseconds, and
Unicode line-end ranges from 6,124 to 416 microseconds. Across 512 real rename
symbol extractions, ASCII medians fell from 6,036 to 320 microseconds and
Unicode medians fell from 4,101 to 718 microseconds. The shared indexed paths
preserve LF/CRLF endings, missing lines, scalar versus grapheme boundaries,
cursor saturation, punctuation, underscores, combining marks, emoji, CJK,
Unicode symbols, and viewport offsets.

Shared terminal-coordinate helpers were independently measured across 2,048
forward and reverse Unicode-scalar and tab-aware grapheme conversions on 20 KiB
ASCII lines. Eleven alternating release-build samples reduced scalar-to-column
medians from 19,969 to 2,135 microseconds, column-to-scalar medians from 32,116
to 2,141 microseconds, grapheme-to-column medians from 388,290 to 2,130
microseconds, and column-to-grapheme medians from 394,493 to 2,131
microseconds. Exhaustive alignment checks preserve all 128 ASCII values,
zero-width control characters, tab stops, LF/CRLF, Unicode byte boundaries,
combining marks, CJK, emoji sequences, regional-indicator flags, and saturated
cursor positions.

Editor leading-whitespace paths were independently measured across 2,048 real
Vim first-nonblank operators and ASCII/Unicode automatic-indentation lookups on
20 KiB source lines. Eleven alternating release-build samples reduced Vim
operator medians from 2,796 to 984 microseconds, ASCII indentation from 2,547
to 718 microseconds, and Unicode indentation from 2,474 to 707 microseconds.
Indexed prefix reads preserve tabs, Unicode whitespace, all-whitespace rows,
LF/CRLF endings, empty and missing lines, cursor saturation, Vim scalar ranges,
and viewport offsets.

Shared Vim character-search motions were independently measured across 2,048
counted forward and backward searches on ASCII and Unicode source lines. Eleven
alternating release-build samples reduced ASCII forward medians from 3,858 to
596 microseconds and backward medians from 5,312 to 594 microseconds; Unicode
forward medians fell from 2,394 to 586 microseconds and backward medians from
2,421 to 599 microseconds. Indexed bidirectional Rope traversal preserves
repeated-target counts, excluded cursor characters, scalar boundaries, missing
lines, cursor saturation, LF/CRLF endings, combining marks, CJK, and emoji.

Shared Vim end-of-document motions were independently measured across 2,048
paragraph and sentence searches on 20 KiB final source lines. Eleven
alternating release-build samples reduced final paragraph cursor medians from
4,513 to 1,100 microseconds and final sentence cursor medians from 4,479 to
1,078 microseconds while preserving CRLF, empty buffers, Unicode combining
marks, family emoji, regional-indicator flags, CJK, and final grapheme
boundaries.

Shared Vim quoted-text objects were independently measured across 2,048 inner
and around selections on ASCII and Unicode source lines. Eleven alternating
release-build samples reduced ASCII inner medians from 2,464 to 616
microseconds, ASCII around medians from 2,443 to 619 microseconds, Unicode
inner medians from 2,206 to 572 microseconds, and Unicode around medians from
2,270 to 564 microseconds. A separate nine-sample existing long-prefix control
improved from 6,737 to 1,996 microseconds, confirming chunk-level quote
searches retain fast distant-target lookup. Regression coverage preserves quote
pairing, odd/even escape parity across Rope chunks, Unicode scalar positions,
CRLF, cursor inclusion, and inner versus around selection scopes.

Shared Vim paragraph objects were independently measured across 128 inner and
128 around selections spanning 768-line paragraphs. Eleven alternating
release-build samples reduced inner medians from 16,028 to 5,928 microseconds
and around medians from 16,338 to 5,925 microseconds while preserving
whitespace-only groups, CRLF, Unicode whitespace, empty/trailing lines,
out-of-range cursors, and exact paragraph selection scopes.

Shared Vim final-sentence objects were independently measured across 2,048
inner and around production selections following real paragraph boundaries.
Eleven alternating release-build samples reduced inner medians from 102,767 to
2,194 microseconds and around medians from 103,380 to 2,195 microseconds while
preserving punctuation, counts, leading and trailing whitespace, Unicode scalar
positions, and inner versus around selection scopes.

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

Eleven alternating real-terminal typing runs exercised 60 verified ASCII and
Unicode text insertions at four-millisecond spacing through the production
executable on a 50-row, 120-column PTY. The harness disables first-run release
notes and requires one observed insertion per requested key so modal dialogs
cannot silently intercept the workload. Median user-visible typing events fell
from 1,612 to 414 microseconds, and complete action handling fell from 1,575 to
381 microseconds. Cursor-moved plugin delivery independently fell from 727 to
36 microseconds because indentation guides reuse an exact visible-geometry
signature across ordinary same-line text edits and horizontal motion.

Automatic completion previously forced a complete editor render for each
subsequent text insertion. Completion-aware surface reuse reduced the median
production edit frame from 587 to 128 microseconds and the number of full
frames in each 60-insertion session from 63 to four. The replacement frame
includes every visible editor window, status line, completion popup, cursor,
and terminal diff; even compared against only the previous window-paint phase,
it improved from 416 to 128 microseconds. Docked panes, modal dialogs, visible
overlays, signature help, and other unsafe surfaces retain the complete-frame
fallback.

Edit-invalidated Rust highlighting fell from 304 to 20 microseconds. Bounded
per-language syntax trees support incremental parsing after general source
edits. Interior edits to ordinary lowercase bundled-Rust identifiers additionally
reuse their existing Tree-sitter captures, shifting every UTF-8 byte span and
cached tree by the exact edit. The shortcut excludes token boundaries, reserved
keywords, uppercase-sensitive names, non-identifier Unicode, custom queries,
and source or span cache-limit violations; Unicode edits, comments, YAML
context, Markdown language injections, and fresh-parser parity remain covered.

Seven alternating release-build samples per Rust token scenario applied 128
non-identifier edits to bounded 96-line documents. Ordinary line-comment
punctuation fell from 34,480 to 904 microseconds, Unicode comment edits from
34,433 to 920, and CRLF comment edits from 34,244 to 955. String punctuation
fell from 34,508 to 878 microseconds, Unicode string edits from 34,417 to 988,
and CRLF string edits from 34,453 to 981. Existing captures and cached syntax
trees are shifted only when the edit remains strictly inside an ordinary line
comment or plain string-content node. Documentation prefixes, newline
insertion, quote and escape characters, control bytes, raw or block comments,
custom queries, token boundaries, and oversized caches retain the complete
parser path. Fresh-parser parity covers punctuation, Unicode, CRLF, insertions,
deletions, and all rejected syntax changes.

Another seven alternating real-terminal sessions per source variant inserted
32 alternating punctuation and Unicode characters inside Rust comments and
strings. Ordinary comment highlighting fell from 189 to 18 microseconds,
Unicode-source comments from 183 to 19, and CRLF comments from 180 to 17.
Ordinary string highlighting fell from 183 to 19 microseconds, Unicode-source
strings from 189 to 21, and CRLF strings from 187 to 18. Complete ordinary
comment edit frames fell from 299 to 125 microseconds, CRLF comment frames
from 286 to 118, ordinary string frames from 291 to 121, and CRLF string
frames from 295 to 119. At this stage, Unicode-source comment and string frames
improved only 46.48% and 47.22%, respectively.

Seven alternating release-build samples across 96-line JavaScript, JSX,
TypeScript, TSX, and JSON fixtures applied 128 punctuation and Unicode edits
per source token. JavaScript comment and string medians fell from 26,018 to
899 and 25,751 to 957 microseconds; JSX medians fell from 26,942 to 861 and
27,059 to 910; TypeScript medians fell from 30,373 to 954 and 30,918 to 1,000;
TSX medians fell from 30,479 to 960 and 31,123 to 1,030; and JSON string
medians fell from 11,204 to 339. Capture reuse requires the actual bundled
grammar, its exact complete bundled highlight-query list, bounded source and
style caches, and an edit strictly inside a JavaScript-family `//` comment,
ordinary quoted string, or JSON key/value string. Block comments, template
literals, quote or escape insertion, JavaScript Unicode line separators,
custom grammars or queries, token boundaries, and oversized source retain the
complete parser path. Fresh-parser parity covers punctuation, Unicode, CRLF,
JSON keys and values, and all rejected syntax changes.

Five further alternating real-terminal sessions per JavaScript-family comment
or string and JSON string exercised 32 punctuation and Unicode edits through
production editor releases. JavaScript highlighting fell from 140 to 18 and
143 to 18 microseconds; JSX fell from 147 to 17 and 145 to 18; TypeScript fell
from 158 to 19 and 159 to 19; TSX fell from 160 to 17 and 148 to 18; and JSON
fell from 209 to 11. Complete JSON string edit frames fell from 317 to 122
microseconds; TSX comment and string frames from 266 to 122 and 245 to 116;
TypeScript comment and string frames from 263 to 130 and 264 to 125; JSX
comment frames from 254 to 125; and JavaScript string frames from 248 to 124.
Complete input events and action handling, JavaScript comment frames, and JSX
string frames remain below the target and are excluded from verified results.

Seven alternating release-build samples across 96-line TOML, YAML, Bash, Fish,
PowerShell, and Lua fixtures applied 128 punctuation and Unicode edits per
comment and quoted string. TOML comment and string medians fell from 13,836
to 483 and 14,122 to 575 microseconds; YAML fell from 38,315 to 467 and 38,310
to 488; Bash fell from 20,650 to 506 and 20,756 to 457; Fish fell from 18,725
to 414 and 18,890 to 508; PowerShell fell from 26,718 to 435 and 27,117 to
486; and Lua fell from 23,324 to 507 and 23,793 to 561. Exact bundled grammars
and their complete bundled query sets remain mandatory. Hash comments, Lua
line comments, and ordinary quoted string nodes each validate their own
grammar boundaries. Shell variable interpolation and command substitutions,
PowerShell backticks, TOML multiline strings, Lua block comments, newline and
escape insertion, custom grammars or queries, and oversized sources retain
normal parsing. Fresh-parser parity also preserves YAML document prefixes,
Unicode, CRLF, and exact syntax-style ranges.

Five alternating real-terminal sessions per configuration or shell token
exercised 32 punctuation and Unicode edits using extension-aware `#` and Lua
`--` comment placement. TOML comment and string highlighting each fell from
85 to 16 microseconds; YAML fell from 196 to 21 and 196 to 22; Bash fell from
105 to 10 and 109 to 11; Fish fell from 97 to nine and 99 to 11; PowerShell
fell from 137 to 11 and 140 to 12; and Lua fell from 130 to 14 and 131 to 16.
Complete YAML comment and string edit frames fell from 290 to 116 and 290 to
118 microseconds; PowerShell frames fell from 233 to 106 and 239 to 113; and
Lua comment frames fell from 233 to 116. TOML, Bash, Fish, and Lua string
frames plus complete input events remain below the target and are excluded.

Nine alternating release-build samples across 96-line Husk and Git commit
fixtures applied 128 punctuation and Unicode edits to each specialized syntax
path. Husk comment and string highlighting fell from 7,655 to 516 and 7,554
to 565 microseconds; Unicode comments fell from 8,080 to 564, and CRLF
comments from 7,641 to 485. Git commit comment, branch, path, diff, and
ordinary subject highlighting fell from 569 to 184, 720 to 275, 756 to 312,
707 to 236, and 737 to 284 microseconds; Unicode comment highlighting fell
from 672 to 252, and CRLF comment highlighting from 567 to 197. Both shortcuts
require their exact specialized language definitions without custom grammars
or highlight queries, existing bounded source and capture caches, and edits
that cannot cross line boundaries. Husk preserves an existing ordinary `//`
comment or plain quoted-string span only when its delimiters, active theme
style, Unicode byte ranges, and unchanged lexer boundaries remain valid;
newline, control, quote, and escape changes retain complete lexing. Git
commit highlighting independently recomputes the changed line and shifts all
unaffected capture ranges, preserving ordinary unstyled subjects and semantic
transitions involving headings, branch names, paths, references, and added or
deleted diff lines.

Five alternating real-terminal sessions per Husk or Git commit variant applied
32 punctuation and Unicode edits to 240-line documents. Husk comment, string,
Unicode-comment, and CRLF-comment highlighting respectively fell from 49 to
12, 48 to 13, 48 to 12, and 45 to 12 microseconds. Actual Git commit viewport
highlighting already took only eight to 11 microseconds before optimization;
its real-terminal improvements, complete Husk edit frames, and all complete
input events remain below the 50% target and are excluded.

Seven alternating release-build samples across 96-line Markdown heading
fixtures applied 128 punctuation and Unicode edits per heading. H1, H2, and
H3 medians fell from 16,204 to 370, 16,463 to 360, and 16,258 to 369
microseconds; Unicode headings fell from 16,307 to 371 and CRLF headings
from 16,349 to 362. Headings sharing one document with both Rust and
JavaScript fenced-code injections fell from 18,339 to 555 microseconds while
preserving both injected capture sets after every edit. The shortcut requires
the exact bundled Markdown grammar, highlight query, and injection query, an
edit strictly inside an ATX heading inline node, and existing bounded source
and capture caches. Heading markers, list/link/quote syntax, backticks,
escapes, newline insertion, custom injection queries, and edits inside fenced
code retain the complete parser and nested-language path.

Five alternating real-terminal sessions per Markdown heading variant applied
32 punctuation and Unicode edits using the heading-aware typing driver. H1,
H2, and H3 highlighting fell from 89 to 10, 92 to 12, and 91 to 10
microseconds; Unicode heading highlighting fell from 85 to 10 and CRLF
heading highlighting from 84 to nine. Complete heading frames improved only
43.75% to 46.59%, and input events plus action handling also remain below the
target; all are excluded from the verified results.

Nine further alternating real-terminal sessions per Unicode-source token
reduced complete comment edit frames from 354 to 151 microseconds and string
edit frames from 362 to 158. Mixed Unicode rows now coalesce their ordinary
ASCII runs within exact syntax-style and viewport boundaries, leaving each
combining accent, keycap sequence, ZWJ emoji, wide-character continuation,
tab stop, clipped segment, and wrapped line on its correct grapheme-aware
path. Complete input events and action handling remain below the target and
are intentionally excluded from the verified results.

Editor chrome independently fell from 85 to 16 microseconds, and bundled plugin
startup fell from 35,873 to 17,263 microseconds. Complete interactive startup
fell from 46,299 to 18,149 microseconds after the first visible language's
Tree-sitter query compilation moved onto a background worker during independent
plugin initialization. Query and injection compilation retain their exact
language-registry snapshot; the foreground installs captures with the current
theme and discards stale, failed, unsupported, or already-initialized results.
Real Rust, YAML, and Markdown sessions still open and accept verified Unicode
edits.

Nine additional alternating runs per language exercised 40 verified ASCII and
Unicode insertions into real YAML and Markdown files. Exact viewport contents
and UTF-8 line offsets now come from one pre-sized Rope traversal instead of
allocating a temporary string for every source line. YAML highlighting fell
from 345 to 152 microseconds, and Markdown highlighting fell from 452 to 214
microseconds, while preserving complete YAML document-prefix context and nested
Markdown language injections. YAML typing events fell from 1,797 to 543
microseconds, typing actions from 1,768 to 512, edit frames from 628 to 263,
and interactive startup from 39,845 to 18,297. Markdown typing events fell from
1,584 to 596 microseconds, typing actions from 1,552 to 554, and edit frames
from 714 to 311. A subsequent nine-run alternating comparison reduced complete
Markdown interactive startup from 42,463 to 18,591 microseconds by preparing a
bounded, deduplicated set of visible fenced-language grammars alongside outer
Markdown syntax and independent plugin initialization. Unsupported aliases,
oversized viewports, stale registries, failed queries, and repeated languages
retain the existing safe foreground fallback.

Nine alternating real-terminal search sessions exercised 12 incremental query
cycles apiece against frozen release executables. Query resolution fell from
1,950 to five microseconds, complete input events from 2,316 to 211, editor
chrome from 90 to 14, window painting from 150 to 54, and complete search
frames from 354 to 166 microseconds. A bounded history reuses prior regex
results only when stable buffer identity, content revision, effective case
sensitivity, and exact pattern match; pathologically dense result sets are not
retained. Viewport-wide byte bounds avoid unnecessary per-line checks, repeated
matches reuse their source line and visible layout segments, and printable
ASCII uses direct display columns. Highlight backgrounds paint contiguous
visible screen ranges without per-match layout reconstruction or point
allocations, and complete editor frames paint their command line only once.
Wrapping, horizontal clipping, tabs, control characters, Unicode, custom
selection styles, modal/workspace precedence, current-match precedence,
navigation direction, wraparound, and preview/cancel restoration remain
unchanged. Search-frame terminal diff and flush improved only 19.23%, from
52 to 42 microseconds, and remains below the target.

Nine alternating real-terminal file-picker sessions exercised 12 query-edit
cycles apiece against frozen release executables. Exact-query resolution fell
from 60 to three microseconds, complete picker input events from 586 to 273,
complete picker frames from 480 to 214, dialog and preview composition from
197 to 86, editor window painting from 117 to 52, and chrome from 80 to 14
microseconds. Locally filtered structured pickers retain a bounded history of
eight previous queries, excluding result sets larger than 16,384 items and
invalidating every prior ranking when authoritative items change. External
filters, custom nonincremental scorers, stable ordering, selection identity,
and asynchronous updates retain their existing behavior. Printable ASCII
results and preview rows reuse terminal-cell storage without temporary
strings; tabs, control characters, Unicode, highlighting, and clipped wide
graphemes retain their general fallbacks. Preview syntax captures are shared
only for an exact source window, preview identity, language, and active
theme, with bounded source and capture counts. Picker-frame terminal diff and
flush improved only 27.66%, from 47 to 34 microseconds, and remains below the
target.

Seven alternating real-terminal sessions per visual-block scenario replayed 128
characters across 16 rows in 2,000-line files. Ordinary completion fell from
95,037 to 1,926 microseconds, Unicode source from 108,210 to 2,180, CRLF
source from 86,503 to 1,886, and combined Unicode/CRLF source from 107,256 to
2,131. Bundled plugins fell from 83,885 to 2,297 microseconds, shared-buffer
split windows from 79,592 to 1,868, incremental LSP synchronization from
87,975 to 1,996, and full-document LSP synchronization from 87,462 to 2,004.
A bounded ASCII-keyword insertion of at most 256 characters now runs as one
canonical transactional replacement per secondary row instead of separately
dispatching every character. Unicode input, punctuation, whitespace, oversized
insertions, Python, indentation-sensitive words, wrapping comments, snippets,
completion dialogs, signature-help providers, tutorials, and learning sessions
retain their existing per-character path. Automatic completion state, plugin
event causes, primary cursor restoration, cancellation checkpoints, shared
window contents, exact saved Unicode/CRLF bytes, and one undo transaction all
remain intact. Every terminal run verified one completed render and one buffer
notification; both language-server modes additionally verified one document
notification and the exact synchronized source.

Seven alternating real-terminal sessions per substitution scenario replaced
every occurrence across 2,000 lines, with a separate 8,000-line fixture.
Ordinary substitution fell from 7,423 to 2,920 microseconds, Unicode from
7,933 to 3,245, CRLF from 8,001 to 2,927, and combined Unicode/CRLF from
8,563 to 3,308. Bundled plugins fell from 7,587 to 3,009 microseconds,
shared-buffer splits from 8,374 to 2,945, incremental LSP from 11,085 to
5,025, full-document LSP from 10,483 to 3,096, and the 8,000-line fixture
from 26,843 to 9,683. Line planning borrows contiguous Rope chunks, keeps
scalar-coordinate ranges, and skips capture expansion for literal replacements.
The shared transactional seam resolves each scalar range once; empty history,
annotation, and mark bookkeeping avoids intermediate work while preserving
real marks, jumps, and their fallback coordinates. Recognized substitute
commands publish one completed frame instead of three, while bundled callbacks
may legitimately publish one additional frame. Incremental LSP retains one
original snapshot and all 2,000 exact changes inside one notification; known
full-sync servers bypass unused incremental bookkeeping altogether. Regex
captures, escaped delimiters, confirmations, visual ranges, Unicode UTF-16,
CRLF, named marks, undo/redo, diagnostics, and external plugin barriers retain
their original behavior. Substitute-command classification stays outside the
recursive dispatcher so visual-block dot-repeat remains safe on a 2 MiB stack.

Seven alternating real-terminal sessions per replay scenario applied 128 macro
actions or counted character deletions inside 2,000-line fixtures. Ordinary
macro playback fell from 6,684 to 2,367 microseconds, Unicode from 9,174 to
3,820, CRLF from 6,611 to 2,239, and combined Unicode/CRLF from 9,119 to
3,845. Bundled plugins fell from 7,325 to 2,878 microseconds, shared-buffer
splits from 6,744 to 2,276, incremental LSP from 7,166 to 2,643, and
full-document LSP from 7,542 to 2,506. Ordinary counted deletion fell from
4,359 to 1,376 microseconds, Unicode from 5,849 to 1,604, CRLF from 4,342 to
1,279, and combined Unicode/CRLF from 5,687 to 1,644. Bundled plugins fell
from 4,324 to 1,480 microseconds, shared-buffer splits from 4,220 to 1,353,
incremental LSP from 4,642 to 1,643, and full-document LSP from 4,674 to
1,394. While a deferred replay remains on the first visible wrapped source
line, conservative break-indent capacity and one reserved wide-grapheme cell
per row prove that the cursor remains above the configured bottom scroll
margin without rebuilding the viewport. Macro layout misses fell from 129 to
two, and counted-deletion misses from 128 to one. Inline comments, predictions,
skipped columns, scrolled origins, long wrapped lines, and exhausted visible
capacity retain their original full-layout path. All runs preserved exact saved
Unicode/CRLF text, bundled callbacks, shared-window contents, fairness and
cancellation checkpoints, and one optimized buffer publication; both LSP
modes additionally retained one exact synchronized document notification.

Eleven alternating real-terminal runs reduced overlay and cursor composition
from five to two microseconds. Each frame resolves cursor geometry once for
overlay placement, its underlying terminal surface, synthetic cursor painting,
and committed frame state; idle overlays are positioned only after they gain
visible content. Cursor-avoiding overlays, modal dialogs, focused panels,
wide characters, and theme-dependent cursor contrast retain their existing
behavior. Terminal diff and flush improved only 24.39%, while
process-to-first-paint remains unresolved because executable warm-up and
filesystem effects make its samples unstable.

## Remaining gaps

- Single-file process startup, terminal diff and flush, remaining syntax-token
  classes and edits inside Markdown fenced-language injections, recovery snapshot
  writes, broader Vim editing, platform-specific paths, and several other areas
  above do not yet meet the 50% improvement target.
- An eager Neo-tree row index was intentionally rejected because it increased
  memory and slowed opening; the retained single-position cache avoids both
  regressions in real 2,048- and 8,192-entry terminal runs.
- A SHA-256-verified recovery-generation cache was also rejected: although it
  preserved all corruption and no-follow safeguards, durable snapshot writes
  became 3.93% slower in the 24-buffer, 48-undo-node fixture.
- An exact-byte verified recovery-generation cache preserved protected handles,
  corruption detection, owner isolation, invalid-snapshot rejection, and crash
  rotation but improved durable snapshot writes by only 11.76%; its added
  memory and implementation complexity were therefore rejected.
- Skipping unchanged terminal-cell assignments improved isolated ASCII repaint
  by only 8.10% and left JavaScript comment and JSX string frames below 50%;
  the extra per-cell branch was rejected after real-terminal validation.

## Reproducing measurements

```shell
cargo build --locked --release --example performance_hotspots

python3 scripts/compare_performance_hotspots.py \
  --before /path/to/frozen-baseline \
  --after target/release/examples/performance_hotspots \
  --samples 7 \
  --scenarios picker preferences detached \
  --minimum-improvement 50

cargo clippy --locked --all-targets --all-features -- -D warnings
```

Use a baseline that actually contains the requested scenario. A later frozen
binary may already contain an earlier optimization and therefore cannot measure
that earlier change. Plugin startup uses a separately pinned `c7408b2` baseline
and passed the minimum-improvement gate over 21 alternating samples.
