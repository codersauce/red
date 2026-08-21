---
title: "Agent Architecture"
summary: "Agent architecture routes readers through the direct Codex app-server workflow, dynamic tools, followed editing, agent-attributed edits, and history operations."
topics: [architecture, agent, codex, agent-edits]
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
  - id: tools
    type: file
    path: src/agent_tools.rs
  - id: agent-plugin
    type: file
    path: plugins/agent.hk
  - id: copilot-guide
    type: file
    path: almanac/guides/agent/copilot-completion.md
---

# Agent Architecture

Red's agent architecture is a direct Codex app-server integration wrapped in editor-owned followed editing. Red starts `codex app-server --stdio`, manages Codex threads and turns, exposes bounded dynamic tools, and denies native mutation or approval escalation while routing writes through revision-checked editor tools [@workflow] [@codex]. The editor owns the bridge, active-session state, and editor-tool request channel through `AgentManager`, while the bundled Husk agent plugin owns the terminal-facing conversation and history UI [@editor] [@manager] [@agent-plugin]. Use this hub to choose the page for the layer you need to change.

## Reading Order

Start with [Codex App-Server Workflow](codex-app-server-workflow) for process launch, app-server initialization, account checks, turn dispatch, conversation-scoped model selection, activity presentation, event polling, and fail-closed behavior. It is the runtime path that turns `Space A` or `:Agent` into a Codex thread and streamed assistant updates [@workflow] [@codex].

Read [Dynamic Tools And Editor Tools](dynamic-tools-and-editor-tools) when work touches the tool surface Codex can call. That page covers the app-server tool definitions, the six strict editor-tool schemas, UTF-16 editor coordinates, directory creation, allow-listed editor actions, and the bounded channel that routes editor tools back through Red [@codex] [@tools].

Use [Followed Editing](followed-editing) for the full-agent write path: Red reveals the target file, checks the visible revision, applies an agent-attributed editor transaction, and saves through the editor [@workflow] [@editor].

[Agent-Attributed Edits](../../concepts/agent-attributed-edits) is the safety model behind the architecture. Read it before changing whether Codex can inspect state, edit text, or cross the editor transaction boundary [@workflow] [@editor].

For task-oriented operation, use [Inspect Agent History](../../guides/agent/inspect-agent-history). For inline AI suggestions that run beside language-server completion rather than through Codex turns, use [Copilot Inline Completion](../../guides/agent/copilot-completion) [@copilot-guide]. For prerequisites and offline readiness checks, use [Agent Check](../../reference/agent/agent-check). For the accepted integration decision, use [Direct Codex App-Server](../../decisions/agent/direct-codex-app-server).

## Boundaries To Preserve

Do not collapse the Codex transport, editor transaction path, and Husk UI into one ownership model. The Codex worker owns the app-server protocol and dynamic-tool dispatch [@codex]. `AgentManager` owns bridge state, turn state, conversation state, and editor-tool channels inside the editor core [@manager]. Text writes enter the visible editor through editor-owned transactions and saves, not through Codex native file changes [@workflow] [@editor]. The bundled agent plugin listens to `agent:*` events and sends `Agent*` requests; it does not own the Codex process or mutate buffers directly [@agent-plugin].
