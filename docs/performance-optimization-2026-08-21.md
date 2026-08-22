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
| Husk unchanged document updates | 98.44% |
| Viewport cursor snapshot updates | 98.58% |
| Embedded Vim line-boundary motions | 98.48% |
| Editor LSP request cursor coordinates | 98.29% |
| Shared Vim final-sentence operators | 98.20% |
| Gutter namespace updates | 97.94% |
| Shared Vim around final-sentence text objects | 97.88% |
| Embedded Vim word motions | 97.88% |
| Shared Vim inner final-sentence text objects | 97.87% |
| Decoration namespace updates | 97.48% |
| Long transcript cursor lookup | 97.35% |
| Character-wrapped layout cursor lookup | 97.29% |
| Embedded Home/End document navigation | 96.88% |
| Editor cursor display-column positioning | 96.83% |
| Word-wrapped layout cursor lookup | 96.61% |
| Git workspace row navigation | 96.35% |
| Editor logical line-length lookup | 96.33% |
| Editor final-cell boundary lookup | 96.24% |
| Real-terminal cursor-moved plugin delivery | 95.05% |
| Shared ASCII grapheme counting | 94.94% |
| ASCII LSP rename-symbol extraction | 94.70% |
| LSP absolute-document routing | 94.68% |
| Real-terminal redundant full-frame publication | 93.65% |
| Real-terminal edit-invalidated Rust highlighting | 93.42% |
| Shared display-column-to-scalar conversion | 93.33% |
| Unicode Vim line-end operators | 93.21% |
| Editor scalar-to-grapheme cursor conversion | 92.82% |
| Workspace inline file discovery | 92.65% |
| Editor backward word-end boundary operators | 92.63% |
| Shared buffer final-line lookup | 91.46% |
| Shared buffer navigable-line counting | 91.38% |
| Embedded redo cursor restoration | 91.33% |
| Embedded text-area document loading | 91.26% |
| Sparse ASCII full-buffer regex searching | 90.87% |
| Embedded Vim nested delimiter matching | 90.03% |
| ASCII Vim line-end operators | 89.83% |
| ASCII Unicode-scalar line boundaries | 89.78% |
| Shared scalar-to-display-column conversion | 89.31% |
| Unicode scalar line boundaries | 89.12% |
| Sparse Unicode full-buffer regex searching | 89.12% |
| Shared Vim escaped-quote text objects | 88.86% |
| Shared Vim ASCII backward character search | 88.82% |
| Shared Vim long-line paragraph operators | 88.15% |
| Shared modal editor backward word motion | 87.58% |
| Shared modal editor forward word motion | 87.01% |
| Printable ASCII frame rendering | 85.25% |
| Startup user-configuration loading | 84.04% |
| Embedded undo cursor restoration | 84.17% |
| Long-line Vim word-end motion | 84.79% |
| Shared Vim ASCII forward character search | 84.55% |
| Idle plugin timer polling | 84.29% |
| Default editor status-line rendering | 83.45% |
| Workspace inline content search | 82.60% |
| Unicode LSP rename-symbol extraction | 82.49% |
| Editor forward word-end boundary operators | 81.94% |
| Real-terminal editor chrome rendering | 81.18% |
| Long-line Vim backward word motion | 80.33% |
| Theme hexadecimal color parsing | 79.71% |
| Detached incremental frame serialization | 79.06% |
| Real-terminal completion-aware edit frames | 78.19% |
| Shared Vim final sentence cursor boundary | 75.93% |
| Real-terminal typing action handling | 75.81% |
| Embedded forward Delete-key editing | 75.72% |
| Shared Vim final paragraph cursor boundary | 75.63% |
| Shared Vim Unicode forward character search | 75.52% |
| Shared Vim Unicode backward character search | 75.26% |
| Shared Vim Unicode around quoted text objects | 75.15% |
| Shared Vim ASCII inner quoted text objects | 75.00% |
| Embedded Ctrl-Backspace word deletion | 74.86% |
| Shared Vim ASCII around quoted text objects | 74.66% |
| Shared Vim sentence navigation | 74.63% |
| LSP incremental large-document changes | 74.61% |
| Real-terminal text-insertion events | 74.32% |
| Shared Vim Unicode inner quoted text objects | 74.07% |
| Embedded Vim long-line end motions | 72.63% |
| Shared Vim paragraph navigation | 72.60% |
| Undo history capacity pruning | 72.42% |
| ASCII automatic indentation columns | 71.81% |
| Unicode automatic indentation columns | 71.42% |
| Real-terminal YAML typing action handling | 71.04% |
| Real-terminal YAML text-insertion events | 69.78% |
| Real-terminal edited-window painting | 69.23% |
| LSP completion filtering | 67.58% |
| Vim first-nonblank line-start operators | 64.81% |
| Structured picker ranking | 64.81% |
| Real-terminal Markdown typing action handling | 64.30% |
| Shared Vim around paragraph text objects | 63.73% |
| Shared Vim paragraph boundary operators | 63.42% |
| Shared Vim inner paragraph text objects | 63.01% |
| Real-terminal Markdown text-insertion events | 62.37% |
| Complete real-terminal interactive startup | 60.80% |
| Git workspace status directory indexing | 60.28% |
| Git repository discovery and branch refresh | 58.73% |
| Real-terminal YAML completion-aware edit frames | 58.12% |
| Real-terminal Markdown completion-aware edit frames | 56.44% |
| Plugin cursor-event delivery | 56.35% |
| In-buffer search navigation | 55.98% |
| Real-terminal edit-invalidated YAML highlighting | 55.94% |
| Owned Husk JSON boundary conversion | 54.49% |
| Complete real-terminal YAML interactive startup | 54.08% |
| Bundled theme startup loading | 53.81% |
| Complete editor frame composition | 52.88% |
| Real-terminal edit-invalidated Markdown highlighting | 52.65% |
| Real-terminal bundled plugin startup | 51.88% |
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

Git status requests outside repositories were measured across 32 production
refreshes from nested workspace directories. Eleven alternating release-build
samples reduced the median from 172,090 to 731 microseconds by avoiding Git
subprocesses when no repository exists. Real repositories retain modified,
untracked, and ignored status entries, nested repository precedence, linked
worktrees, canonical roots, and retargeted directory symlinks.

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
from 714 to 311. Markdown startup improved only 48.61% and remains open.

Terminal diff and flush improved only 19.51%, and overlay/cursor composition
remained unchanged. Process-to-first-paint also remains unresolved because
executable warm-up and filesystem effects make its samples unstable.

## Remaining gaps

- Single-file process startup, Markdown startup, overlay/cursor composition,
  terminal diff and flush, broader non-identifier syntax edits, recovery
  snapshot writes, in-repository Git subprocess status refresh, broader Vim
  editing, platform-specific paths, and several other areas above do not yet
  meet the 50% improvement target.
- Real-repository Git status refresh improved 35.67%, from 418,087 to 268,941
  microseconds across 32 requests, but the remaining `git status` subprocess
  still keeps this path below the target.
- An eager Neo-tree row index was intentionally rejected because it increased
  memory and slowed opening; the retained single-position cache avoids both
  regressions in real 2,048- and 8,192-entry terminal runs.
- A SHA-256-verified recovery-generation cache was also rejected: although it
  preserved all corruption and no-follow safeguards, durable snapshot writes
  became 3.93% slower in the 24-buffer, 48-undo-node fixture.

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
