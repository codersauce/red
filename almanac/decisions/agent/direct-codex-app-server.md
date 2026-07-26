---
title: "Direct Codex App-Server"
summary: "Red accepted direct Codex app-server integration and superseded its earlier ACP foundation for the agent workflow."
topics: [decisions, agent, codex, architecture]
sources:
  - id: adr-0003
    type: file
    path: docs/adr/0003-direct-codex-app-server.md
  - id: adr-0001
    type: file
    path: docs/adr/0001-acp-foundation.md
  - id: codex
    type: file
    path: src/codex/mod.rs
---

Red's accepted agent integration decision is to speak directly to the installed Codex CLI through `codex app-server --stdio`. ADR 0003 supersedes the earlier ACP foundation and removes the ACP client, generic ACP adapters, OpenAI Responses companion, and Codex ACP translation companion from the supported workflow [@adr-0003]. The consequence for future agent work is narrow but important: Red owns the app-server process, JSONL framing, threads, turns, cancellation, request correlation, dynamic-tool dispatch, and proposal workspace, while Codex runs read-only with native approvals denied [@adr-0003] [@codex].

## Status

This decision is accepted as of ADR 0003, dated 2026-07-19 [@adr-0003]. ADR 0001 is explicitly superseded by ADR 0003 [@adr-0003] [@adr-0001].

## Context

The original ACP foundation put Red's agent boundary behind an Agent Client Protocol client and a child adapter process [@adr-0001]. That design was careful about process lifecycle, bounded queues, filesystem callbacks, permission requests, and offline behavior, but it also discovered a concrete reviewability problem with the audited Codex ACP adapter: the candidate adapter reconstructed diff display by reading the process filesystem while Codex or app-server owned the actual file-change path, so Red could not advertise isolated reviewable edits through that adapter [@adr-0001].

ADR 0001 therefore made provider qualification a gate: Phase 2 had to either select an adapter whose edits demonstrably used ACP client filesystem methods or build a provider-specific integration that redirected every read and write into Red's proposal filesystem [@adr-0001]. ADR 0003 records that the removed Codex companion already translated ACP into app-server calls, so moving that client into core removed a process and protocol boundary while preserving persistent conversations, streaming, cancellation, editor-aware tools, and reviewable proposals [@adr-0003].

`codex exec` was rejected as an automatic fallback because its one-shot automation surface cannot provide Red's bidirectional live editor tools and proposal callbacks without a workspace mirror and post-hoc diff import [@adr-0003]. That would weaken unsaved-buffer semantics and the review guarantee that Red maintains through [Reviewable Agent Edits](../../concepts/reviewable-agent-edits).

## Decision

Red integrates directly with `codex app-server --stdio` and supports one agent backend [@adr-0003]. Red no longer ships or supports ACP adapters or companion executables for the agent path [@adr-0003]. The app-server worker in `src/codex/mod.rs` launches Codex with `app-server --stdio`, disables apps, connectors, plugins, and remote plugins, initializes with experimental app-server capability enabled, verifies authentication through `account/read`, starts ephemeral threads, and submits turns with read-only sandboxing, no execution environments, and `approvalPolicy = "never"` [@codex].

The decision keeps the proposal workspace as the only supported write path [@adr-0003]. In implementation, Codex receives Red's dynamic tool definitions and base instructions telling it to use Red's read, editor, and proposal tools rather than a shell or native patch tool [@codex]. Native file-change, command-execution, and permission approval requests are declined or reduced to empty permissions at the app-server boundary [@codex].

## Consequences

Release archives contain one `red` binary, while users install and authenticate Codex separately [@adr-0003]. `[agent] command` can override the Codex executable, and configured args and env remain direct process values without shell expansion [@adr-0003]. `red --agent-check` verifies executable discovery and the minimum Codex version needed for the app-server dynamic-tool contract, but authentication remains a live-session check [@adr-0003].

The architecture is simpler in process count and product surface, but tighter in protocol dependency. ADR 0003 states that Codex dynamic tools are experimental, Red requires Codex CLI 0.144.1 or newer, opts into the experimental app-server capability, and fails closed if the required contract is unavailable [@adr-0003]. The app-server implementation follows that by enabling experimental API capability during initialization and relying on direct protocol methods such as `thread/start`, `turn/start`, and app-server item notifications [@codex].

Future work on agent behavior should extend the direct [Codex App-Server Workflow](../../architecture/agent/codex-app-server-workflow) rather than reintroducing a generic backend selector. A new transport or backend would need a fresh decision because ADR 0003 intentionally removed backend and API-key setup choices in favor of one Codex app-server integration [@adr-0003].
