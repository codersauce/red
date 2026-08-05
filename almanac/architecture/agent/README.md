---
title: "Agent Architecture"
summary: "Agent architecture routes readers through the direct Codex app-server workflow, dynamic tools, proposal workspaces, reviewable edits, and agent review operations."
topics: [architecture, agent, codex, reviewable-edits]
sources:
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: codex
    type: file
    path: src/codex/mod.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: manager
    type: file
    path: src/editor/agent_manager.rs
  - id: workspace
    type: file
    path: src/agent_workspace.rs
  - id: tools
    type: file
    path: src/agent_tools.rs
  - id: agent-plugin
    type: file
    path: plugins/agent.hk
---

# Agent Architecture

Red's agent architecture is a direct Codex app-server integration wrapped in a proposal-first edit model. Red starts `codex app-server --stdio`, manages Codex threads and turns, exposes bounded dynamic tools, and denies native mutation or approval escalation while preserving all writes as reviewable proposals [@workflow] [@codex]. The editor owns the bridge, active-session state, proposal workspace, and editor-tool request channel through `AgentManager`, while the bundled Husk agent plugin owns the terminal-facing conversation and review UI [@editor] [@manager] [@agent-plugin]. Use this hub to choose the page for the layer you need to change.

## Reading Order

Start with [Codex App-Server Workflow](codex-app-server-workflow) for process launch, app-server initialization, account checks, turn dispatch, event polling, and fail-closed behavior. It is the runtime path that turns `Space A` or `:Agent` into a Codex thread and streamed assistant updates [@workflow] [@codex].

Read [Dynamic Tools And Editor Tools](dynamic-tools-and-editor-tools) when work touches the tool surface Codex can call. That page covers the four workspace dynamic tools, the five strict editor-tool schemas, UTF-16 editor coordinates, allow-listed editor actions, and the bounded channel that routes editor tools back through Red [@codex] [@tools].

Use [Proposal Workspace](proposal-workspace) for staged contents, visible-file bases, path normalization, conflict detection, acceptance staging, rejection, and recovered pending proposals. It is the data boundary that lets later Codex reads see staged proposals without changing visible buffers or disk [@workspace] [@editor].

[Reviewable Agent Edits](../../concepts/reviewable-agent-edits) is the safety model behind the architecture. Read it before changing whether Codex can inspect state, stage writes, or cross the editor transaction boundary [@workflow] [@workspace].

For task-oriented operation, use [Review Agent Proposals](../../guides/agent/review-agent-proposals). For prerequisites and offline readiness checks, use [Agent Check](../../reference/agent/agent-check). For the accepted integration decision, use [Direct Codex App-Server](../../decisions/agent/direct-codex-app-server).

## Boundaries To Preserve

Do not collapse the Codex transport, proposal store, editor transaction path, and Husk UI into one ownership model. The Codex worker owns the app-server protocol and dynamic-tool dispatch [@codex]. `AgentManager` owns bridge state, turn state, proposal workspace access, and editor-tool channels inside the editor core [@manager]. `ProposalWorkspace` owns proposed file contents and recovery snapshots, but accepted text still enters the visible editor through editor-owned transactions [@workspace] [@editor]. The bundled agent plugin listens to `agent:*` events and sends `Agent*` requests; it does not own the Codex process or apply proposals directly [@agent-plugin].
