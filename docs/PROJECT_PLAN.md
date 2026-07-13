# Red Project Plan — "The Terminal Editor for the Agent Era"

**Status:** Draft v2 · July 2026

**Positioning:** *Real Vim keys. Zero Red config. Agents built in. Review every change.*

This plan turns Red's differentiation strategy into a foundation phase followed by four
product phases. Each phase has one demonstrable outcome, explicit prerequisites, and an
exit criterion that can be tested. It intentionally distinguishes product guarantees
from aspirations.

## Product contracts

The following terms have precise meanings throughout this plan:

- **Zero Red config** means no Red configuration file is required. For the first agent
  release, a supported agent or adapter may still need to be installed, available on
  `PATH`, and authenticated. `red --agent-check` must explain missing prerequisites and
  never install or download software without consent.
- **Reviewable agent edits** means an agent can work against proposed file contents while
  the user-visible buffer remains unchanged until acceptance. Saving a buffer never
  writes an unaccepted proposal.
- **Plugin compatibility** means incompatible calls to Red's typed host API are rejected
  before activation, the plugin is quarantined, and the editor continues to start. It
  does not promise that arbitrary plugin logic is bug-free.
- **Native ACP** means Red owns the protocol client and buffer integration. The agent UI
  may remain a bundled Husk plugin connected through a versioned host API.
- **Cross-platform** continues to mean Linux, macOS, and Windows for the editor. Any
  phase-specific platform limitation must be explicit in its exit criterion.

## Strategic sequence

1. **Foundation and discovery.** Establish a replayable edit boundary and retire stale
   documentation before building features on incorrect assumptions. Prototype the two
   riskiest seams—ACP proposals and detach—before committing to their schedules.
2. **Vim credibility.** Close the first-hour muscle-memory gaps. A user who reaches for
   `.` or a macro must not discover that the compatibility claim is aspirational.
3. **Agent-native workflow.** Deliver the switching experience: an existing agent works
   against Red buffers, proposals are reviewable, and the workflow works over SSH with
   no Red configuration.
4. **Resilient sessions and attribution.** Preserve unsaved and agent work across crashes
   and disconnects, then make each accepted change auditable and selectively reversible.
5. **Typed plugin compatibility.** Reject incompatible host-API calls before activation
   while keeping the editor and unrelated plugins running.

## Current repository baseline

This baseline is intentionally concrete and should be regenerated at each phase exit with
`python3 scripts/repository_inventory.py`.

- `src/editor.rs` is approximately 18.7k lines. `Action` currently has 168 variants,
  `PluginRequest` has 71 variants, and production execution runs through `execute` /
  `execute_with_tracking`; there is no current `apply_action_core` function.
- The full test suite is green as of this revision. Phase 0 preserves that baseline
  rather than treating test recovery as unfinished work.
- Editing uses per-buffer branching `UndoHistory` and attributed `EditTransaction`
  records. Transactions, undo trees, marks, registers, and jumplist state are
  serializable for recovery.
- Core session recovery snapshots open buffers and unsaved contents, window layout,
  cursors, registers, marks, jumplist, attributed undo trees, agent transcript, and
  pending proposals. Restored dirty contents remain in memory until an explicit save.
- The Husk VM parses, resolves names, and typechecks plugins with `husk-semantic` and
  the generated host declarations before activation; diagnostics retain source spans.
- Plugin metadata supports `red_api_version` and dependency requirements. Incompatible
  or malformed plugins are quarantined without preventing unrelated plugins or editor
  startup; transactional reload retains the previous program on failure.
- Thirteen bundled `.hk` plugins ship. `git.hk` is roughly 69 KB / 2k lines and is a
  useful UI/process reference, but not a reusable diff engine by itself.
- Formatting, code actions, signature help, and rename are exposed through default
  keymaps. Ordered `WorkspaceEdit` changes cover open and unopened files with URI,
  UTF-16, revision/version, workspace-root, and symlink validation. Regular-file
  create/rename/delete operations have bounded snapshots and rollback; unsupported
  recursive or open-buffer deletes fail closed.

## Phase overview

Estimates are focused engineer-weeks for one full-time developer. Parallel tracks reduce
calendar time only when a contributor independently owns one; the base schedule assumes
the primary developer works sequentially.

