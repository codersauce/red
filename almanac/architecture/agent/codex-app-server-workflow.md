---
title: "Codex App-Server Workflow"
summary: "Red runs Codex as a direct app-server worker while keeping all edits behind Red-owned dynamic tools, followed application, and agent attribution."
topics: [architecture, agent, codex, agent-edits]
sources:
  - id: codex
    type: file
    path: src/codex/mod.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: manager
    type: file
    path: src/editor/agent_manager.rs
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: agent-plugin
    type: file
    path: plugins/agent.hk
---

The Codex app-server workflow is Red's direct integration with the installed Codex CLI. Red starts `codex app-server --stdio`, initializes the app-server protocol, verifies the Codex account, starts an app-server thread, and then drives turns through JSONL requests and events [@codex]. Codex is never given native workspace mutation authority: Red starts threads and turns with read-only sandboxing, denied native approvals, explicitly granted extension surfaces, and Red-owned dynamic tools that read, edit, and save through the editor [@workflow] [@codex]. The editor side owns the bridge, active-session state, and bounded editor-tool channel through `AgentManager`, so app-server events can be polled from the editor loop without making Codex a direct editor owner [@manager] [@editor].

## Process And Session Ownership

`CodexProcessSpec` records the executable, literal arguments, environment, working directory, and explicit Agent capability policy for one app-server worker [@codex]. The worker disables ungranted apps, connectors, plugins, remote plugins, skill MCP dependency installation, and orchestrator MCP through Codex config overrides. Thread configuration enables only MCP server names and feature categories listed under `[agent]`; inline and commit-message threads do not receive those extension grants. The worker also captures a bounded sanitized stderr tail and kills the child on drop [@codex]. After startup, Red sends `initialize`, `initialized`, and `account/read`; the worker refuses to continue without an authenticated Codex session [@codex].

The user-visible lifecycle follows the workflow document: `Space A` or `:Agent` opens the prompt, Red lazily starts the app-server, creates a thread, submits a turn, streams assistant deltas, and sends `turn/interrupt` for cancellation [@workflow]. The implementation mirrors that flow through `CodexCommand::NewSession`, `Prompt`, `PromptWithContext`, `Cancel`, and `CloseSession`; responses from `thread/start` become `SessionCreated`, responses from `turn/start` set the active turn id, assistant deltas become `Update`, and `turn/completed` becomes `Completed` [@codex].

The editor owns the bridge rather than the app-server owning the UI. When an agent session is requested, the editor creates a bounded editor-tool channel, starts Codex with `EditorToolHost`, stores the resulting bridge and task in `AgentManager`, records the workspace root, and then sends `NewSession` [@editor]. Active sessions and turn timing are tracked in `AgentManager`, which lets the editor reject stale editor-tool calls and measure completed turns without putting that state inside the Codex worker [@manager] [@editor].

## Read-Only Safety Boundary

The workflow's safety rule is that Codex may inspect and request edits, but Red keeps mutation authority [@workflow]. Red starts each app-server thread with `approvalPolicy = "never"`, `sandbox = "read-only"`, empty execution environments, a restricted config, Red's dynamic tool definitions, and base instructions that tell Codex it has no shell or native patch tool and must use `apply_edits` or `write_file` for every edit [@codex]. Each turn also carries `approvalPolicy = "never"`, a read-only sandbox policy, and no execution environments [@codex].

Native Codex requests are denied at the app-server boundary. File-change and command-execution approval requests receive a declined decision, permission requests receive an empty permission set with strict auto-review, and unknown server requests with ids get a JSON-RPC method-not-found error [@codex]. The documented workflow states the same product contract: native command, file-change, and permission escalation requests are denied, and Red never asks Codex to edit the workspace directly [@workflow].

This boundary connects the workflow to [Agent-Attributed Edits](../../concepts/agent-attributed-edits). App-server availability alone is not enough; Codex must use [Dynamic Tools And Editor Tools](dynamic-tools-and-editor-tools) so unsaved buffers, cursor state, diagnostics, revision checks, saves, and transaction attribution remain authoritative on Red's side.

## Turn And Event Flow

Prompt dispatch begins in the editor. Before sending a turn, `dispatch_agent_prompt` verifies a running bridge, rejects concurrent prompts for the same active session, emits `agent:turn_started`, marks the session active, records the user message for conversation recovery, and sends either `Prompt` or `PromptWithContext` to the Codex bridge [@editor]. `PromptWithContext` appends bounded active-editor context to the user text before the worker sends `turn/start` [@codex].

The worker keeps app-server reading, command handling, response correlation, and dynamic-tool results in one async loop [@codex]. It reads stdout frames on a separate task, uses a pending-request table keyed by JSON-RPC id, tracks sessions by Codex thread id, and drops tool results if the referenced turn is no longer active or has been cancelled [@codex]. Tool arguments, tool responses, app-server frames, tool runtime, file-list pages, workspace walks, search matches, and search bytes are bounded; there is no per-turn tool-call count ceiling [@codex] [@workflow].

The editor polls both directions from `service_background`. It follows and executes pending editor-tool requests through the owner task, then drains Codex events; inactive-session updates are ignored, stale permission requests are denied, terminal events mark sessions inactive, and all user-facing Codex events are translated to plugin notifications such as `agent:update`, `agent:activity`, `agent:completed`, `agent:cancelled`, and `agent:error` [@editor].

## Conversation UI State

The bundled agent plugin keeps three related states separate: the live Codex `session_id`, a `pending_prompt` that has not safely become part of a turn, and a persisted human-readable `transcript` [@agent-plugin]. `Space A` opens and focuses the conversation panel when a live session exists, but it also opens the panel for a restored transcript when there is no pending prompt; that avoids treating archived context as an unsent retry prompt [@agent-plugin].

The editor replays persisted transcript text to the plugin during detached-core creation and session restoration by sending `agent:transcript_restored`, while crash recovery stores restored transcript text back into agent plugin storage and reports archived context when the snapshot is not resumable [@editor]. The plugin converts restored plain transcript text back into conversation blocks and renders the panel without inventing a Codex session [@agent-plugin]. This is the UI counterpart to [Crash Recovery Snapshots](../sessions/crash-recovery-snapshots): transcript recovery preserves context, not app-server continuity.

## Failure Behavior

Red fails closed when the direct app-server contract is unavailable. The workflow documentation says Red pins a minimum tested Codex CLI version and does not fall back to `codex exec` or native edits [@workflow]. In code, missing sessions produce `Failed` events, app-server request errors become user-facing failure events, tool calls against unknown or inactive turns return errors, and a finished worker causes the editor to drop the bridge, clear active sessions and tool requests, and notify `agent:session_lost` [@codex] [@editor].

The retry path depends on the loss event payload. `dispatch_agent_prompt` includes the submitted prompt when no bridge is available or when sending the command to the bridge fails, but the generic finished-worker notification only reports that the app-server stopped [@editor]. The bundled agent UI saves and replays a prompt only when the event includes one; otherwise it tells the user to start a new session [@agent-plugin].

The readiness gate for this workflow is [Agent Check](../../reference/agent/agent-check). `red --agent-check` is offline, so it verifies executable discovery and minimum version but leaves authentication to the first live `account/read` call [@workflow]. The accepted architectural constraint behind the workflow is recorded in [Direct Codex App-Server](../../decisions/agent/direct-codex-app-server).
