# ADR 0003: Direct Codex app-server integration

- Status: accepted
- Date: 2026-07-19
- Supersedes: ADR 0001

## Decision

Red integrates with an installed Codex CLI directly through
`codex app-server --stdio`. Red no longer ships or supports an ACP client,
generic ACP adapters, the OpenAI Responses companion, or the Codex ACP
translation companion.

Red owns app-server process lifecycle, JSONL framing, request correlation,
Codex threads and turns, cancellation, and dynamic-tool dispatch. The existing
proposal workspace remains the only supported write path. Threads run read-only
with native approvals denied and configured extension surfaces disabled.

Codex dynamic tools are currently experimental. Red requires Codex CLI 0.144.1
or newer, opts into the experimental app-server capability, and fails closed if
the required contract is unavailable.

## Rationale

The removed Codex companion already translated ACP into app-server calls.
Moving that client into core removes a process and protocol boundary while
preserving persistent conversations, streaming, cancellation, editor-aware
tools, and reviewable proposals.

`codex exec` is not an automatic fallback. Its one-shot automation surface
cannot provide Red's bidirectional live editor tools and proposal callbacks
without a workspace mirror and post-hoc diff import, which would weaken unsaved
buffer semantics and the review guarantee.

## Consequences

- Release archives contain one `red` binary.
- Users install and authenticate Codex separately.
- `[agent] command` may override the Codex executable; `args` and `env` remain
  direct process values with no shell expansion.
- `red --agent-check` verifies executable discovery and minimum version.
- App-server protocol changes are covered by a deterministic mock integration
  test and a minimum-version gate.
- Red supports one agent backend and no longer presents backend/API-key setup
  choices.