| Phase | Headline outcome | Est. effort | Indicative window |
|-------|------------------|-------------|-------------------|
| 0. Foundation & spikes | Current docs, replayable edit boundary, ACP and detach risks tested | 3–5 wk | Q3 2026 |
| 1. Vim credibility | A Vim-native user completes a week of real work without a release-blocking compatibility failure | 8–11 wk | Q3–Q4 2026 |
| 2. Agent-native | A supported agent edits proposed buffer state with reviewable diffs over SSH and no Red config | 13–17 wk | Q4 2026–Q1 2027 |
| 3. Sessions & attribution | Unsaved work survives a crash; SSH can disconnect while an agent task keeps running | 14–21 wk | Q1–Q4 2027 |
| 4. Plugin compatibility | An incompatible plugin is diagnosed and quarantined without preventing editor startup | 10–14 wk | Q2–Q4 2027 |

Phase 1 is the public-launch credibility gate. Phase 2 implementation may begin after the
Phase 0 ACP spike only if independently owned; otherwise it follows Phase 1. The launch
always waits for Phase 1. Phase 3 attribution foundations are introduced while Phase 2
applies agent edits. Phase 4 may overlap only when independently owned; it is not treated
as free parallelism in the one-developer schedule.

---

## Phase 0 — Foundation and architecture spikes (3–5 weeks)

**Goal:** replace stale assumptions with tested seams that later phases can build on.

### 0.1 Repository and documentation truth pass

- Rewrite README feature bullets around Husk; remove the removed Deno/JS runtime claims.
- Mark, archive, or rewrite stale plans, including `docs/HOT_RELOAD_PLAN.md`,
  `docs/PLUGIN_SYSTEM_IMPROVEMENTS.md`, and obsolete Unicode references.
- Delete dead legacy `.js` plugin files after confirming they are not referenced by
  packaging or release automation.
- Replace line-count/API-count prose with a small reproducible inventory script or a
  dated appendix. Do not use source line numbers as enum-variant counts.

### 0.2 Canonical input, action, and edit boundary

- The maintained production-path contract lives in
  [`docs/EDIT_PIPELINE.md`](EDIT_PIPELINE.md).
- Document the current path from terminal event → key resolution → action → transaction
  → buffer mutation → render/plugin/LSP side effects.
- Establish one production path that tests can drive. The design may refactor
  `execute_with_tracking`; it does not depend on preserving the stale
  `apply_action_core` name.
- Separate three concepts explicitly:
  - replayable input events, required by macros;
  - repeatable semantic changes, required by dot-repeat;
  - recorded buffer edits, required by undo, attribution, marks, and agent review.
- Route all content mutation through a documented edit notification point so anchors,
  dirty state, LSP synchronization, undo, and future proposal rebasing cannot drift.
- Add invariant tests: cursor positions remain valid, a committed edit is undoable, and
  replay does not bypass normal mode/count/pending-key behavior.

### 0.3 ACP vertical-slice spike

- Pin a protocol schema version and generate or validate Rust protocol types from the
  official ACP schema.
- Test the initial adapter against recorded sessions and a live conformance fixture:
  initialize, `session/new`, prompt, streaming update, `fs/read_text_file`,
  `fs/write_text_file`, permission request, cancellation, and shutdown.
- Verify that the chosen launch adapter actually uses client filesystem methods for the
  edits shown in the demo. An agent that writes directly through its own process cannot
  provide Red-controlled pending review without a different integration strategy.
- Build the smallest core↔Husk bridge: create one session, submit one prompt from a
  plugin, stream one update back, and cancel it.
- Decide and document adapter discovery, installation, version pinning, authentication,
  and offline behavior.

### 0.4 Detach feasibility spike

- Inventory everything currently owned by `Editor`: terminal/stdout, input polling,
  buffers, windows, rendering, plugins, LSP, and process lifetimes.
- Prototype a headless owner that accepts one input event and returns one render delta
  across local IPC.
- Write an ADR covering process ownership, protocol versioning, reconnect behavior,
  backpressure, cleanup, crash behavior, and the Windows transport decision.
