# Red post-foundation sweep and implementation plan

Date: 2026-07-11
Branch: `feat/agent-native-foundation` at `ec8604c`
Status: implemented end to end in the local worktree

This sweep reviews the complete agent-native foundation branch, the current working tree, editor and terminal behavior, detachable sessions, ACP, plugins, CI/release, documentation, tests, and daily-driver feature gaps. Findings are ordered by user impact and by the risk of shipping a misleading or broken contract. The intent is to make the next changes small, independently reviewable, and testable through production paths.

## Implementation status (2026-07-11)

The safety, correctness, delivery, and responsiveness work identified by the sweep is implemented. The historical findings and source-line evidence below describe the pre-implementation baseline; line numbers and inventory counts may have shifted as the fixes and regressions were added.

- **F01/F10 - delivery and documentation:** CI now has one strict Clippy job, pinned workflow linting, real relative-link and anchor validation, valid example-plugin metadata, and multiline-aware packaged self-check gates. Git's documented `signs_staged` setting is honored with legacy compatibility, plugin/API/release/session documentation matches runtime behavior, and historical JavaScript/type surfaces are clearly marked.
- **F02/F03/F06/F11/F12 - editor, terminal, and Vim correctness:** plugin buffer reads preserve exact bytes and ranges, including CRLF, Unicode, and unterminated files; native and detached resizing safely handles tiny panes, Unicode widths, row-cache truncation, mouse input, large paste, and wrap state; detached rendering is dirty/delta driven and instrumented; operator counts, dot/macro replay, EOF behavior, linewise paste, and `zz` have production-path regressions. Native and detached tmux smoke runs repeatedly resized Unicode content without stale rows, wrapping, corruption, or editor exit.
- **F04/F05/F07/F08/F09 - ACP, detach, session, and plugin hardening:** ACP framing, pending requests, prompt lifetime, cancellation, callbacks, proposal refresh, and transcript/MRU behavior are bounded and failure-isolated. Detach handshake, dimensions, frames, paste, permissions, stale files, idle stop, and client writes are bounded and authenticated; snapshots fail closed and preserve a valid predecessor; subprocess environments, input/output, metadata, API signatures, command ownership, and reload side effects are constrained and regression-tested.
- **F13 - production ACP and LSP daily-driver actions:** the bundled `red_openai_acp` adapter uses the OpenAI Responses tool loop, keeps reads and writes on Red's reviewable filesystem callbacks, bounds workspace tools and transport output, and passes live multi-turn/read-after-write/proposal checks without changing disk. The bundled `red_codex_acp` companion uses an installed, authenticated Codex CLI with native execution and patch tools disabled, exposes bounded workspace tools, routes unsaved reads and all writes through Red's proposal host, and passes real app-server handshake plus proposal, traversal/symlink, cancellation, authentication, and failure-path coverage. First use is now `Space A`/`:Agent`, type, and submit: sessions start lazily, missing credentials open a masked session-only setup flow, the prompt survives setup/retry, proposals announce exact file/hunk counts, and `--agent-check --strict` verifies local prerequisites for either backend. LSP now validates URI, UTF-16/CRLF, version, revision, root, symlink, size, overlap, and concurrency constraints before applying edits to open or unopened UTF-8 files; ordered create/rename/delete, rollback, server-initiated `workspace/applyEdit`, rename, formatting/format-on-save, code actions, completion edits, and signature help have focused mock-server coverage. Filesystem resource edits intentionally fail closed on non-Unix platforms and are not undoable through the per-buffer undo stack.

Final validation passed formatting, all 1,116 all-target/all-feature tests, strict all-target/all-feature Clippy, ACP/detach/editing/LSP/Git-buffer/self-check regressions, workflow linting, Markdown-link and anchor checks across 39 files, repository inventory, manifest validation, and `git diff --check`. The two home-path tests require execution outside the filesystem sandbox and pass there. Clean-profile tmux smokes verified `Space A`, lazy startup, OpenAI and Codex setup choices, masked paste with secret-free performance tracing, exact readiness failures, a real installed-Codex session, unchanged disk, and isolated adapter stderr. The corrected release detach benchmark at 50x120 with Unicode, mouse input, repeated resizes, 120 paced multi-line edits, detach/reattach, and a 1,536 KiB paste completed in 5.83 seconds, emitted 107 KiB, and recorded 192 microseconds p95 frame serialization; a second independent run also completed cleanly. Husk's 2,000-event cursor benchmark recorded 1,589 microseconds p95. Release self-check activated all 12 default plugins, agent readiness is `ready`, and a live GPT-5.6 ACP turn retained context and left disk unchanged.

