# Red Project Plan — "The Terminal Editor for the Agent Era"

**Status:** Draft v1 · July 2026
**Positioning:** *Real vim keys. Zero config. Agents built in. It doesn't break.*

This plan turns the differentiation strategy into four sequenced phases. Each phase has a
single headline outcome, concrete workstreams grounded in the current codebase, and an
exit criterion phrased as something demonstrable — not a percentage.

**Strategic logic of the sequence:**

1. **Phase 1 — Vim credibility.** Migrations happen when a categorical capability meets
   near-zero switching friction. For terminal users, friction = muscle memory. A vim user
   who hits `.` and nothing happens churns before ever seeing the agent features. This
   phase closes the gaps that break the "real vim keys" promise in the first hour.
2. **Phase 2 — The switching driver.** Native ACP (Agent Client Protocol) support: the
   categorical capability no terminal editor has. This is the benchmarkable demo that
   earns the HN post.
3. **Phase 3 — Defensibility.** Agent-attributed undo and detachable sessions — features
   that fall naturally out of red's architecture and that competitors can't copy quickly.
4. **Phase 4 — The moat.** The typed-plugin stability story ("your setup will not break
   on update"), which Neovim structurally cannot offer and which Husk uniquely enables.

**Assumptions:** ~1 full-time developer plus occasional contributors. Estimates are in
focused engineer-weeks; calendar durations assume streaming/community work continues in
parallel. Adjust proportionally for more contributors.

---

## Phase overview

| Phase | Headline outcome | Est. effort | Target window |
|-------|-----------------|-------------|---------------|
| 0. Groundwork | Refactor + doc debt paid down; foundation safe to build on | 2–3 wk | Jul 2026 |
| 1. Vim credibility | A daily-driver vim user survives a week without muscle-memory breaks | 6–9 wk | Q3 2026 |
| 2. Agent-native | Demo: Claude Code editing live in your buffer, reviewable diffs, over SSH, zero config | 8–11 wk | Q3–Q4 2026 |
| 3. Sessions & attribution | Kill the terminal; reattach; the agent task is still running and its edits are auditable | 10–14 wk | Q4 2026–Q1 2027 |
| 4. Stability moat | Editor upgrade + incompatible plugin → friendly compile error at load, not a crash | 8–10 wk | Q1–Q2 2027 |

Phases 1→2 are strictly sequential (credibility before launch). Phase 3's two tracks can
overlap with late Phase 2. Phase 4 runs partly in parallel with Phase 3 (it is mostly
compiler/tooling work, not editor-core work).

---

## Phase 0 — Groundwork (2–3 weeks)

Small but load-bearing. Everything later touches `editor.rs` (14.5k lines, 724-variant
`Action` enum) and the plugin runtime, so pay down the debt that makes those changes risky.

### Workstreams

**0.1 Finish the action-execution refactor** (`plan.md`)
- Complete the `apply_action_core` migration so production and tests share one execution
  path. Macros (1.2) and dot-repeat (1.1) both need a clean, replayable action pipeline —
  this refactor is their prerequisite.
- Restore the full editing test suite to green.

**0.2 Documentation truth pass**
- README still leads with "sandboxed Deno runtime" (line ~20); the Deno/JS runtime no
  longer exists. Rewrite the feature bullets around Husk.
- Mark or rewrite stale docs: `docs/HOT_RELOAD_PLAN.md` (targets the removed Deno
  design), `docs/PLUGIN_SYSTEM_IMPROVEMENTS.md`, `docs/unicode-handling.md` references.
- Delete dead legacy `.js` files in `plugins/` (`cool_search.js`, `indent_guides.js`) —
  the asset loader only accepts `.hk` (`src/assets.rs`).

**0.3 Baseline metrics**
- Capture RED_PERF baselines (startup, keypress-to-render, large-file scroll) so every
  later phase can show "no regression." Wire `scripts/scroll_bench.py` into CI or a
  pre-release checklist.

**Exit criterion:** test suite green; README describes the editor that actually ships;
perf baselines recorded.

---

## Phase 1 — Vim credibility + LSP surfacing (6–9 weeks)

**Goal:** a vim user's muscle memory survives. Define a written "vim gauntlet" acceptance
checklist (below) and dogfood against it.

### Workstream 1.1 — Dot-repeat (`.`) — ~1.5 wk
- Record the last *change* (action + count + register + inserted text) at the action
  dispatch layer; replay through `apply_action_core`.
- Must compose with: operator+motion, operator+text-object, insert-mode sessions
  (capture typed text as part of the change), and visual-block insert (reuse the
  `block_replay_depth` machinery in `src/editor.rs`).

### Workstream 1.2 — Macros (`q`/`@`) — ~2 wk
- Record raw key events into named registers (vim stores macros in registers — build on
  the existing `HashMap<char, Content>` register store in `src/editor.rs`).
- Replay by feeding events through the normal input pipeline (not by calling actions
  directly) so pending-key state, counts, and mode transitions behave identically.
- `@@` repeat, count prefixes (`3@a`), recursion guard with a replay-depth cap.

### Workstream 1.3 — Marks — ~1 wk
- Per-buffer marks (`m{a-z}`, `` ` ``/`'` jumps), global marks (`A-Z`) across buffers.
- Special marks: `` `` `` (previous jump), `'.` (last change), `'<`/`'>` (last visual) —
  the visual pair also unblocks `:'<,'>s` ranges in 1.4.
- Marks must shift with edits (anchor to char positions via ropey, adjust on
  `EditTransaction` apply/undo).
- Integrate with the existing jumplist (`src/editor.rs`, size 100).

### Workstream 1.4 — Substitute `:s///` — ~2 wk
- Ranges: current line, `%`, `N,M`, `'<,'>`; patterns via the existing Rust-regex search
  engine (respect `ignorecase`/`smartcase`).
- Flags: `g`, `i`, `c`. The `c` (confirm) flag needs an interactive prompt loop —
  reuse the search-mode input machinery.
- Apply as a single `EditTransaction` so one `u` undoes the whole substitute.
- Register `:s`, `:substitute` in the ex-command table (`src/editor.rs:5483` region).

### Workstream 1.5 — LSP surfacing (cheap wins) — ~2 wk
Capabilities are already declared in `src/lsp/capabilities.rs` with trait stubs in
`src/lsp/mod.rs`; each item needs an `Action` variant, a keybinding, and a UI hookup:
- **Rename** (`Space r` or `grn`): needs a single-line input prompt — the Husk stdlib
  already has `text_field`; core needs the equivalent prompt primitive.
- **Code actions** (`Space a` / `gra`): render via the existing generic picker
  (`src/ui/picker.rs`); apply `WorkspaceEdit` as one transaction.
- **Formatting**: `:format` command + optional format-on-save in `config.toml`.
- **Signature help**: render via the existing overlay primitive during insert mode.

### Workstream 1.6 — Explicit deferrals (documented, not forgotten)
- **Code folding** — large; needs tree-sitter fold queries + display-layout work.
  Defer to post-Phase-2; folding absence did not top any switching-blocker research.
- **Undo tree / persistent undo** — deferred to Phase 3 where undo is being reworked
  anyway (attribution). Keep linear undo for now.
- **Multi-cursor** — non-goal (see Non-goals).

### Vim gauntlet (exit criterion)
A written checklist, run by at least two vim-native users on real work for one week:
- `.` repeats every operator/text-object/insert change tested
- record/replay a macro across 50 lines; `3@a` works
- `ma` … `` `a `` returns exactly; marks survive edits above the mark
- `:%s/foo/bar/gc` with confirmations behaves like vim
- rename/code-action/format round-trips on a rust-analyzer project
- Zero "my fingers did X and red did Y" reports of severity "would uninstall"

---

## Phase 2 — Native ACP client + agent workspace (8–11 weeks)

**Goal:** the launch demo. *"Claude Code editing live in your buffer with reviewable
diffs, over SSH, zero config."* No terminal editor has native ACP in core (as of July
2026, Zed's editor-support page lists only nvim plugins) — this is the categorical
capability.

**Framing guardrail (from Zed's backlash):** build "a first-class surface for the agent
you already run," not "an editor with a chatbot." `disable_ai = true` in `config.toml`
is respected from day one: no agent process spawns, no UI appears, no network calls.

### Architecture decision (proposed)
Mirror the LSP split, which already works well in this codebase:
- **Protocol client in core Rust** (`src/acp/`, modeled on `src/lsp/`): JSON-RPC over
  stdio to an agent server child process (`claude-code-acp` adapter first, Gemini CLI
  adapter second). Sessions, streaming updates, tool-call events, permission requests.
- **UI as a bundled Husk plugin** (`plugins/agent.hk`), composed from existing
  primitives: `CreatePanel`/`CreateOverlay` for the conversation surface,
  `SetDecorations` for inline pending edits, `OpenWorkspace` for full-screen review
  (the 68k-line `git.hk` workspace is the pattern and parts donor — its diff/stage UI
  is exactly what hunk-by-hunk edit review needs), and the dynamic-picker API for
  session/model selection.
This split also pressure-tests the plugin API before Phase 4 makes stability promises
about it.

### Workstreams

**2.1 ACP transport + session core** — ~3 wk
- Tokio child-process management (reuse patterns from `src/lsp/` and the plugin
  `SpawnProcess` host module), JSON-RPC framing, request/response + notification
  routing into the main `tokio::select` loop.
- Session lifecycle: `session/new`, prompt submission, streamed agent output, cancel.
- Config: agent server registry in `config.toml` (command, args, env), like LSP servers.

**2.2 Editor-as-filesystem for the agent** — ~1.5 wk
- Implement ACP's `fs/read_text_file` / `fs/write_text_file` served from **buffers**,
  not disk — the agent sees unsaved changes, and agent writes land as buffer edits.
  This is the piece plugin-based integrations fake badly, and it feeds Phase 3's
  attribution directly.

**2.3 In-buffer diff review** — ~3 wk
- Agent-proposed edits render as pending hunks (decorations/virtual text + gutter
  signs), with accept/reject per hunk, per file, or all.
- Accepted hunks apply as `EditTransaction`s (tagged `origin: agent` — forward-compatible
  with Phase 3).
- Full-screen review workspace for multi-file changes (adapt `git.hk` diff view).

**2.4 Permission + tool-call UI** — ~1.5 wk
- ACP permission requests (run command, write outside workspace, fetch URL) map to a
  blocking prompt overlay with allow-once / allow-session / deny; persist session
  grants. Reuse the plugin process-permission allowlist model (`src/plugin/process.rs`)
  as the policy layer.
- Tool-call progress in the panel (the `fidget.hk` LSP-progress pattern).

**2.5 The demo + launch** — ~2 wk
- Script and record the flagship demo **over SSH** on a bare server: `scp` the binary,
  open a repo, invoke Claude Code, review and accept a multi-file change — under 2
  minutes, zero config shown on screen.
- Launch assets: README rewrite around the positioning line, comparison page
  (red vs nvim+plugins vs Helix vs Zed), HN "Show HN" post, YouTube build-up episodes.
- **Launch gate:** Phase 1 gauntlet passed. Do not launch the agent demo while `.` is
  missing — the audience that cares will try vim muscle memory within 60 seconds.

**Exit criterion:** the demo runs end-to-end on a clean machine over SSH with no config
file; `disable_ai = true` verifiably inert; one external tester reproduces it unaided.

---

## Phase 3 — Agent-attributed undo + detachable sessions (10–14 weeks)

**Goal:** the features that make red *defensibly* the agent-era editor rather than the
first mover: an audit trail for AI edits, and sessions that survive disconnects — the
thing Neovim users waited a decade for (`:detach`, requested 2016, shipping ~0.13).

### Track A — Agent-attributed undo (~4–5 wk)

**A.1 Attributed transactions** — ~1.5 wk
- Extend `EditTransaction` (`src/undo.rs`) with `origin: User | Agent{session, turn} |
  Plugin{name} | Lsp{server}` plus a timestamp. Phase 2 already tags agent edits;
  this generalizes it.
- Serialize origin with the transaction (feeds Track B persistence).

**A.2 Change review UI** — ~2 wk
- "Agent changes" picker: list agent transactions grouped by turn; jump to location;
  visual diff of each transaction.
- **Selective revert**, honestly scoped: reverting the *tip* transaction is exact;
  reverting an older transaction applies its inverse only if the affected ranges are
  untouched since ("clean revert"), otherwise present a conflict view rather than
  guessing. Do not promise arbitrary-order revert in v1.

**A.3 Upgrade linear undo → undo tree** — ~1.5 wk (do here, while `undo.rs` is open)
- Branch on undo+edit instead of truncating the redo stack; simple tree navigation
  (`g-`/`g+` time-ordered traversal). Persistent undo files are a stretch goal, not a
  commitment.

### Track B — Detachable sessions (~6–9 wk) — the largest single bet in this plan

Staged deliberately, because true detach requires splitting a 14.5k-line monolith:

**B.1 Milestone: crash-safe session persistence** — ~2.5 wk
- Serialize full session state: open buffers (+ unsaved content), window layout,
  cursors, jumplist, marks, registers, **undo history with attribution**.
- Periodic + on-exit snapshots; `red --resume` restores everything. Upgrade the
  existing `session_restore.hk` plugin into a core feature (plugins keep access via
  host API).
- Agent sessions resume logically: conversation transcript + pending diffs restored;
  the agent process itself restarts (ACP session resume where the adapter supports it).
- **This milestone alone is shippable and valuable.** It de-risks B.2.

**B.2 Milestone: true detach/reattach (client–server split)** — ~4–6 wk
- Headless core process owns buffers, LSP clients, plugin VM, and **running agent
  sessions**; thin TUI client speaks a render/input protocol over a unix socket.
- The existing architecture helps: rendering already diffs against a `RenderBuffer`
  (`src/editor/render_buffer.rs`), input is already an async event stream, and the
  main loop is already a `tokio::select` multiplexer — the seam exists conceptually.
  The work is enforcing it across `editor.rs`.
- `red --detach` / `red --attach <session>`; killing the terminal (or dropping SSH)
  leaves the agent task running.
- Explicit non-goals for v1: multiple simultaneous clients, remote (TCP) attach,
  collaboration. Unix socket, one client, same machine.

**Exit criterion (the demo):** start a long agent refactor over SSH, kill the SSH
connection, reattach from a new terminal — the agent task is still running, its
completed edits are listed in the agent-changes view with attribution, and one hunk is
selectively reverted.

**Sequencing note:** Track A and B.1 can start during late Phase 2 polish. B.2 should
not start until the Phase 2 launch has shipped — it is invasive, and the launch must
not wait on it.

---

## Phase 4 — Typed-plugin stability story (8–10 weeks, partly parallel with Phase 3)

**Goal:** make "your setup will not break on update" a checkable guarantee, not a slogan.
Neovim structurally cannot offer this (volunteer plugins are load-bearing; the
nvim-treesitter archival proved it). Husk — a typed language with rustc-style
diagnostics — is the mechanism. This phase puts the type checker on the runtime path,
where it currently is not (the `crates/husk` VM runs untyped; `husk-semantic` ships in
the workspace but is only wired to the old JS-codegen design).

### Workstreams

**4.1 Typed host API surface** — ~2.5 wk
- Author a canonical typed declaration of the `red::*` API (~55 execute actions, ~23
  request actions, ~37 stdlib fns currently implemented in `src/plugin/runtime.rs` and
  `crates/husk/src/lib.rs`) — the Husk equivalent of a `.d.ts` file, generated or
  checked from the Rust side so it cannot drift.
- This also fixes the documentation gap: `docs/PLUGIN_SYSTEM.md` documents ~10 of the
  55 actions today. Generate API reference docs from the same source.

**4.2 Type checking on the plugin load path** — ~3 wk
- Wire `husk-semantic` into plugin load: parse → resolve → typecheck against the host
  API declarations → only then hand to the VM.
- Failure mode is the product: `error[HUSK-E####]` with source span, call frame, and a
  "this action's signature changed in red 0.x — see migration note" message. The
  diagnostics engine (`crates/husk-diagnostics`) already produces this quality of error.
- Escape hatch: `--no-typecheck` / per-plugin override, so the checker can ship before
  it is perfect. Measure: all 12 bundled plugins pass clean.

**4.3 Plugin API versioning + compat policy** — ~1.5 wk
- Declare `api_version` in plugin metadata (`src/plugin/metadata.rs`); semver the host
  API; write the public compatibility promise (what can change in minor vs major).
- CI job: typecheck all bundled plugins + a corpus of community plugins against every
  release candidate. Extend `red --self-check` to include the typecheck pass.

**4.4 Hot reload, for real** — ~2 wk
- Rewrite `docs/HOT_RELOAD_PLAN.md` against the Husk runtime (the current doc targets
  the removed Deno design; `registry.rs` has only a manual `reload()`).
- File-watch plugin sources (the `WatchDirectory` host machinery exists), typecheck,
  swap the VM program, replay `activate()`, preserve plugin `state_*` across reloads.
- This is also the plugin-developer-experience story: edit `.hk`, save, see it live.

**4.5 Finish the JS→Husk plugin port + polish** — ~1.5 wk
- Complete the porting roadmap in `docs/PLUGIN_SYSTEM.md` (README calls several bundled
  plugins "placeholders"); remove the remaining placeholder states.
- Stretch (explicitly optional): Husk LSP for `.hk` editing — red already highlights
  Husk via `husk_lexer` in `src/highlighter.rs`; completion/diagnostics from
  `husk-semantic` would make plugin dev delightful, but do not block the phase on it.

**4.6 Release discipline (cross-cutting, starts now, formalized here)**
- Public release cadence (e.g., 6-weekly), announced and kept — the research is blunt
  that invisible cadence killed Helix's and Sublime's momentum.
- Every release: self-check matrix (exists per-platform per `docs/RELEASING.md`),
  RED_PERF regression gate against Phase 0 baselines, bundled-plugin typecheck,
  CHANGELOG with migration notes.

**Exit criterion (the demo):** install a deliberately-outdated community plugin, upgrade
red across a breaking API change → load produces a friendly, span-annotated compile
error naming the changed API and the migration note. Then: edit the plugin, save,
hot-reload picks it up without restarting the editor.

---

## Cross-cutting: positioning & communications

Not a phase — a drumbeat aligned to phase exits:

- **Now → Phase 1 exit:** quiet mode. YouTube episodes on the vim-gauntlet work; recruit
  2–3 vim-native daily-driver testers.
- **Phase 2 exit:** the launch. "Show HN: Red — a zero-config terminal editor with vim
  keys and native agent support." Lead with the SSH demo video. Comparison page. The
  `disable_ai` switch featured *prominently* in the launch post (lesson from Zed's
  577-point de-AI thread: respecting skeptics is itself a differentiator).
- **Phase 3 exit:** second wave — "kill your SSH session, keep your agent working" demo;
  agent-audit-trail post aimed at AI-skeptical engineers.
- **Phase 4 exit:** the stability manifesto — "why your editor setup breaks, and how a
  typed plugin language fixes it," pegged to the nvim-treesitter archival story.

## Risks & mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| ACP protocol churn (Zed-controlled, young) | Medium | Medium | Isolate protocol in `src/acp/` behind an internal trait (as LSP is); pin adapter versions; integration tests against recorded sessions |
| `editor.rs` monolith makes B.2 (detach) a swamp | High | High | Phase 0 refactor first; ship B.1 as standalone value; timebox B.2 spike (1 wk) before committing; never block a launch on it |
| Neovim ships core ACP or Helix merges Steel + vim keymap | Medium | High | Speed is the mitigation — the window is the point of this plan; Phases 3–4 are the durable moat if the window closes |
| Husk typechecker not mature enough for 4.2 | Medium | Medium | `husk-semantic` is 9.2k lines and real, but audit it early (during Phase 2) against all bundled plugins; keep `--no-typecheck` escape hatch |
| Solo-dev bandwidth / burnout (the LunarVim failure mode) | Medium | High | Phases have shippable milestones every ≤3 wk; cut stretch goals ruthlessly; the release cadence includes "smaller release" as a valid release |
| Agent-feature backlash ("another AI editor") | Low–Med | Medium | `disable_ai` from day one, agent UI is a plugin not a takeover, marketing leads with *surface for your agent*, never "AI editor" |
| Pre-alpha data-loss bug during launch attention | Medium | High | B.1's crash-safe snapshots pulled earlier if needed; expand the crash-hardening work (recent commits already trend this way); prominent pre-alpha banner until Phase 3 |

## Success metrics

- **Phase 1:** vim gauntlet checklist 100%; ≥2 external vim users complete a 1-week
  daily-driver trial with zero "would uninstall" reports.
- **Phase 2:** demo reproduced unaided by an external tester; launch post survives
  technical scrutiny (no "but it doesn't have `.`" top comment); 1k GitHub stars as a
  directional (not gating) signal.
- **Phase 3:** detach demo works over real SSH; session restore data-loss reports: zero.
- **Phase 4:** 100% bundled plugins typecheck clean; ≥3 consecutive on-cadence releases;
  zero "update broke my setup" issues attributable to the plugin API.

## Non-goals (explicit, to protect the sequence)

- **Out-plugin-ing Neovim** — curated core + typed plugins is the strategy, not ecosystem scale.
- **Multi-cursor** — different editing model; vim idiom + macros + (future) `:s` cover it. Revisit only on sustained user demand.
- **Collaboration/multiplayer** — Zed's funded territory; demand signals thin in terminal.
- **GUI** — the terminal + SSH niche is the wedge, not a limitation to apologize for.
- **DAP/debugger** — real gap, but not on the differentiation path; post-Phase-4 candidate.
- **Training-wheels distro layer** — red *is* the distro; that's the point.