- Re-estimate Phase 3 B.2 from the spike rather than treating `RenderBuffer` as an
  already-existing client/server boundary.

### 0.5 Baselines and release gates

- Record RED_PERF startup, keypress-to-render, large-file scroll, and plugin-startup
  baselines with machine/build metadata.
- Put deterministic benchmarks in CI where stable; keep noisy wall-clock comparisons in
  a documented pre-release runbook with an allowed regression threshold.
- Keep `cargo test --all-targets --all-features` green.
- Run `cargo clippy --all-targets --all-features -- -D warnings` before pushing Rust
  changes.

**Exit criterion:** README and maintained docs match the shipped runtime; the test suite
and clippy pass; performance baselines are recorded; the edit-boundary design is covered
by tests; ACP and detach ADRs contain working vertical slices and revised estimates.

---

## Phase 1 — Vim credibility (8–11 weeks)

**Goal:** a Vim-native user's normal editing loop is predictable enough that the agent
workflow gets a fair evaluation.

### 1.1 Dot-repeat (`.`) — 2–3 wk

- Record the last completed semantic change, including count, register, operator/motion
  or text object, inserted text, and entry/exit mode behavior.
- Replay through the normal edit path without recording the replay as a new definition.
- Cover operator+motion, operator+text-object, insert sessions, paste, replace, indent,
  open-line, and visual-block insert.
- Specify behavior for failed/no-op changes and counts applied to `.`.

### 1.2 Macros (`q`/`@`) — 2–3 wk

- Record normalized key events into named registers and replay them through the same
  input/key-resolution pipeline used for interactive input.
- Support `@@`, count prefixes such as `3@a`, inspection/editing through registers, and
  deterministic recursion/instruction limits.
- Define what is recorded for mouse, paste, resize, plugin, and asynchronous LSP events;
  non-key background events must not make a macro nondeterministic.

### 1.3 Marks and edit anchors — 1.5–2 wk

- Implement per-buffer marks (`a-z`), global marks (`A-Z`), previous-jump, last-change,
  and last-visual marks.
- Store anchors in character coordinates with a defined insertion affinity and update
  them at the canonical edit boundary, including undo/redo and multi-edit transactions.
- Integrate mark jumps with the jumplist and define behavior when a marked buffer or file
  disappears.

### 1.4 Substitute command — 2–2.5 wk

- Support current-line, `%`, numeric, and last-visual ranges.
- Support Rust-regex patterns with documented Vim differences and `g`, `i`, and `c`
  flags.
- Implement confirmation as an explicit state machine; do not overload search mode if it
  makes cancellation, rendering, or undo behavior ambiguous.
- Apply all accepted replacements from one command as one `EditTransaction`.

### 1.5 Vim compatibility matrix and dogfood

Create a versioned checklist in the repository rather than relying on an open-ended
promise that every Vim behavior works. It must cover:

- counts, operators, motions, and supported text objects;
- registers, yank/delete/change/paste, macros, and dot-repeat;
- insert, normal, visual, visual-line, and visual-block transitions;
- search, substitution, command-line cancellation, undo/redo, marks, and jumplist;
- Unicode graphemes, empty buffers, final lines, wrapped lines, and multi-window use.

Each unsupported Vim behavior is labeled **supported**, **intentional difference**, or
**not yet supported**. “Real Vim keys” refers to this published matrix, not complete Vim
emulation.

### Launch-quality LSP track — implemented foundation, not part of Vim scope

Rename, code actions, formatting, and signature help use the same production edit path.
The foundation now provides:

- a reusable, grapheme-aware single-line prompt for rename with replacement-on-type and
  safe paste/cancel behavior;
- URI-aware, UTF-16-correct, revision/version-checked multi-buffer application for
  rename, code actions, formatting, and server-initiated `workspace/applyEdit`;
- ordered regular-file create/rename/delete operations inside the originating workspace,
  with handle-relative no-follow checks, protected-path denial, per-file and aggregate
  memory limits, race detection, and filesystem rollback before any buffer mutation;
- unopened text targets loaded as attributed dirty buffers rather than silently written
  to disk, preserving the normal explicit-save contract;