Detached-session CLI ergonomics are resolved: `red --detach path` opens the path in the default session and `red --detach=NAME path` selects a named session unambiguously. The remaining qualification is environmental, not a known code failure: an MSVC cross-target check from this macOS host cannot compile `ring` without the Windows SDK (`assert.h`/`VCINSTALLDIR` are unavailable), so the packaged Windows job remains the authoritative gate for that target.

## Pre-implementation executive summary

The branch has substantial functionality, but several important paths are not yet launch-safe:

- CI contains a duplicate job key and packaged release smoke rejects the current, valid self-check output.
- A plugin request used to stage unsaved Git hunks can return altered or truncated buffer text.
- Detached rendering corrupts wide-character rows, misses native input and resize behavior, performs unnecessary full renders, and can stall or exceed its advertised IPC bounds.
- ACP callback failures can terminate an otherwise healthy agent, ordinary turns inherit a 30-second timeout, and framing/pending/output limits are not actually bounded.
- The published plugin API signatures, some configuration keys, compatibility claims, and release/plugin documentation disagree with runtime behavior.
- Counts for Vim operators and `zz` do not match the published compatibility matrix.
- The most valuable next product tracks remain a genuinely reviewable production ACP adapter and URI/UTF-16-correct multi-buffer LSP edits.

The recommended first sequence is: **repair delivery gates, restore exact buffer semantics, make terminal/detach rendering correct, harden ACP/IPC and snapshots, reconcile the plugin contract, then pursue daily-driver features**.

## Pre-implementation baseline and evidence gathered

The sweep used the local checkout and live tmux sessions. No external service state is assumed.

- `python3 scripts/repository_inventory.py` reports 161 `Action` variants, 71 `PluginRequest` variants, 13 bundled Husk plugins, and a 1,956-line `git.hk`.
- `NO_COLOR=1 target/debug/red --self-check` reports all 12 default bundled plugins active followed by `red self-check ok`.
- `NO_COLOR=1 target/debug/red --agent-check` reports no configured production adapter and `reviewable-edit readiness: not ready`.
- The current Rust baseline passed formatting, strict Clippy, and all 933 unit and integration tests during the preceding resize investigation. That green baseline does not cover the failures below.
- A detached 44x12 tmux session displaying `start 👋 漢字 👩‍💻 끝 end` lost its first content row and added padding spaces; the same file in a native session rendered correctly. This reproduces the wide-grapheme serialization defect.
- Shrinking a native tmux editor pane to two rows exited the editor. The resize handler performs unchecked subtraction for heights below three.
- `red --detach src/main.rs` exits with `detach session names may contain only letters, numbers, dash, underscore, and dot`: the optional session argument consumes the file path.
- `python3 -m json.tool examples/example-plugin/package.json` currently fails at line 1 because the working-tree manifest contains Husk source. This is an **existing local edit**, not a committed branch change; preserve it until the intended content is confirmed.
- `src/main.rs` already contains the local fix and regression for cached rows surviving a detached vertical shrink. Keep that fix when implementing the broader terminal work.

Effort labels below are deliberately coarse: **S** is a contained fix, **M** spans a few components/tests, **L** is a feature-sized track, and **Spike** requires external or architectural validation before committing to delivery.

## Prioritized findings

### P0 - delivery, data-integrity, and terminal-correctness blockers

#### F01. CI defines `clippy` twice and release smoke rejects valid self-check output (S)

Evidence:

- `.github/workflows/ci.yml:76` and `:221` both define the `clippy` job key. Duplicate mapping keys are invalid or silently overwrite one definition in permissive YAML tooling.
- `.github/workflows/release.yml:126-127` and `:139-141` require self-check output to equal exactly `red self-check ok`, while `src/self_check.rs:17-24` emits one status line per plugin before the final success line.
- `actionlint` is not installed in the current environment and is not a CI gate. The docs job at `.github/workflows/ci.yml:325-326` calls `cargo doc` again rather than checking Markdown links. The MSRV job at `:343-364` skips because the root `Cargo.toml:1-6` has no `rust-version`.

Plan:

1. Keep one strict Clippy job and add an `actionlint` workflow-validation job.
2. Make Unix and Windows release smoke assert the final success line and reject quarantined/error statuses instead of exact whole-output equality. Add a focused CLI-output contract test.
3. Add a real relative-Markdown-link check. Either declare and test a supported Rust version for every workspace member or remove the misleading MSRV job.
4. Update `docs/RELEASING.md:18-44` to use the existing Prepare Release workflow (`.github/workflows/prepare-release.yml:61-137`), generated changelog, green PR gates, and packaged smoke. The current runbook's direct-tag path omits the changelog validation enforced by `.github/workflows/release.yml:173-216`.

