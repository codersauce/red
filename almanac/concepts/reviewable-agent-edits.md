---
title: "Reviewable Agent Edits"
summary: "Reviewable agent edits are Red's model for keeping Codex changes in proposal state until the user explicitly accepts them."
topics: [agent, reviewable-edits, safety]
sources:
  - id: agent-workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: codex
    type: file
    path: src/codex/mod.rs
  - id: workspace
    type: file
    path: src/agent_workspace.rs
---

Reviewable agent edits are Red's safety model for Codex-assisted changes: Codex may inspect editor state through bounded tools and stage proposed file contents, but the visible editor buffer and disk do not change until the user reviews and accepts a proposal. The workflow documentation states that Red launches Codex as an app-server, starts threads with read-only sandboxing and `approvalPolicy = "never"`, disables broader Codex surfaces, and exposes Red-owned dynamic tools whose write paths update an isolated proposal workspace [@agent-workflow]. The Rust proposal workspace enforces the same boundary in code: agent writes update proposed contents only, while callers must explicitly accept a proposal and route accepted text through the editor transaction boundary [@workspace].

## Proposal-First Editing

The user-visible flow starts with `Space A` or `:Agent`, then continues through `:AgentReview` for pending files and hunks [@agent-workflow]. Codex is instructed to use Red's editor tools for every read and edit; the `INSTRUCTIONS` string in the Codex worker tells it that edits are reviewable editor proposals and never touch disk [@codex]. This keeps the assistant's write capability behind the editor's review and attribution system rather than giving it native workspace mutation.

The proposal workspace stores visible file revisions and per-session proposed contents. `read` returns staged proposal contents when present, `write` replaces only the in-memory proposed contents under a size bound, and `apply_editor_edits` applies revision-checked UTF-16 edits to proposal text before computing review hunks [@workspace]. This means later agent reads in the same session can see earlier staged proposals without making those changes visible in the user's active buffer.

## Safety Boundaries

Red starts Codex with a read-only sandbox, no execution environments, disabled configured MCP servers, disabled apps/connectors/plugins/orchestrator MCP/notifications, and Red's bounded dynamic tools [@agent-workflow]. The implementation builds `turn/start` requests with `approvalPolicy` set to `never`, a read-only sandbox policy, and dynamic tool definitions for the Red tool surface [@codex]. Native command, file-change, and permission escalation requests are denied by the documented contract [@agent-workflow].

Path and content checks exist on the Red side as well. Proposal paths are normalized under the physical workspace root, root identity is verified before exposing paths, proposal content is capped, stale revisions are rejected, and accepted proposals are staged before commit so review state does not advance unless the editor-side attributed edit succeeds [@workspace]. These rules are why reviewable edits belong with [Proposal workspace](../architecture/agent/proposal-workspace), not only with the Codex transport layer.

## Review And Recovery

The review step is explicit. The workflow document says accepting a proposal passes through the editor transaction boundary and receives agent attribution, while rejecting it discards only the selected proposal and unaccepted proposals never mutate a visible buffer or disk [@agent-workflow]. The workspace supports full-file acceptance, hunk computation, conflict reporting when current contents diverge from a proposal base, and archiving pending proposals after process loss [@workspace].

This model also shapes the app-server decision. Red speaks the Codex app-server protocol directly rather than using an ACP adapter, and the workflow documentation states there is no fallback to `codex exec` or native edits when the required protocol is unavailable [@agent-workflow]. Read [Codex app-server workflow](../architecture/agent/codex-app-server-workflow), [Direct Codex app-server](../decisions/agent/direct-codex-app-server), and [Review agent proposals](../guides/agent/review-agent-proposals) for the operational details.
