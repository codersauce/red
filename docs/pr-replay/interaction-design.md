# PR Replay interaction design

Status: target interaction model. Some controls already exist; a persistent
Codex companion, first-class findings, ordering profiles, grouped steps, and
compile checkpoints remain proposed. Current commands are documented in the
[existing PR Replay guide](../PR_REPLAY.md).

## Surface ownership

Replay uses genuine Red editor and panel primitives:

- `PanelManager` owns the dedicated Replay guide, its focus, scrolling,
  placement, and native rendering.
- The source is a genuine editor buffer and editor window, never an editable
  guide pretending to be source.
- A future persistent Codex companion uses an existing editor-owned text or
  conversation panel rather than replacing the source editor.
- The editor event loop owns buffer mutations, selections, transactions, undo,
  and plugin-visible UI updates.
- Git, GitHub, worktree, agent, and build tasks run in bounded background
  workers.

The existing answer-only view inside the Replay pane is an interim behavior.
The target keeps the change guide visible while a conversation is open.

## Core surfaces

### Replay guide

The guide is the stable review navigator. It displays:

- PR or local-branch identity and exact original head.
- Verified relationship, such as `REVIEW` or `YOUR PR`.
- Current change, real completion, and dependency-aware ordering profile.
- Visible original change list and selected file.
- Exact original diff with syntax and addition/removal highlighting.
- Author-stated or explicitly inferred rationale.
- Relevant finding, draft, and checkpoint indicators.
- A compact, pinned action bar.

The guide is read-only. Its list and metadata remain stable when the diff is
long. Only the current hunk scrolls in the normal guide layout.

### Source editor

The source is a real, focusable editor window. Its window bar always explains
the current reality:

```text
ORIGINAL PR · src/app.rs · 15c4957 · READ ONLY
REPLAY · src/app.rs · change 14/49
SCRATCH SOURCE · src/app.rs · 2 local edits
PROPOSED FIX · src/app.rs · 1 pending hunk
PR SOURCE · src/app.rs · YOUR PR · 15c4957
```

Scratch and original-author worktrees must not share an ambiguous generic
`SOURCE` label. The editor's real cursor, selection, language highlighting,
diagnostics, and undo behavior remain intact.

### Codex companion

The companion is hidden until requested. Opening it must not replace the Replay
guide, close the source editor, or consume the entire terminal with an
oversized modal.

It retains one PR-wide conversation while recording which original step and
snapshot each turn used. The source remains visible while the person asks
follow-up questions or investigates a finding.

The panel contains:

- Conversation history and Markdown-capable streaming responses.
- A real composer for follow-up questions.
- Explicit actions for investigation, finding promotion, comment drafting,
  and authorized patch proposals.
- Busy, cancelled, error, and retry states.
- Current step and whole-PR context indicators.

## Responsive layouts

### Wide terminal

At comfortable widths, a third panel can be shown without making code unreadable:

```text
┌ PR REPLAY ─────────────┬ SOURCE ──────────────────────┬ CODEX ──────────────┐
│ #2733 · REVIEW         │ src/app.rs · REPLAY 14/49    │ Why does fork need  │
│                        │                              │ a different token   │
│ ✓ 12 Add state         │ pub fn resume_thread(...) { │ restoration path?   │
│ ✓ 13 Restore tokens    │     ...                      │                     │
│ ▶ 14 Handle forks      │ }                            │ Forked threads do   │
│ ○ 15 Add tests         │                              │ not inherit the...  │
│                        │                              │                     │
│ ORIGINAL CHANGE        │                              │ > Ask a follow-up   │
│ Original diff...       │                              │                     │
└────────────────────────┴──────────────────────────────┴─────────────────────┘
```

The source should remain the visually dominant surface. A conversation panel
may be docked or moved with existing editor pane commands.

### Normal terminal

The default is a roughly equal Replay/source split. Opening Codex uses a
bottom drawer when three side-by-side columns would clip source excessively:

```text
┌ PR REPLAY ───────────────────┬ SOURCE ──────────────────────┐
│ ▶ 14 Handle fork behavior    │ src/app.rs · REPLAY 14/49    │
│ Original diff and rationale  │ Current source and cursor    │
├──────────────────────────────┴──────────────────────────────┤
│ CODEX                                                       │
│ You: Why is restoration different for forks?                │
│ Codex: The fork response does not include the same...       │
│ > Ask a follow-up                                           │
└─────────────────────────────────────────────────────────────┘
```

Opening or closing the companion preserves the exact prior guide/source split.

### Small or short terminal

When there is not enough space for three readable surfaces:

- Keep source and current change usable.
- Collapse secondary metadata before removing the selected change.
- Offer a toggleable companion drawer or temporary zoomed panel.
- Preserve conversation, draft, scroll, and source state when a surface hides.
- Never force an unreadable three-column layout.
- Keep destructive or external actions clearly labeled even in a compact bar.

## Focus, movement, and resizing

Each focused panel has a visible title accent, highlighted divider, and real
terminal cursor position. The editor status line distinguishes Replay, Codex,
and ordinary source editing.

Existing Vim window conventions remain available:

- `Ctrl-w h/j/k/l` moves focus between neighboring surfaces.
- `Ctrl-w H/J/K/L` repositions the focused dockable panel.
- `Ctrl-w >` and `Ctrl-w <` resize vertical splits.
- `Ctrl-w +` and `Ctrl-w -` resize horizontal splits.
- `Ctrl-w =` restores the default split size.
- Dragging a divider changes size and visibly brightens the exact divider.

Resize or reposition operations preserve original source, scratch progress,
conversation history, findings, pending drafts, and editor cursor state.

## Navigation

The current Replay guide binds `j/k` to change selection and uppercase `J/K` to
diff scrolling. This remains the baseline until an explicit interaction review
changes it.

Target principles:

- Plain navigation follows the focused surface.
- Source-buffer Vim editing remains unchanged.
- A focused change list moves between review steps.
- A focused conversation scrolls its own transcript.
- A focused diff scrolls without changing the selected step.
- Changing a step updates the current source location and AI context without
  resetting unrelated pane state.
- Jumping between changed files is distinct from moving within one file.
- Moving to an unvisited step does not claim that its dependencies are done.

Exact final conversation shortcuts remain an open UX decision. Existing
`Space R` commands continue to provide namespaced, discoverable access.

## Original change and grouped steps

Every individual original hunk remains inspectable even when several hunks form
an atomic semantic group. The list can represent:

```text
✓ 12 Add pagination state
▶ 13 Update thread-resume interface · 3 files
    src/protocol.rs
    src/app.rs
    tests/resume.rs
○ 14 Add fork regression coverage
```

The group can be inspected one original hunk at a time. Its completion and
optional compile checkpoint are reported only when the full group is complete.

Dependency annotations explain ordering without requiring the reviewer to leave
the selected change:

```text
Requires: PaginationState · change 12
Used by: resume_thread · change 16
```

## Findings and outbox

Findings are lightweight private observations, not submitted comments. A
finding can be expanded, investigated, promoted to a draft, converted to an
authorized patch request, or dismissed.

The outbox is a stable dedicated surface containing:

- Outstanding findings and review coverage.
- Original-source inline comments.
- A PR-level review summary.
- Agent suggestions waiting for explicit human acceptance.
- Approved original-PR patches when branch repair is authorized.
- Provider receipts and unresolved submission states.

Final actions show their actual consequence: publish review, approve a source
hunk, save, commit, or push. They never share an ambiguous `Accept` label.

## Notices, errors, and long-running operations

Progress appears immediately in the relevant stable surface. Animated spinners
and bounded background work make source loading, agent turns, provider lookup,
and approved compilation visible.

Notices never displace the current diff or cause the change list to jump.
Errors state whether anything was changed, saved, posted, committed, or pushed.
Cancellation preserves the current review and any already approved local work.

Use compact dialogs for true confirmation boundaries. Do not use a giant
full-screen composer to display an answer, a status message, or a finding.

See [review workflows](review-workflows.md) for complete journeys and
[Codex collaboration](codex-collaboration.md) for agent interaction states.
