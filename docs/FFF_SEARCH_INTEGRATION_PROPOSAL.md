# FFF integration proposal

Investigated 2026-11-06. **Implementation update.** Find in Files now includes
an opt-in FFF backend. Launch with `red -c plugin_config.project_search.backend=fff`.
Parity testing led to a narrower integration: use FFF’s bigram index to select
candidate files, then match the current bytes with the same regex and streaming
search libraries as rg. The editor builds the index on a background thread,
walks current ignore rules on each query, and verifies file identity and change
time before skipping a file. This avoids file-backed mmap and an additional
recursive filesystem watcher. The rest of this document records the original
proposal. See the [parity harness and validation notes](../tools/fff-parity/README.md)
for the implemented contract and remaining rollout criteria.

Use the embedded `fff-search` Rust library for **Find in Files**, keeping the
existing picker, preview, history, split opening, and export behavior. Make the
first implementation milestone a compatibility harness. Switch the default only
after it proves that the library can preserve Red's search behavior and improves
the interactive experience within a measured memory budget.

Ctrl-P already uses native Rust discovery and fuzzy matching. Replacing that
implementation is a separate decision; it is not necessary to remove Red's
current `rg` subprocess dependency.

## Evidence and scope

- Red worktree: `/Users/felipe.coury/code/red.fcoury-fff-search`.
- Branch: `fcoury/fff-search`, based on current `origin/main`,
  `5b15a726e98f5a4c7e8a05aa18523bf1708d8339` (Red v0.11.1).