- optional `format_on_save` that waits for formatting, prevents duplicate in-flight
  requests/recursion, and cancels the save when formatting is invalid or stale.

Directory/recursive deletion, overwriting or deleting open buffers, non-UTF-8 text
edits, confirmation-required change annotations, and edits outside the selected
language-server workspace are intentionally rejected. Resource-only renames update LSP
document identity with `didClose`/`didOpen`. Secondary open targets retain the revision
snapshot captured when a rename or code action was requested, so delayed unversioned
responses cannot overwrite intervening edits.

Application is atomic at the edit boundary and resource failures roll back when safe,
but undo is intentionally per buffer; resource create/rename/delete operations are not
part of buffer undo history.

Future LSP work can extend this policy for directory operations or richer annotations
without introducing a second mutation path.

**Exit criterion:** the compatibility matrix is green for all supported rows; two
Vim-native external testers use Red on real work for one week; no unresolved issue marked
“release-blocking compatibility” remains; all failures and intentional differences are
recorded rather than summarized as “zero reports.”

---

## Phase 2 — Native ACP and reviewable agent workspace (13–17 weeks)

**Goal:** a supported agent works against Red's current buffer state, proposed edits are
reviewed before becoming user edits, and the workflow runs over SSH without a Red config
file.

ACP support alone is not the differentiator. The product claim is the combined workflow:
unsaved-buffer context, proposal isolation, review, attribution, terminal ergonomics, and
an explicit off switch. Revalidate the editor/agent landscape at phase kickoff rather
than publishing a brittle “no other editor” claim.

### Architecture

- **Protocol core in Rust** (`src/acp/`): versioned JSON-RPC transport, process lifecycle,
  capability negotiation, sessions, requests, notifications, cancellation, and client
  filesystem/terminal methods.
- **Versioned agent host API:** core requests/actions and events exposed to Husk for
  session creation, prompting, streaming updates, permissions, proposal review, and
  cancellation.
- **Bundled Husk UI** (`plugins/agent.hk`): conversation panel, prompt composer, progress,
  session selection, permissions, and review workspace. Existing panel, overlay,
  decoration, gutter, workspace, and picker APIs are starting points, not proof that a
  prompt/composer or diff-review interaction already exists.

### 2.1 Transport, lifecycle, and conformance — 3–4 wk

- Implement initialize/authentication, session creation, prompt, streaming updates,
  permission requests, cancellation, session close, and process shutdown.
- Negotiate optional capabilities rather than assuming session load/resume or terminal
  support.
- Bound queues and output retained in memory; a noisy agent must not block input/render.
- Test protocol behavior against recorded fixtures and the pinned live adapter version.
- Surface adapter exit, malformed messages, timeout, and version mismatch as recoverable
  session errors.

### 2.2 Proposal filesystem and consistency model — 3–4 wk

For each agent session and file, track:

- the visible-buffer revision and contents used as the proposal base;
- proposed contents visible to the agent;
- pending hunks between base/current buffer and proposed contents;
- accepted/rejected state and attribution metadata.

Rules for v1:

- The first agent read starts from the latest Red buffer, including unsaved edits.
- Agent filesystem writes update proposed contents and return success; later agent reads
  observe those proposed contents.
- User-visible buffers and disk remain unchanged until acceptance.
- Each proposal records its base revision. If the user edits that buffer before
  acceptance, Red performs a three-way rebase when clean and opens a conflict review when
  not; it never silently applies stale ranges.
- Rejecting all proposals resets the agent-visible file to the current Red buffer.
- Saving writes only accepted buffer contents.
- File creation, deletion, rename, files outside the workspace, symlinks, and path
  normalization have explicit permission and review rules.

Build property/integration tests for read-after-write, user-edit divergence, partial
acceptance, rejection, save-before-accept, multiple files, Unicode, and process restart.

### 2.3 Diff review and attributed application — 3–4 wk

- Render pending hunks with decorations and gutter signs without mutating the buffer.
- Support accept/reject per hunk, file, and session, plus a full-screen multi-file review.
- Introduce `EditOrigin` now. Accepted edits become `EditTransaction`s tagged with agent
  session and turn identifiers; Phase 3 later generalizes the review/history UI.