Exit checks:

```text
actionlint .github/workflows/*.yml
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
target/debug/red --self-check
```

Exercise both release-smoke shell branches with representative multiline self-check output.

#### F02. `GetBufferText` can corrupt or truncate unsaved content used by Git (M)

Evidence:

- `src/buffer.rs:279-285` returns Rope lines including their line endings.
- `src/editor.rs:4961-4977` defaults the exclusive end to `current_buf.len()`, iterates `start..end`, then calls `lines.join("\n")`. Existing line endings are doubled and a final non-newline line can be omitted.
- `plugins/git.hk:1594-1612` passes that result directly to `git diff --no-index ... -` when staging an unsaved hunk. A malformed snapshot can produce an incorrect hunk or stage text different from the visible buffer.
- The canonical schema says `GetBufferText(callback, buffer_id?)` at `src/plugin/host_api.json:70`, while runtime interprets optional arguments as `start_line, end_line` at `src/plugin/runtime.rs:712-725`.

Plan:

1. Define exact byte-preserving semantics. Full-buffer requests should use the buffer contents directly; range requests should use a documented `[start, end)` line slice without synthesizing separators.
2. Decide whether buffer selection is supported. If it is, add an explicit buffer identifier rather than overloading line arguments and reconcile the schema/runtime API in F08.
3. Cover empty buffers, one line with and without a final newline, multiple lines, CRLF, Unicode, bounded ranges, inactive buffers, and an unsaved Git hunk fixture that verifies the exact stdin and staged patch.

Exit criterion: every returned byte matches the visible buffer/range and an unsaved hunk stages exactly what Red displays.

#### F03. Native and detached terminal resize/render paths are not safe or equivalent (M)

Evidence:

- Native resize at `src/editor.rs:5594-5599` computes `height as usize - 2` and then `max_y - 1`. Heights zero through two can underflow; the live two-row tmux resize exited the editor.
- Detached resize at `src/editor.rs:1581-1593` updates size/layout directly, bypassing dialog resize/dismissal and the `editor:resize` notification in the native path at `:5606-5622`. `plugins/fidget.hk:1-2,24-29` consumes that event, so overlays can retain stale geometry.
- `RenderBuffer::set_text` stores a width-two grapheme followed by a padding space (`src/editor/render_buffer.rs:231-265`), but both detached row serializers concatenate every cell (`src/editor.rs:1681-1717`). The attach painter prints with wrapping enabled (`src/main.rs:402-427`). A serialized row can therefore exceed the terminal width and wrap/scroll into another row.
- The native renderer explicitly disables wrapping and skips wide padding while emitting runs (`src/editor/rendering.rs:1483-1523`); detached rendering does not share that invariant.
- The existing detached-height fix at `src/main.rs:443-450` correctly drops cached rows below a shrunken terminal and must remain covered.

Plan:

1. Introduce one clamped terminal-size/viewport helper. Reject or safely render zero/tiny dimensions, use saturating arithmetic, and test widths/heights zero, one, and two plus rapid resize bursts.
2. Route detached resize through the same production event pipeline as native resize after clamping, including dialog geometry and plugin notification. Coalesce adjacent resize events at the attached client so a divider drag does not require one IPC round-trip per intermediate size.
3. Create one display-cell row encoder shared by plain/styled detached output: skip wide-grapheme padding, preserve styles, and assert encoded display width never exceeds the negotiated columns. Disable terminal line wrapping during frame painting and restore it afterward.
4. Keep height truncation, clear on connect/resize, and cover grow/shrink, focus changes, dialogs, overlays, emoji, CJK, combining/ZWJ graphemes, tabs, and theme changes in a real PTY/tmux regression.

Exit criterion: native and detached sessions render identical cell grids across repeated horizontal/vertical resizes, including tiny panes and wide Unicode, without exit, wrap, stale rows, or clipped overlays.

### P1 - agent/session safety, published-contract, and daily-use correctness

#### F04. ACP host failures terminate the transport and normal prompts inherit a 30-second timeout (M)

Evidence:

- `src/acp/transport.rs:603-619` propagates filesystem-read, filesystem-write, and permission callback errors with `?`; `ProcessActor::run` propagates that error and exits. The workspace intentionally rejects outside-root and symlink paths (`src/agent_workspace.rs:402-412,512-529`), so a legitimate denial can kill the entire agent session rather than returning a request-scoped JSON-RPC error.
- The default request timeout is 30 seconds (`src/acp/transport.rs:27-29`). The prompt path at `:209-228` uses the same timeout, even though ordinary agent turns can run substantially longer. The timeout drops only the response receiver (`:427-439`); the actor's pending entry is removed only when a late response arrives (`:510-513,563-565`).

Plan:

1. Convert invalid parameters and host-policy/callback failures into correlated JSON-RPC error responses while keeping the actor alive. Reserve actor exit for framing, child-process, and unrecoverable IO failures.
2. Separate bounded setup/control requests from long-running prompts. Prompts should remain active until completion, explicit cancellation, or a clearly configured long-turn policy; cancellation must clean correlation state.
3. Add deterministic fixture/paused-clock tests: rejected outside-root read followed by a valid read and completion; malformed request followed by valid traffic; long prompt; never-responding prompt; repeated timeout/cancel; and late/unknown responses.

Exit criterion: one rejected request never tears down a healthy adapter, normal turns do not fail at 30 seconds, and pending state is bounded and cleaned deterministically.

#### F05. ACP and detach framing/backpressure are not actually bounded (M)

Evidence:

- ACP stdout uses `BufReader::lines()` (`src/acp/transport.rs:399-403,472-497`) with no line/frame limit. An adapter can allocate an arbitrarily large line or never emit a newline.
- Detach declares a 1 MiB frame limit, but `read_frame` calls unbounded `read_until` before checking it (`src/headless/mod.rs:911-922`). The initial handshake has no deadline and can occupy the only attached slot. Resize dimensions are accepted as `u16` and used to allocate the render buffer without a practical bound (`src/editor.rs:1581-1590`).
- A bracketed paste is sent as one frame (`src/main.rs:301-303`); a paste above the frame limit fails the attached TUI instead of being inserted/chunked.
- The production input handler holds the core mutex across `write_frame().await` (`src/headless/mod.rs:548-554`), while background ACP/LSP/plugin ticks need the same mutex. A slow or non-reading client can freeze the supposedly independent owner.
- Permitted plugin subprocesses use an unbounded output channel and unbounded raw/line reads (`src/plugin/process.rs:93-102,296-351`). They also inherit the editor environment, which can expose unrelated credentials to a permitted `git`/`rg` process.

Plan:

1. Add capped incremental frame readers for ACP and detach, including an explicit no-newline/oversize error. Bound pending requests, child output, event queues, and retained bytes; document truncation/backpressure behavior.
2. Add a handshake deadline, validate nonzero/reasonable dimensions before allocation, free the attached slot on every invalid-client path, and use a scoped core guard so socket writes happen after releasing the mutex. Bound or time out writes so an abandoned client cannot wedge the owner.
3. Chunk large paste into ordered input frames with explicit begin/end or sequence semantics, while preserving one editor transaction and one LSP notification.
4. Define a minimal inherited subprocess environment plus explicit overrides. Do not implicitly pass every editor credential into a plugin child.
5. Add adversarial tests for oversize/no-newline frames, stalled handshakes, non-reading clients, extreme dimensions, repeated timeouts, stdout/stderr floods, large paste just below/at/above the limit, and successful reconnect.

Exit criterion: no local client, adapter, or permitted subprocess can grow memory without bound or block owner progress, and all malformed/stalled cases leave a usable session.

#### F06. Detached input, background updates, and rendering have major parity/performance gaps (M)

Evidence:

- Attached terminal setup enables paste/focus but not mouse capture, ignores mouse events, and the versioned `InputEvent` has only `Key` and `Paste` (`src/main.rs:274-311`, `src/headless/mod.rs:29-40`). Native cursor clicks, scrolling, panes, panels, and picker interaction therefore disappear when detached.
- The owner ticks every 10 ms (`src/headless/mod.rs:439-450`), and `DetachedEditorCore::tick` always calls `finish_render`, which fully renders and serializes every cell (`src/editor.rs:1620-1627,1643-1646,1681-1717`). An idle owner can render at 100 Hz and contend with input.
- Input/focus already render through `process_editor_event(...Immediate)`, then call `finish_render` and render again (`src/editor.rs:1559-1578,1596-1613`). The client clears/reprints the entire cached frame even for a one-line delta (`src/main.rs:392-403`).
- The attached client polls terminal input every 250 ms and only requests background deltas on a five-second heartbeat (`src/main.rs:281-317`). Agent streaming, progress, timers, and other idle updates can appear frozen for seconds.

Plan:

1. Extend the versioned detach protocol with normalized mouse events, enable capture/cleanup, and route click/drag/scroll through the production editor path. Add mouse/panel/picker/window tests and preserve keyboard-only use.
2. Make background servicing report a dirty/render-generation signal. Render and serialize only when state changes; perform one final render per input, and paint only changed rows except on connect/resize/recovery.
3. Separate render delivery from the lease heartbeat. Prefer bounded server push or a short render poll while retaining the longer liveness lease.
4. Instrument detach with `RED_PERF`, add idle render-count/CPU assertions, and add a 50x120+ PTY benchmark for input-to-frame p95 and output bytes.

Exit criterion: detached behavior matches native input/UI, idle sessions do no unnecessary rendering, and background updates appear promptly without a full frame per key.

#### F07. Snapshot writes can replace an unsupported snapshot or publish an insecure stale temp file (S/M)

Evidence:

- `src/session.rs:167-173` turns every `load()` error into generation zero via `unwrap_or_default`. A future-version, invalid, or unreadable snapshot that loading correctly rejects (`:221-234`) can be rotated/replaced by the next periodic write. If a corrupt latest replaces a good previous generation and a crash occurs during rotation, both recoverable generations can be lost.
- `restrictive_file` uses `create(true).truncate(true).mode(0600)` (`src/session.rs:243-251`); `mode` does not correct permissions on an existing temp file and does not prevent following a stale symlink. Renaming that temp can violate the owner-only snapshot guarantee.
- Snapshot-write failures are logged only (`src/editor.rs:13316-13329`), so a user can continue editing with no visible recovery warning.

Plan:

1. Distinguish not-found from invalid, future-version, and permission failures; fail closed without replacing an unsupported generation. Rotate only a validated latest generation and preserve a known-good previous snapshot.
2. Use a unique `create_new`/no-follow temp file or explicitly validate/fchmod the opened file, then fsync/rename/directory-sync as today.
3. Surface repeated snapshot failures as a bounded, actionable editor warning.
4. Add fault tests for corrupt latest plus good previous, crash after rotation, future-version immutability, stale 0644 temp, symlink temp, permission failure, and repeated write failure.

Exit criterion: a failed or unsupported snapshot can never destroy the last known-good generation or weaken file permissions.

#### F08. Canonical plugin API signatures and transactional guarantees disagree with runtime (M)

Evidence:

- `src/plugin/host_api.json:37` advertises `OpenBuffer(buffer_id: i32)` while runtime requires a name string (`src/plugin/runtime.rs:401-408`).
- The schema's `UpdateWindowBar`/`CloseWindowBar` entries at `:44-45` omit the window identifier runtime consumes (`src/plugin/runtime.rs:479-508`).
- `GetBufferText`, character/display-column conversion, and `Record*` calls similarly disagree (`src/plugin/host_api.json:57-60,70,84-85` versus `src/plugin/runtime.rs:625-661,712-725,802-810`).
- API validation checks call names, not call kind/signature parity (`src/plugin/api.rs:34-75,107-135`). A third-party plugin can follow the published contract and still fail at runtime.
- Dependency requirement/version parsing can silently accept invalid semver (`src/plugin/registry.rs:162-173`). Staged hot reload invokes the real host during activation/import, allowing a failed replacement to leak timers, processes, panels, or actions. Duplicate commands can overwrite one another nondeterministically and unloading the winner can remove the command.

Plan:

1. Decide the actual public signatures, reconcile runtime/schema/docs, and generate or mechanically validate declarations and dispatch metadata from one source. Test action/request kind, arity, optional arguments, and types, not just names. Bump API/migration metadata when a promised signature must change.
2. Make invalid dependency semver and duplicate command ownership explicit quarantine/diagnostic cases with deterministic precedence.
3. Give hot-reload staging an effect journal/staging host. Commit callbacks, commands, timers, processes, watchers, panels, and pending requests only after parse/typecheck/activation/state migration succeeds; roll them back on failure.
4. Add schema/runtime parity, bundled corpus, directory/manifest load, dependency, command-collision, reload-side-effect, and migration tests.

Exit criterion: documented signatures compile and behave as advertised, a bad plugin/reload affects only its owner, and no staged side effect leaks.

#### F09. Proposal and conversation updates are incomplete for real agent turns (M)

Evidence:

- An ACP write mutates proposal state but does not emit a coalesced `agent:proposals_changed` update (`src/agent_workspace.rs:454-466`, `src/acp/transport.rs:287-292`). The bundled review UI refreshes on proposal events, while completion does not force a refresh (`plugins/agent.hk:12-14,107-110,290-292`). An already-open review can remain stale until reopened.
- The editor creates one `ProposalWorkspace` for the first ACP cwd and reuses it for later sessions (`src/editor.rs:4538-4593`), although later adapters can receive another cwd. A second root is then rejected by path validation.
- Each streamed text chunk is independently prefixed/appended in the conversation panel (`plugins/agent.hk:103-105,121-128`). Split tokens and multiline chunks can render as many artificial `Agent:` lines and increase dispatcher churn.
- Prompt history is appended indefinitely and is never recalled or persisted (`plugins/agent.hk:24-30,79-89`), contrary to the bounded-history contract in `docs/AGENT_WORKFLOW.md:75-79`.

Plan:

1. Emit a bounded/coalesced proposal-change event on ACP writes and turn completion, filtered by session. Test an open review receiving a new hunk without reopen plus accept/reject/rebase transitions.
2. Store proposal workspaces per ACP session/root, or reject a root change before starting the second session. Test two independent temp roots.
3. Coalesce consecutive agent chunks into messages, preserve Unicode/newlines, bound the live view, and persist on debounce/completion. Add split-token, multiline, cancellation, and restart tests.
4. Add a small persistent, deduplicated prompt-history MRU with keyboard recall and tests for paste, wrapping, many prompts, reload, and restart.

Exit criterion: review and conversation UI reflect live agent activity without reopen, malformed chunking, unbounded state, or cross-root failures.

#### F10. Git configuration and example/plugin documentation are misleading or broken (S/M)

Evidence:

- README and defaults publish `[plugin_config.git.signs_staged]` (`README.md:242-246`, `default_config.toml:304-318`), while `plugins/git.hk:181-182` reads `options.staged_signs`. Every staged-glyph override is silently ignored.
- `examples/example-plugin/package.json` is currently invalid JSON in the working tree. The pinned example test loads `index.hk` directly (`src/plugin/runtime.rs:1531-1535`) and never exercises directory/metadata loading; `.github/workflows/plugin-check.yml:4-27` omits `examples/**`.
- README's bundled-plugin table (`README.md:281-290`) and `docs/PLUGIN_SYSTEM.md:86-95` describe rich plugins as placeholders/early ports even though Git, breadcrumbs, progress, and other behavior now exists. `docs/PLUGIN_SYSTEM_IMPROVEMENTS.md:18-36` still claims `buffer:changed` is unimplemented despite the production notification in `src/editor.rs:11957-11960`.
- Legacy Deno/JS/TypeScript instructions and ten JS/TS examples remain under `types/`, `test-harness/`, and `examples/`, contradicting the Husk-only runtime and the cleanup called out in `docs/PROJECT_PLAN.md:97-103`.

Plan:

1. Read `signs_staged`, optionally accept the old alias for compatibility, and test a visibly non-default staged glyph.
2. Confirm the intended existing manifest edit, restore a valid metadata file, add an end-to-end directory/manifest load test, and include `examples/**` in plugin-check filters. Validate JSON in CI.
3. Update plugin capability/status documentation from the actual bundled corpus. Archive/remove obsolete JS/types/harness material or clearly mark it historical so new plugin authors are routed to Husk.
4. Refresh `docs/PROJECT_PLAN.md:51-103` baseline counts/status and reconcile `docs/VIM_COMPATIBILITY.md:57-58`, which simultaneously calls undo linear and branching. Keep historical roadmap versions clearly labeled.

Exit criterion: default/configured Git behavior matches docs, examples load as packages, and every public plugin/roadmap statement matches the shipped runtime.

#### F11. Published Vim behavior is contradicted by operator counts and `zz` (M)

Evidence:

- The compatibility matrix marks counts, operators, and find/till supported (`docs/VIM_COMPATIBILITY.md:15-18`), but `PendingOperator` has no count (`src/editor.rs:1911-1935`). Starting `d`, `c`, or `y` clears the repeater (`:8351-8354`), and a digit while pending is rejected (`:8339-8341,8357-8411`). `2dd`, `2yy`, `2dw`, `d2w`, `c2w`, and counted operator/find sequences cannot provide the documented result. Existing integration tests cover bare character counts and uncounted operators, not these combinations.
- The default keymap exposes `zz` (`default_config.toml:142`), but `MoveLineToViewportCenter` has reversed/unrelated guards (`src/editor.rs:9611-9635`). A cursor below center on the initial page can be a no-op, and no production-path `zz` test exists.

Plan:

1. Track operator and motion counts explicitly, multiply with saturation, and apply them consistently to line/word/find/till ranges as one transaction and one register write. Preserve count semantics through dot-repeat/macros.
2. Center using the absolute target line: compute a clamped viewport top from `vtop + cy`, recompute cursor row, and handle wrapped/multi-window layouts.
3. Add production-path tests for delete/change/yank counts before/after the operator, character motions, Unicode/end-of-buffer, dot/macro replay, and `zz` from first page/interior/EOF with wrapped and multi-window content.
4. Only promote behavior in the compatibility matrix after those tests pass. Named text registers, visual `r`, and dot after confirmed substitute remain explicit follow-up features rather than accidental claims.

Exit criterion: supported Vim rows match actual key sequences, preserve undo and register semantics, and work at viewport boundaries.

### P2 - test architecture, performance coverage, and next product features

#### F12. Current tests/performance gates miss production-state and editing/detach regressions (M)

Evidence:

- Harness tiny-size tests call `test_set_size`, which directly mutates layout and never sends `Event::Resize` (`tests/common/editor_harness.rs:360,375,379`, `src/editor.rs:14901-14904`). Event/action helpers construct fresh `RenderBuffer` and `Runtime` instances, so cached-frame, runtime-state, and sequence regressions can escape.
- The PTY performance driver disables LSP and sends only repeated `j` (`scripts/scroll_bench.py:45-48,72-84`, `docs/performance.md:43-53`). It does not measure editing, detach, plugin-process output, ACP streaming, or LSP.
- Every file-backed edit materializes the full Rope for `did_change` (`src/editor.rs:11946-11954`); incremental LSP then compares full old/new strings (`src/lsp/client.rs:369-375,702-751`). This may be acceptable for the current baseline, but it is invisible to the only enforced motion benchmark.

Plan:

1. Build a persistent production-session harness that retains runtime/frame, feeds actual key/paste/mouse/focus/resize events and bursts, supports detach/reattach, and exposes both cell-grid and terminal-byte assertions.
2. Add deterministic correctness fixtures for dialogs, plugins, background updates, Unicode widths, tiny panes, multiple windows, ACP writes, and Git staging. Keep slow real-PTY/SSH checks separate from fast integration tests.
3. Add pre-release edit+LSP and detached benchmarks: large file, single-key and paste latency, output bytes, idle render count, streaming updates, and p95. Establish a baseline before optimizing Rope/LSP synchronization.

Exit criterion: the paths implicated in this sweep have production-state regressions and measurable latency/output budgets rather than isolated helper tests.

#### F13. Prioritize a reviewable production adapter and LSP daily-driver actions (L/Spike)

Evidence and opportunity:

- The built-in ACP registry contains only the development fixture, and the audited Codex adapter bypasses Red's reviewable client filesystem (`docs/AGENT_WORKFLOW.md:20-25`). This is the principal gap between an impressive foundation and a safe, usable agent workflow.
- LSP client methods/capabilities exist for formatting, code actions, and signature help, but editor actions/keymaps do not expose the full workflow; rename and general multi-file `WorkspaceEdit` application are absent. The original plan correctly calls out URI-aware, UTF-16-correct edits, resource operations, rollback, stale responses, and format-on-save rules (`docs/PROJECT_PLAN.md:223-243`).

Plan:

1. **Adapter spike:** identify one maintainable ACP adapter/version that routes reads and writes through Red. Run the live conformance fixture, unsaved buffer/read-after-write, multi-file proposal, conflict/rebase, accept/reject, cancellation, dropped-SSH/detach, and `disable_ai` scenarios before adding it to the supported registry. Do not weaken proposal isolation to accommodate an adapter that edits disk directly.
2. **LSP foundation:** implement a reusable single-line prompt and an atomic, URI-aware multi-buffer `WorkspaceEdit` applicator with UTF-16 conversion, stale-version checks, resource-operation policy, attribution, and rollback.
3. Add rename, code-action picker, formatting/format-on-save, and signature-help UX on that foundation. Cover multiple buffers, Unicode, dirty files, failed partial edits, server-initiated `workspace/applyEdit`, and stale responses.
4. Keep Windows detach/named pipes, collaboration/TCP attach, DAP, and multi-cursor out of this sequence unless user evidence changes priorities.

Exit criterion: a fresh-profile SSH user can safely complete a real reviewed agent turn, and common rename/code-action/format workflows behave atomically on real language servers.

## Recommended implementation sequence and PR boundaries

Keep each numbered item independently reviewable and land in dependency order. The estimated size is a planning aid, not a delivery promise.

