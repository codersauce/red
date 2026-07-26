---
title: "Proposal Workspace"
summary: "The proposal workspace is Red's session-scoped filesystem model for Codex reads, staged writes, conflict-aware review, and crash-recoverable pending changes."
topics: [architecture, agent, persistence, reviewable-edits, safety]
sources:
  - id: workspace
    type: file
    path: src/agent_workspace.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: agent-plugin
    type: file
    path: plugins/agent.hk
---

The proposal workspace is Red's review-before-apply filesystem for agent changes. It stores editor-visible file bases, per-session proposed contents, active turn attribution, archived recovered sessions, and a generation counter; its module-level contract says agent writes update proposed contents only and callers must explicitly accept a proposal through the editor transaction boundary before visible buffers or disk can change [@workspace]. This workspace is the data structure behind [Reviewable Agent Edits](../../concepts/reviewable-agent-edits): Codex can read its own staged changes and build on them, while the user still decides what enters the editor through [Review Agent Proposals](../../guides/agent/review-agent-proposals) [@workflow] [@editor].

## Session-Scoped Filesystem Model

`ProposalWorkspace` is rooted at a normalized workspace directory and stores a `VisibleFile` map plus an `AgentSession` map [@workspace]. Each proposed file keeps the visible base revision, base contents, proposed contents, whether the proposal creates a new file, and the turn id that last changed it [@workspace]. The editor creates or reuses this workspace when starting a Codex session, syncs visible buffers into it, wraps it in `ProposalToolHost`, and shares it with both the Codex worker and the editor-tool dispatcher [@editor].

Reads and writes are session-scoped. `read` normalizes the requested path and ensures a proposal entry exists; if the session already has staged contents, the read returns those contents, otherwise the visible editor buffer or safely read disk file becomes the stable base [@workspace]. `write` replaces only the proposed contents for that session, records the active turn id or `unattributed`, enforces the proposal size bound, and bumps the workspace generation when contents change [@workspace].

This model lets a Codex turn perform read-after-write reasoning without changing the user's editor. The workflow documentation states that later reads in the same session see staged proposal contents, while unaccepted proposals never mutate a visible buffer or disk [@workflow].

## Path And Content Safety

Proposal paths are not free-form filesystem paths. `normalize_path` requires an absolute path, lexically normalizes it, requires it to stay under the workspace root, and rejects symlink components [@workspace]. Before exposing paths that could later be saved through the editor, `ensure_root_is_current` verifies that the lexical root still names the pinned physical directory on Unix, or at least a safe non-symlink directory on non-Unix platforms [@workspace].

Unopened disk reads also use safe boundaries. On Unix, `read_bounded_file` walks below the pinned root with `openat`, `O_NOFOLLOW`, and `O_NONBLOCK`, requires the final target to be a regular file, and caps content at the proposal size limit [@workspace]. On non-Unix platforms, an unopened existing file cannot be read safely by this helper; callers must open the file in Red first [@workspace]. The workflow summarizes the same constraints as rejecting parent traversal, symlink components, special files, unsafe roots, oversized content, stale revisions, and overlapping edits [@workflow].

## Visible Bases And Conflict Detection

The workspace treats visible editor contents as the review base. `sync_visible_file` and `replace_visible_files` publish current buffer revisions and contents, while existing proposal bases remain stable so later review can detect divergence [@workspace]. `apply_editor_edits` requires the proposal base revision and current visible revision to match the agent's `expected_revision`; stale editor state fails before the proposed text is updated [@workspace].

Review hunks are computed by rebasing proposed contents from the stable base onto current contents. `hunks` returns a conflict if rebase fails, and acceptance staging returns `ProposalDisposition::Conflict` with base, current, and proposed text when the current buffer overlaps the proposal's change [@workspace]. The editor turns those results into proposal payload entries with `conflict: true`, gutter signs, decorations, and messages that say pending changes were left intact when review cannot be performed safely [@editor].

## Staged Acceptance And Rejection

Acceptance is deliberately two-phase. `stage_accept_all` and `stage_accept_hunk` validate the current file state and create a `StagedProposalAcceptance` without mutating proposal state; `commit_acceptance` consumes that token only after checking that the proposal still matches the expected file state [@workspace]. The editor then applies an accepted proposal through an attributed transaction with `EditOrigin::Agent`, replaces the visible buffer contents, commits the editor transaction, notifies change consumers, renders, resyncs visible buffers, and emits `agent:proposal_applied` [@editor].

Rejection mutates only proposal state. `reject_all` resets a proposal to the current visible contents, and `reject_hunk` rebases remaining changes, removes the selected hunk, resets the file base, and keeps any un-rejected changes as proposed contents [@workspace]. The editor request handlers for `AgentAcceptProposal` and `AgentRejectProposal` both sync visible buffers first; on failure they leave pending changes intact and set a user-facing error rather than consuming proposal state [@editor].

This is why proposal acceptance belongs behind the [Text Mutation Boundary](../editor/text-mutation-boundary). Proposal state may decide what should be applied, but only the editor-owned transaction path makes an accepted change visible and attributable.

## Recovery State

The workspace is serializable for crash recovery. `ProposalWorkspaceSnapshot` captures the root, visible files, and session proposals, and `from_snapshot` restores sessions as recovered sessions without reading or writing workspace files [@workspace]. `archive_session` retains sessions with pending changes after process loss or session close, `review_sessions` returns the active session plus archived sessions with pending files, and `adopt_recovered_sessions` transfers non-overlapping archived proposals into a replacement live session [@workspace].

The workflow relies on that state when app-server failure occurs: a stopped process must archive pending proposals, while prompt retry is available only on loss events that carry the submitted prompt [@workflow] [@editor]. The bundled agent UI calls `AgentArchiveSession` for the current session on `agent:session_lost` and saves a retry prompt only when `event.prompt` is present [@agent-plugin]. The editor also archives proposals for `AgentCloseSession` and `AgentArchiveSession`, and `agent_proposals_payload` adopts recovered sessions before building review data [@editor]. The result is that a lost app-server process does not turn pending proposals into hidden disk edits or discard reviewable work.