- Define partial-hunk acceptance and how remaining proposals are rebased.
- Keep review navigation and commands usable without a mouse and document all keys.

### 2.4 Agent UI and permissions — 2–3 wk

- Add a focused prompt/composer primitive with history, cancellation, paste, wrapping,
  and clear keyboard ownership.
- Stream text and tool-call updates into a bounded conversation model; virtualize or
  truncate old rendered rows without losing persisted transcript data.
- Present ACP-provided permission options and return their exact option IDs. Any
  allow-session policy must be an explicit Red policy layered on top, scoped by session,
  normalized resource, and tool kind.
- Never treat the plugin process allowlist as sufficient authorization for ACP tools;
  reuse its normalization/audit ideas while keeping agent grants separate.

### 2.5 Distribution, off switch, and launch — 2 wk

- Ship a built-in registry of tested adapter commands and versions while allowing custom
  agents through configuration.
- Add `red --agent-check` showing adapter presence/version, authentication readiness,
  protocol compatibility, and remediation without exposing secrets.
- Define the first-launch prerequisite honestly: no Red config, with a supported adapter
  or agent already installed and authenticated. If one-binary adapter distribution is
  later desired, treat licensing, updates, signatures, and offline behavior as a separate
  release project.
- `disable_ai = true` prevents agent plugin activation, process spawning, adapter checks,
  and agent-initiated network access. Cover this with an integration test.
- Record the SSH demo on a clean Red profile and show prerequisite verification rather
  than hiding it.

**Exit criterion:** on a fresh Red profile over real SSH, an external tester runs
`red --agent-check`, starts a supported pre-authenticated agent, gives it unsaved buffer
context, reviews a multi-file proposal, accepts one hunk, rejects another, and verifies
that unaccepted content never reached disk. The same test verifies that
`disable_ai = true` spawns no agent process and exposes no agent UI.

---

## Phase 3 — Attributed history and resilient sessions (14–21 weeks)

**Goal:** agent edits are auditable, unsaved work survives a crash, and a dropped SSH
connection does not terminate an in-progress agent task.

### Track A — Attributed history and selective revert (4–6 wk)

#### A.1 Generalize transaction origins

- Extend the Phase 2 `EditOrigin` to `User`, `Agent { session, turn }`,
  `Plugin { name }`, and `Lsp { server }`, plus timestamp and stable transaction ID.
- Define attribution for composite edits and plugin-triggered core actions; do not infer
  an origin from whichever UI happened to be focused.
- Expose read-only origin/history data to plugins through a versioned API.

#### A.2 Change-review UI and selective revert

- List attributed transactions by session/turn, jump to locations, and show a visual
  before/after diff.
- Reverting the tip transaction is exact.
- Reverting an older transaction is allowed only when affected content still matches the
  transaction's post-image. Otherwise open a conflict review; never apply stale inverse
  ranges optimistically.
- A revert is itself a new attributed transaction, preserving audit history.

#### A.3 Undo tree

- Branch on undo+edit instead of truncating redo history.
- Provide deterministic branch and time traversal with a small visual navigator.
- Keep persistent per-file undo as optional until snapshot format and privacy rules have
  proven stable.

### Track B.1 — Crash-safe session persistence (4–6 wk)

- Version and serialize open buffers, unsaved contents, file identity, window layout,
  cursors, jumplist, marks, registers, undo tree, transaction origins, agent transcript,
  and pending proposals.
- Write snapshots atomically with restrictive permissions. Use a temp file, flush, and
  atomic replacement where supported; retain the last known-good generation.
- Snapshot periodically, on material state transitions, and on clean exit. Debounce and
  measure snapshot overhead.
- Restore dirty buffers without overwriting newer disk contents. Show a recovery diff
  when disk identity/mtime/content has diverged.
- Add schema migrations and fail safely on unknown future versions.
- Resume an ACP session only when the agent advertises `session/load` or
  `session/resume`. Otherwise restore the transcript as archived context and start a new
  session only with user confirmation.

**B.1 exit criterion:** kill Red without running exit hooks while two buffers contain
unsaved changes and one agent proposal is pending; `red --resume` restores all state
without changing disk, reports any external file divergence, and passes repeated
crash-during-snapshot fault tests.