- FFF means [dmtrKovalenko/fff](https://github.com/dmtrKovalenko/fff).
  Inspected release [v0.11.0](https://github.com/dmtrKovalenko/fff/releases/tag/v0.11.0),
  commit `95fd777c2529fc7b4d7572dabff64cc07268f2c5`, and main at
  `b35b96ad065aea26aafb73eac69fd4866f173b76`.
- Evidence is source inspection and release metadata. No dependency build,
  differential tests, runtime smoke, or performance measurements were run.
  Upstream performance claims are not evidence of a Red speedup.

## What Red does today

| Component | Behavior | Consequence for this work |
| --- | --- | --- |
| [`plugins/project_search.hk`](../plugins/project_search.hk) | The built-in `rg` invocation is in `run_search`. Queries debounce for 100 ms, cancel the old process, and consume `rg --json`. Defaults are regex and smart case; hidden, ignored, and symlink following are off. | This is the first migration target. |
| Project-search presentation | One row per matching line; 500-row cap; refreshes batched at 16 ms. External filtering is already enabled. It supports preview, split opening, history per cwd, and export. | Preserve this presentation; replace process events and rg JSON parsing. |
| [`src/ui/file_picker`](../src/ui/file_picker) | Ctrl-P uses `ignore` and `SkimMatcherV2`, cached discovery, asynchronous matching, cancellation, and stale-result rejection. | Leave it in place for the first migration. |
| [`src/workspace_paths.rs`](../src/workspace_paths.rs) | Neo-tree uses a separate path index, including directories and ancestor projections. | FFF's file index is not a direct replacement for this contract. |
| [`default_config.toml`](../default_config.toml) and CI | The project-search plugin has `process = ["rg"]`; four workflows install rg. | Remove these only once the replacement and tests no longer require rg. |

The current search root is cwd, not the Git root. The search reads saved files on
disk. Unsaved-buffer search, fuzzy content search, query constraints, and frecency
ranking are possible later features, not prerequisites for this migration.

## Why embed the library

FFF can retain its file inventory, content index, cache, and watcher across
queries. Embedding also removes process startup and rg's JSON output parsing on
each query. These are plausible benefits for repeated interactive searches;
index startup, resident memory, watcher traffic, and native dependency costs are
the corresponding tradeoffs.

Start the harness with `fff-search = "=0.11.0"` and its default features. The
package is MIT licensed, uses Rust edition 2024, and upstream selects stable Rust.
Its feature named `ripgrep` means the embedded `ignore`/`globset` backend; it does
**not** require the rg executable. Optional `zlob` requires Zig. Keep Zig out of
the first integration. Native dependencies still include libgit2 and LMDB;
verify compiler compatibility, linking, binary size, and dependency duplication.
The release does not declare an MSRV. [FFF manifest][fff-manifest],
[toolchain][fff-toolchain], [upstream CI][fff-ci].

An external fff CLI would add another executable and would not automatically
retain engine state between searches. An MCP server adds protocol and process
lifecycle work that Red does not need for its own picker. A persistent helper
process remains an option if safe embedding cannot be established, but it should
be an explicit architectural decision after the spike.

## Compatibility work required before adoption

The released library cannot preserve every current behavior through option
mapping alone. Prefer narrow, tested upstream changes over accumulating query
rewrites and result filtering in Red.

| Area | Inspected FFF behavior | Required handling |
| --- | --- | --- |
| Hidden and ignored files | Release walker includes hidden files for Git roots, hides them for non-Git roots, and always honors ignores. It has no public hidden/ignored overrides. `.rgignore` is not registered. | Add traversal policy for Red's toggles, `.rgignore`, Git/global/local ignores, and `.git`/`.bare` exclusions. Apply policy to initial scans, rescans, and watcher admissions. Filtering matches cannot recover files omitted from an index. |
| Query text | Parser recognizes path/glob/Git constraints. Constructing an unparsed `FFFQuery` avoids that, but `grep_text()` still removes a leading backslash before `*`, `/`, or `!`. Grep also converts `\n` escapes even in literal mode. | Require a verbatim pattern entry point with explicit regex/literal mode. Test `\*`, literal `\n`, repeated whitespace, `*.rs`, `git:modified`, and paths as search text. |
| Regex errors and Unicode | Invalid regex falls back to literal results with `regex_fallback_error`. Regex compilation disables Unicode by default; plain-text case folding is ASCII. | Reject invalid regex and discard fallback hits. Match Red's rg behavior for Unicode case, classes, word boundaries, and smart case; changing the library compiler may be needed. |
| Result limits | Defaults are 50 results/page, 200/file, and 10 MiB/file. File size is constrained by both query options and content-cache budget. Pages finish a file and can exceed their nominal limit. | Explicitly configure limits. Preserve Red's 500-row UI cap without silently losing matches in one file or excluding large files. Report incomplete results. |
| Locations and long lines | Lines are 1-based; columns/spans are bytes. Display text is truncated at 512 bytes, affecting match-span extraction for late matches. | Normalize to zero-based lines and original byte offsets. Preserve exact navigation independently of displayed text. Verify behavior against Red's current long-line invocation. |
| Root and links | Following links is an index option. Home/filesystem roots are rejected by default; non-Git roots have extra directory exclusions. | Preserve explicit cwd scope and traversal toggles. Test non-Git directories, linked worktrees, symlink loops, and outside-root targets when follow is enabled. Account explicitly for root restrictions. |
| Readiness and errors | Initial scan/content warmup are asynchronous; some walk/read failures are skipped. | Do not report an unfinished index or failed scan as completed zero matches. Carry failure/incomplete status and permit explicit retry. |

Sources: [walker][fff-walker], [query transformations][fff-query],
[grep implementation][fff-grep], [regex configuration][fff-regex],
[options/results][fff-results], [line handling][fff-sink].

Upstream main adds `FilePickerIgnoreOptions`, but the default Rust walker accepts
`_ignore_options` without applying them at the inspected revision. Upgrading to
main therefore does not solve the visibility gap while retaining the default
backend. Main also changes APIs and includes a pagination correction absent from
the release. Pin the exact patched revision used by the eventual implementation;
do not mix release code with current-main documentation. [Main walker][fff-main-walker].

There is also an embedding reliability gate: Unix searches can mmap files being
changed externally. Upstream acknowledges SIGBUS on concurrent truncation, and
disabling persistent mmap caching still leaves a temporary-mmap path. A file
watcher is not a synchronization guarantee against another process truncating a
file. Before shipping inside the editor, establish a safe read mode or another
sound mitigation, and exercise rewrite/truncate races in an isolated harness.
A passing stress run alone does not establish safety. [Content I/O][fff-io].

## Proposed Red architecture

Add a small `workspace_search` module owned by `Editor`. The plugin remains the
owner of presentation and interaction. Keep FFF types behind the native adapter
so the plugin protocol and UI do not depend on an upstream result layout.

```text
project_search.hk: query + flags + existing debounce
    -> owned native search request
    -> Editor workspace-search service
    -> background FFF worker and persistent root index
    -> owned, normalized result payload
    -> existing picker / preview / history / export
```

**Engine lifetime.** Initialize lazily on the first search; retain the engine
across query changes and picker closure. Key it by canonical active search cwd
and hidden/ignored/follow policy, never the common Git directory. A search opened
in a subdirectory must remain scoped there. Start with one
active engine and replace it on root/policy changes; do not retain all eight flag
combinations. Stop and dispose of obsolete engines off the editor thread. Add an
LRU only if measurements justify keeping multiple roots resident.

**Worker behavior.** Use one active query plus a latest-query slot for the first
consumer. Each request has its own atomic abort signal. Superseding the request,
closing the picker, changing workspace, or deactivating the plugin cancels that
request and suppresses delivery. FFF's engine-wide `cancel()` is for teardown,
not query cancellation. Dropping a callback or aborting a `spawn_blocking` handle
does not cancel native computation. Never acquire FFF locks, wait for scanning,
or release a large index on the editor loop. FFF results borrow indexed files;
copy the bounded result into owned values before releasing the worker's read
guard. [FFF lifecycle][fff-lifecycle], [grep API][fff-api].

**Host API.** Proposed names are `SearchWorkspaceContent(callback, options)` and
`CancelWorkspaceSearch(request_id)`. These do not exist today. Options carry the
pattern, regex/hidden/ignored/follow flags, and query generation. The host captures
the active absolute cwd before dispatch and returns it with the result. Reuse
`GetGitStatus`/`CancelGitStatus` ownership and callback cleanup patterns in
[`src/plugin/runtime.rs`](../src/plugin/runtime.rs) and
[`src/editor.rs`](../src/editor.rs), with actual cooperative cancellation added.
Do not intercept or reinterpret `SpawnProcess("rg")`.

There is no existing `fs_read` plugin permission: current permissions are for
processes and environment inheritance. Expose this first API for the active
workspace, consistent with the built-in search's scope. Preserve the existing
explicit follow-symlinks behavior, which can reach targets outside cwd; do not
claim strict filesystem confinement. An explicitly selected rg fallback must
still pass the existing process permission check.

**Results.** Return request/root/generation identity, stable row IDs, paths,
zero-based line numbers, original byte ranges, bounded display text, and explicit
complete/truncated/error state. Picker display highlights use character offsets;
preview and open locations use bytes. Keep that conversion at the adapter/UI
boundary. Open/preview/export must continue to refer to the captured root after
cwd changes. Bound total payload bytes as well as rows.

Start with one bounded completion through the existing request/callback model;
the picker stays visibly busy while indexing/searching. This changes time to
first result relative to rg streaming, so it is a measured acceptance criterion.
For the first harness, evaluate a single page with 501 requested rows and up to
501 matches per file, then cap displayed rows at 500. This bounds per-file
collection while detecting overflow; the soft page cap still needs a hard
adapter payload bound. FFF collects parallel chunks before applying the page
limit, so this does not bound its transient query memory. Measure that separately.
Do not present an aborted or budget-limited search as
complete. Add progressive delivery or cursor continuation only if needed and
after testing cursor correctness against the selected FFF revision.

**Freshness and resources.** Start without persistent frecency/query-history
databases (`SharedFrecency::noop()`); retain Red's existing query history. Disable
Git recency explicitly. These settings do not remove all ranking: FFF still
orders candidate files by scores and modification time. Evaluate that ordering
and which 500 rows survive the cap as an explicit UX decision; compare result
sets below the cap rather than promising identical rg arrival order. Enable
content indexing explicitly for performance experiments—it is off by default.
Measure a conservative explicit cache budget, with 64 MiB as a starting
experimental setting, not a promised RSS cap. File metadata, bigram indexes,
temporary reads, and workers also consume memory. Keep file-size eligibility
separate from cache retention. `ContentCacheBudget::zero()` also makes the maximum
file size zero; it is not a safe shorthand for searchable uncached mode.
[Budgets][fff-budgets].

Use FFF's watcher plus explicit save/create/delete/rename/ignore-change
invalidation and a refresh path. Existing Ctrl-P invalidation intentionally
ignores edits to the contents of an existing file, so it is insufficient for
content search. Coordinate pending updates with query startup and test immediate
search-after-save. Ensure branch switches and watch failures can trigger a
rescan, and old results cannot overwrite a newer query or workspace.

## Delivery sequence

1. **Compatibility and build spike.** Add an isolated harness for the pinned
   release. Resolve/build with Red's stable toolchain and default FFF features.
   Compare normalized FFF results with the current rg arguments on a fixture
   corpus. Record failures before fixing them. Establish the safe I/O path and
   exact upstream changes needed for raw patterns, traversal policy, Unicode,
   long-line offsets, and any cursor defects. Decide whether maintaining those
   changes is acceptable before connecting the UI.
2. **Opt-in Find in Files integration.** Add the native service, owned
   start/cancel requests, result normalization, and a backend setting under
   `plugin_config.project_search`. Keep `rg` as the default initially; explicit
   `fff` selection must report errors rather than silently changing backends.
   Preserve plugin interaction and retain rg as a selectable rollback while
   verifying the implementation.
3. **Default switch and dependency removal.** After parity and performance
   gates pass on all supported platforms, make FFF the default. Remove the rg
   implementation, bundled permission, and rg-dependent integration tests when
   rollback is no longer needed. Remove CI installation only after a repository
   dependency audit. Replace the picker placeholder `Search with ripgrep` with
   backend-neutral text.
4. **Optional filename search follow-up.** Evaluate FFF for Ctrl-P separately.
   Preserve `file:line`, hidden/ignored toggles, previews, selection stability,
   Unicode highlighting, and path coverage. Treat typo tolerance/frecency as
   deliberate ranking changes. Keep Neo-tree's directory and ancestor behavior
   unless a separate design reproduces it.

Host API versioning must follow the released boundary. Red v0.11.1 already ships
host API `0.21.0`, verified from its release source. If no newer pending version
exists when implementation starts, these additions belong to a single pending
`0.22.0`. Recheck then; use the same pending version for every phase. Update the
canonical schema, change manifest, docs, compatibility checks, and any affected
plugin minimum versions together. This proposal does not bump any version.

## Validation and decision criteria

The differential corpus should compare sets of `(path, line, byte ranges)` where
results fit below the cap, and explicitly verify overflow behavior above it.
Cover Git and non-Git roots; nested/global ignores and `.rgignore`; every toggle;
linked worktrees; external symlinks and loops; Unicode/CRLF/non-UTF-8 inputs;
regex errors and escaped literal text; multiple spans per line; late matches on
long lines; files above 10 MiB; and more than 500 matches in one file.

Exercise cancellation during scan and search, rapid typing/backspace, picker
close/deactivation, root switches, external rewrite/truncate, save followed by
immediate search, create/delete/rename, ignore changes, Git checkout, and watcher
failure. Verify current/split opening, previews, history, export/reopen without
focus theft, stale-result rejection, and a usable editor throughout.

Adapt the existing project-search integration and cleanup tests in
`src/plugin/runtime.rs`; preserve structured-picker tests for external filtering
and selection by ID. If Ctrl-P is later migrated, its existing ignored real-repo
benchmark and path-set parity checks provide a baseline.

Measure Red's own repository and a substantially larger repository on macOS,
Linux, and Windows. Separate cold scan, indexing warmup, first result, completed
query, warm repeated query, cancellation latency, and file-change visibility.
Report p50/p95 plus idle/peak RSS, idle CPU, watcher/thread counts, and binary
size. Compare the same result scope, count, debounce, and query corpus; include
zero-match and high-match queries. Windows uses a different file-reading path,
so a macOS improvement does not establish a Windows improvement.

Proceed to the default switch only with preserved search coverage and navigation,
no stale results, a sound response to concurrent file writes, bounded background
work, and measured warm-query improvement without unacceptable cold-start or
memory cost. Record platform gaps explicitly. Before pushing Rust changes, run
the repository-required `cargo clippy --all-targets --all-features -- -D warnings`
and fix every warning/error.

The next concrete step is the standalone parity/build harness in this worktree.
Its outcome should select a tested FFF revision and answer whether direct
embedding needs upstream patches or a different process boundary.

[fff-manifest]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/Cargo.toml
[fff-toolchain]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/rust-toolchain.toml
[fff-ci]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/.github/workflows/rust.yml
[fff-walker]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/walk/ripgrep.rs
[fff-query]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-query-parser/src/parser.rs#L223-L260
[fff-grep]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/grep/grep.rs
[fff-regex]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/grep/regex.rs
[fff-results]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/grep/types.rs
[fff-sink]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/grep/sink.rs
[fff-main-walker]: https://github.com/dmtrKovalenko/fff/blob/b35b96ad065aea26aafb73eac69fd4866f173b76/crates/fff-core/src/walk/ripgrep.rs
[fff-io]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/types.rs#L636-L808
[fff-lifecycle]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/shared.rs
[fff-api]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/file_picker.rs#L1389-L1415
[fff-budgets]: https://github.com/dmtrKovalenko/fff/blob/95fd777c2529fc7b4d7572dabff64cc07268f2c5/crates/fff-core/src/types.rs#L943-L1039