1. **PR 1 - CI/release truth (S):** F01 plus manifest validation/filtering from F10. Fix duplicate YAML, packaged self-check assertions, release runbook, Markdown links, and MSRV decision. This restores trustworthy gates first.
2. **PR 2 - exact plugin buffer reads and Git config (M):** F02 and the staged sign-key fix from F10. Add exact-byte/range and unsaved-hunk regressions.
3. **PR 3 - terminal correctness (M):** F03, retaining the existing cached-row truncation. Land clamped resize, shared Unicode row encoding, wrap handling, dialog/plugin resize parity, and PTY tests before performance changes.
4. **PR 4 - detach protocol and resource safety (M):** framing, handshake, dimensions, large paste, lock scope, and mouse from F05/F06. Bump protocol version only if required and document migration/reconnect behavior.
5. **PR 5 - ACP failure isolation (M):** F04 and ACP portions of F05. Add adversarial/live-fixture coverage before changing prompt lifecycle.
6. **PR 6 - durable sessions (S/M):** F07. Fail closed, secure temp creation, fault tests, and visible warnings.
7. **PR 7 - plugin compatibility contract (M):** F08 plus stale plugin/docs cleanup from F10. Reconcile signatures, semver/collisions, staging effects, package load, and generated parity checks.
8. **PR 8 - live agent and detach responsiveness (M):** F09 and dirty-render, delta-paint, update-delivery, and instrumentation work from F06/F12.
9. **PR 9 - Vim correctness (M):** F11 and matrix/dogfood updates. Counts and `zz` should land with production-path tests before optional Vim features.
10. **PR 10+ - product tracks (L/Spike):** F13. Start the adapter-conformance spike and LSP `WorkspaceEdit` foundation independently; sequence UI actions only after their safety/atomicity contracts are green.

The terminal and ACP/session tracks can be developed in parallel once PRs 1-2 establish trustworthy validation, but changes to `src/editor.rs`, detach serialization, and the plugin schema should be rebased deliberately to avoid conflicting contract decisions.

## Validation matrix

Run focused suites while implementing each PR, then the complete local gate:

```text
python3 scripts/repository_inventory.py
python3 -m json.tool examples/example-plugin/package.json
actionlint .github/workflows/*.yml
cargo fmt --all -- --check
cargo test --all-targets --all-features
cargo test --test acp_conformance --all-features
cargo test --test detach --all-features
cargo clippy --all-targets --all-features -- -D warnings
target/debug/red --self-check
target/debug/red --agent-check
python3 scripts/detach_bench.py 50 120 120 1536
cargo run --locked --release --example husk_cursor_bench -- --assert
git diff --check
```

Add these production-path scenarios to the release checklist:

1. Native and detached tmux sessions repeatedly shrink/grow horizontally and vertically with CJK, emoji, ZWJ/combining text, dialogs, overlays, and two windows; no exit, wrap, or stale frame occurs.
2. A detached owner receives input, mouse, large paste, and background updates; a stalled/malformed client cannot block progress, and reconnect succeeds.
3. ACP denies an outside-root request, continues a long prompt, streams text, writes two proposal roots, updates an open review, and survives cancellation and reattach without changing unaccepted disk/buffer content.
4. A dirty file stages an exact Git hunk, custom staged glyphs render, and plugin reload failure leaves the previous plugin plus its resources intact.
5. Crash/fault injection preserves an owner-only, loadable previous snapshot; unknown future snapshots are never overwritten.
6. Vim operator counts, dot/macro replay, and `zz` pass first-page/interior/EOF, Unicode, wrapped, and multi-window cases.
7. Packaged binaries on Linux, macOS, and Windows pass the updated self-check output contract and contain no build-workspace path.

## Completion criteria for the sweep plan

The foundation is ready for a broader external dogfood once:

- CI/release/package gates are trustworthy and documentation describes the actual runtime;
- terminal and detach production-path regressions are green;
- ACP/IPC/subprocess resource and failure boundaries are enforced;
- exact buffer, proposal, snapshot, and plugin contracts preserve user data;
- published Vim compatibility rows match real key sequences; and
- at least one production adapter passes the reviewable-filesystem gate.

The local implementation meets these criteria: the production adapter is reviewable and conformance-tested, the documented Vim subset has production-path regressions, terminal/detach stress and reattach pass, and the release gates are green. Keep unsupported Vim operations, non-Unix filesystem resource edits, and Windows packaging explicitly qualified until their dedicated platform gates run. This preserves the strongest part of the project plan: safety and behavior are demonstrated, not inferred from a large feature diff or a green but incomplete unit suite.