### Track B.2 — Detach/reattach (6–9 wk after the Phase 0 spike)

- A headless core process owns buffers, persistence, LSP, plugin VMs, and agent processes.
- A thin TUI client owns terminal setup/input and exchanges versioned input/render/control
  messages with the core over local IPC.
- Define heartbeats, reconnect tokens, stale-client eviction, terminal resize/focus
  reconciliation, socket/pipe permissions, and server cleanup.
- Keep one attached client per session in v1. Multiple clients, TCP attach, and
  collaboration remain non-goals.
- Target Unix sockets on Linux/macOS. Either implement named pipes on Windows in this
  phase or explicitly document that Windows receives B.1 resume but not B.2 detach in the
  first release.

**Phase exit criterion:** start a long agent task over SSH, terminate the SSH connection,
reattach from a new terminal, and confirm the original agent process is still running.
Completed edits appear with attribution, a clean older change can be selectively
reverted, and a conflicting revert opens review rather than changing text silently.

---

## Phase 4 — Typed plugin compatibility contract (10–14 weeks)

**Goal:** an incompatible plugin fails before activation, is quarantined with an
actionable diagnostic, and never prevents Red or unrelated plugins from starting.

### 4.1 Canonical typed host API — 2–3 wk

- Define one machine-readable schema for Red execute actions, request actions, events,
  snapshots, callbacks, and stdlib functions.
- Generate or mechanically validate Rust dispatch, Husk declarations, and API reference
  documentation from that schema so counts and signatures cannot drift independently.
- Model asynchronous callbacks and JSON-shaped payloads honestly; avoid declaring
  everything as an unstructured `Json` escape hatch merely to make bundled plugins pass.

### 4.2 Plugin fault isolation — 1.5–2 wk

- Load and activate plugins independently. A parse, type, activation, or runtime error
  quarantines only that plugin.
- Continue editor startup and load unrelated plugins when one fails.
- Present diagnostics with plugin name/path and actions to open source, disable, retry,
  or view migration guidance.
- Track plugin status in `red --self-check` and make required dependencies fail with a
  clear dependency-chain diagnostic.

Fault isolation precedes mandatory type checking; otherwise the promised compile error
can become an editor startup failure.

### 4.3 Semantic checking on load — 3–4 wk

- Run parse → resolve → typecheck against the canonical host declarations before
  activation.
- Preserve source spans and stable error codes. Runtime call frames remain runtime
  diagnostics; compile-time errors should not claim a call stack that does not exist.
- Typecheck all bundled plugins and a pinned community corpus in CI.
- Provide `--no-typecheck` only as an explicitly unsupported development escape hatch.
  Compatibility guarantees do not apply while it is enabled.

### 4.4 API versioning and migration data — 1.5–2 wk

- Enforce the existing `red_api_version` metadata field with real semver range handling.
- Publish what may change in patch, minor, and major editor releases while Red remains
  pre-1.0.
- Maintain a machine-readable API change manifest containing introduced, deprecated,
  changed, and removed versions plus migration-note links. This is what enables a
  diagnostic to say which Red release changed a signature.
- Test every release candidate against bundled plugins and the pinned community corpus.

### 4.5 Transactional hot reload — 2–3 wk

- Watch user plugin sources with debouncing and normalized paths.
- Parse/typecheck/activate the replacement in isolation; swap it into the live registry
  only after all preconditions succeed. A bad save leaves the previous plugin running.
- Replace implicit `state_*` preservation with an explicit versioned state export/import
  or migration hook. If migration fails, retain the previous plugin and state.
- Clean up old callbacks, processes, timers, watchers, panels, decorations, and pending
  requests on a successful swap.

### 4.6 Release discipline

- Publish a sustainable target cadence, with a smaller release considered valid.
- For each release, run tests/clippy, platform self-checks, performance gates, bundled and
  corpus plugin typechecks, snapshot migration tests, and adapter conformance tests.
- Include API and state-schema migration notes in the changelog.

**Exit criterion:** install a deliberately outdated plugin and upgrade Red across a
breaking host-API change. Red starts normally, quarantines only that plugin, and shows a
span-annotated diagnostic naming the changed API, release, and migration note. After the
plugin is fixed and saved, transactional hot reload activates it without restarting Red;
an intentionally broken follow-up save leaves the working version active.

