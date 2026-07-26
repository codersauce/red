---
title: "Review Agent Proposals"
summary: "Use this guide to inspect, accept, reject, and recover Codex proposal changes without bypassing Red's reviewable-edit boundary."
topics: [guides, agent, reviewable-edits, safety]
sources:
  - id: editor
    type: file
    path: src/editor.rs
  - id: workspace
    type: file
    path: src/agent_workspace.rs
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: plugin-api
    type: file
    path: docs/PLUGIN_API.md
---

Use this guide when Codex has staged changes and a maintainer needs to decide what should enter the editor. A successful review leaves accepted changes as normal editor transactions with agent attribution, leaves rejected changes out of the visible buffer and disk, and preserves pending proposals when Red cannot review them safely [@workflow] [@editor]. The background architecture is [Proposal Workspace](../../architecture/agent/proposal-workspace), and the safety concept is [Reviewable Agent Edits](../../concepts/reviewable-agent-edits).

## Before Reviewing

Confirm that the proposal came through the direct [Codex App-Server Workflow](../../architecture/agent/codex-app-server-workflow), not through an external patch or shell workflow. Red's workflow starts Codex read-only, denies native approvals, and exposes Red-owned dynamic tools, so proposal review assumes the staged contents are held by Red rather than already written to disk [@workflow].

Open review with `:AgentReview`. The workflow document names this command as the way to inspect pending files and hunks [@workflow]. Under the hood, the bundled plugin requests `AgentProposals`; the editor syncs visible buffers, adopts recoverable archived sessions, computes file and hunk payloads, and resolves the request with `files` entries containing session id, path, revision, conflict state, and hunks [@editor].

If the review view reports that a proposal cannot be reviewed safely, do not manually clear proposal state. The editor intentionally returns conflict or safety messages while leaving pending changes intact when it cannot read the current file state or compute hunks [@editor]. Fix the underlying file state first, then request proposal review again.

## Inspect Files And Hunks

Review each file against the current editor-visible contents, not only against what Codex last read. The proposal workspace stores a stable base for each proposed file and computes hunks by rebasing proposed contents against the current buffer; if user edits overlap the proposal, review reports a conflict instead of silently applying stale text [@workspace].

Use file-level review when the entire proposal should be accepted or rejected. Use hunk-level review when only part of a file should move forward. The workspace has separate staging paths for full-file acceptance and one-hunk acceptance, and separate rejection paths for full-file and hunk rejection [@workspace].

The visible decorations are advisory, not the source of truth. The editor builds gutter signs and end-of-line decorations from computed hunks, but the actual decision payload is the proposal file and hunk data returned from `agent_proposals_payload` [@editor]. If UI and payload appear inconsistent during maintenance, trust the payload path and recompute review rather than editing decorations directly.

## Accept A Proposal

Accepting a proposal should always go through the agent proposal action, never through a direct buffer paste. The editor handles `AgentAcceptProposal` by syncing visible buffers, reading the current file state, staging either the selected hunk or the whole file, and then applying the staged disposition [@editor]. Proposal staging does not mutate proposal state; `commit_acceptance` only consumes the staged token after the editor confirms that the proposal still matches the expected state [@workspace].

When the disposition is applied, the editor opens or selects the target buffer, commits the proposal acceptance, begins an `EditOrigin::Agent` transaction, replaces the whole buffer with the accepted contents, commits the transaction, notifies change consumers, renders, resyncs visible buffers, and emits `agent:proposal_applied` with the session id, turn id, path, and created-file flag [@editor]. This is the success condition: the accepted change is now an attributed editor edit, not a hidden proposal.

If acceptance fails, leave the proposal pending. The editor's failure path sets `Unable to accept agent proposal safely; pending changes were left intact` and continues without consuming the proposal [@editor]. Reopen review after fixing the cause, such as stale hunk id, current-file conflict, missing workspace, or unsafe file state.

## Reject A Proposal

Rejecting a proposal also goes through the agent proposal action. The editor handles `AgentRejectProposal` by syncing visible buffers, reading the current file state, and calling either `reject_hunk` or `reject_all` on the proposal workspace [@editor]. `reject_all` resets the proposed file to the current visible contents, while `reject_hunk` removes only the selected hunk and keeps the remaining rebased changes pending [@workspace].

After a successful rejection, the editor emits `agent:proposals_changed` for the session so the review UI can refresh [@editor]. If rejection fails, pending changes remain intact and the editor reports that it was unable to reject the proposal safely [@editor]. Treat that as a recovery condition, not as permission to mutate the proposal maps manually.

## Recover After Session Loss

If the Codex process stops or a session is closed with pending changes, review should still be possible. The workflow states that a stopped process archives pending proposals and preserves the submitted prompt for retry [@workflow]. The workspace implements this by archiving sessions that still have effective pending files, exposing archived sessions to review, and adopting non-overlapping recovered proposals into a replacement live session [@workspace].

Use `:AgentReview` after reconnecting or starting a new agent session to surface archived proposals. The editor's proposal payload builder calls `adopt_recovered_sessions` before returning files, so non-conflicting recovered work can appear under the active review session while overlapping recovered sessions remain separately reviewable [@workspace] [@editor].

Plugin code should use the documented request and event boundary instead of reading workspace internals. `docs/PLUGIN_API.md` describes `AgentArchiveSession` for cases where Codex app-server has already stopped, `AgentCloseSession` for a live session, `AgentPrompt` context behavior, and text-panel events used by the bundled agent UI [@plugin-api]. Those calls preserve Red's proposal ownership and keep review state coordinated with the editor.