---

## Cross-cutting positioning and communications

- **Before Phase 1 exit:** publish implementation/devlog material and recruit external
  Vim-native testers. Avoid categorical compatibility claims before the matrix exists.
- **Phase 2 launch:** lead with the workflow, not protocol ownership: unsaved buffers,
  review-before-apply, SSH, zero Red config, and an explicit off switch. State adapter and
  authentication prerequisites plainly.
- **Phase 3:** demonstrate crash recovery first, then dropped-SSH detach and attributed
  selective revert.
- **Phase 4:** describe a typed compatibility boundary and plugin quarantine, not the
  impossible promise that no setup can ever break.
- Revalidate comparison-page claims at each release. ACP clients, adapters, and competing
  editor capabilities change too quickly for dated assertions to live indefinitely in
  the roadmap.

## Risks and mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Chosen agent bypasses ACP client filesystem methods | Medium | High | Adapter conformance test in Phase 0; support only integrations whose edit path Red can review |
| Proposal state diverges from user edits | High | High | Revisioned bases, three-way rebase, explicit conflict review, property tests |
| ACP protocol/adapter churn | High | Medium | Pin schema and adapter versions, capability negotiation, generated/validated types, recorded fixtures |
| “Zero config” hides installation/auth prerequisites | High | High | Precise product contract, `red --agent-check`, honest launch demo |
| Plugin UI lacks composer/review primitives | Medium | High | Phase 0 vertical slice; budget explicit host and UI APIs in Phase 2 |
| Detach scope exceeds estimate | High | High | Phase 0 IPC spike and ADR; re-estimate before commitment; B.1 ships independently |
| Snapshot bug causes data loss or leaks source | Medium | High | Atomic generations, restrictive permissions, fault injection, restore diff, schema migration tests |
| Type checker cannot model real bundled plugins | Medium | Medium | Audit during Phase 2, canonical host schema, staged enforcement after fault isolation |
| Solo-developer schedule assumes fictional parallelism | High | Medium | Sequential base schedule; overlap only with named independent owners; cut scope at phase gates |
| Agent feature backlash | Medium | Medium | Optional bundled UI, inert `disable_ai`, no hidden downloads/network, workflow-first positioning |

## Success measures

- **Phase 0:** green tests/clippy; reproducible baseline inventory; ACP and detach vertical
  slices complete; revised estimates recorded.
- **Phase 1:** supported compatibility matrix passes; two external one-week trials; zero
  unresolved release-blocking compatibility issues.
- **Phase 2:** unaided external SSH reproduction; median time from opening Red to first
  prompt recorded; no unaccepted proposal reaches a buffer transaction or disk; off-switch
  integration test passes.
- **Phase 3:** crash fault suite restores unsaved buffers and proposals; detach survives a
  real dropped SSH connection; selective revert never silently applies a stale inverse.
- **Phase 4:** bundled and pinned community corpus typecheck; incompatible plugins are
  quarantined without startup failure; three consecutive releases honor the published
  compatibility policy and cadence.

GitHub stars, launch-post ranking, and absence of bug reports are useful context but are
not phase gates; each can look healthy without proving product behavior.

## Non-goals

- Matching Neovim's plugin ecosystem by volume.
- Multi-cursor editing before sustained user demand justifies a second editing model.
- Collaboration/multiplayer or remote TCP attach.
- A GUI before the terminal/SSH workflow succeeds.
- DAP/debugger integration before the four differentiating phases complete.
- Silent agent/adapter installation, hidden network access, or mandatory AI UI.
- A guarantee that typed third-party plugin logic can never contain bugs.

## Decisions that must remain explicit

Record these in ADRs and revisit only with new evidence:

1. Supported ACP schema and adapter versions.
2. Agent installation/authentication contract for “zero Red config.”
3. Proposal filesystem and conflict behavior.
4. Plugin/core ownership of agent UI state and persisted transcripts.
5. Snapshot location, retention, permissions, and schema migrations.
6. Unix-only versus cross-platform detach transport for the first release.
7. Plugin API semver policy while Red is pre-1.0.
