---
title: "JSON-RPC Plugin Boundary"
summary: "Red should add language-agnostic JSON-RPC plugins through a bounded async protocol, not by forwarding the full Husk host API through blocking callbacks."
topics: [decisions, plugins, host-api, json-rpc-plugins, safety]
sources:
  - id: comparison-transcript
    type: conversation
    path: /Users/fcoury/.claude/projects/-Users-fcoury-code-red/873e2737-f14e-4a09-8563-fd1554ff90d4.jsonl
  - id: current-registry
    type: file
    path: src/plugin/registry.rs
  - id: current-api-doc
    type: file
    path: docs/PLUGIN_API.md
---

# JSON-RPC Plugin Boundary

Red's first language-agnostic plugin protocol should use a bounded asynchronous JSON-RPC process boundary with a small typed host surface, not a generic bridge that exposes the full Husk host API through blocking editor-thread callbacks. This decision records the durable outcome of the PR comparison between the broad `feat/jsonrpc-plugins` approach and the narrower `fcoury/json-rpc-plugins` approach: prefer the separate async connection model, then port selected capabilities only when they can preserve editor responsiveness, ownership, and failure isolation [@comparison-transcript].

## Status

This is a design decision, not a description of current `main` behavior. The current committed plugin surface is still the Husk host API at `0.18.0`, with compatibility and host-call validation documented through `src/plugin/registry.rs` and `docs/PLUGIN_API.md` [@current-registry] [@current-api-doc]. If JSON-RPC plugins are merged later, update the plugin architecture, host API reference, and this decision together.

## Context

The compared PRs made different architectural bets. The broad branch added a JSON-RPC path inside the existing Husk runtime and mapped external processes onto a generic `red/execute` and `red/request` bridge. That exposed more of Red quickly, including picker, composer, timer, config, state, release-artifact, and hot-reload state-transfer ideas, but the review found editor-thread waits, event round trips, loose method aliases, weak validation, and documentation drift as structural risks [@comparison-transcript].

The narrower branch introduced a separate RPC connection owned by the plugin registry. It used JSON-RPC 2.0 envelopes, bounded frame and queue limits, request correlation, write timeouts, strict initialization checks, a small set of typed host methods, and one-way event notifications. The review still found gaps, especially inline command execution that could wait up to 30 seconds, full environment inheritance, missing pickers and state transfer, and a single-threaded example client, but its transport boundary better matched Red's safety model [@comparison-transcript].

## Decision

JSON-RPC plugins should be modeled as standalone process plugins with their own bounded connection state, not as another callback target inside the Husk VM. Red should validate initialization before registering commands or event subscriptions, keep event delivery as notifications, treat application errors as recoverable protocol responses, and quarantine plugins only for transport failures or contract violations that make the connection unsafe [@comparison-transcript].

The first merged surface should stay intentionally small and typed. Document snapshot, document apply, document undo, host-rendered text panels, and statusline contributions are suitable first methods because they preserve the existing rule from [Plugin Resource Ownership](../../architecture/plugins/resource-ownership): plugins describe intent while editor managers own buffers, rendering, dirty tracking, undo history, and long-lived UI state [@comparison-transcript]. Broader features from the abandoned broad branch, such as pickers, timers, config, release artifacts, environment allowlisting, and hot-reload state transfer, should be added as explicit v2 host methods only when their lifetime, ownership, and failure modes are specified [@comparison-transcript].

Husk command precedence should remain stable while JSON-RPC is introduced. If a lazy Husk plugin and a pending RPC plugin declare the same command, Husk keeps discovery and dispatch precedence; the reviewed narrow branch had to be fixed to preserve that rule after command lookup crossed plugin modalities [@comparison-transcript].

## Consequences

JSON-RPC plugin work must budget background draining and process IO. A plugin that continuously emits unsolicited frames cannot be allowed to starve `Editor::service_background`, and a fast stdout producer cannot grow host memory without bounded backpressure [@comparison-transcript]. This makes per-tick frame or time budgets, bounded channels, and outstanding-request limits part of the architecture, not optional robustness work.

Command execution still needs a nonblocking editor strategy before merge. The preferred branch serviced host calls while waiting for `command/execute`, but the review still called out that ordinary editor input was blocked during the command timeout window [@comparison-transcript]. A future implementation should either shorten and isolate that wait, make commands resumable from the event loop, or otherwise prove that an RPC command cannot freeze interactive editing.

Reload and shutdown must keep failure identity precise. If an existing RPC process dies during state export, deactivate, or reload, Red should remove its command registrations and transition to quarantine rather than leaving an `ActiveWithReloadError` state that still points commands at a dead process [@comparison-transcript]. If state transfer is added later, failure should preserve the old live process only when the process is still alive and its command registrations still point to a working owner.

The process environment is a security boundary. The broad branch's environment allowlist was worth keeping, while the narrower branch inherited the full environment and resolved bare commands through `PATH` [@comparison-transcript]. Future JSON-RPC process launch should align with [Plugin Process And Filesystem Boundaries](../../architecture/plugins/process-and-filesystem-boundaries): explicit commands, direct argument passing, bounded IO, and a minimal inherited environment.

Protocol documentation and conformance fixtures should land with the implementation. The preferred branch's standalone protocol document, envelope fixture, and cross-platform integration tests were part of why it was the better base, while the broad branch mixed undocumented lifecycle methods, aliases, and response shapes [@comparison-transcript]. Future work should keep the protocol spec, fixtures, examples, host API version, change manifest, and registry behavior in the same review.

This decision links back to [Plugin Lifecycle And Reload](../../architecture/plugins/lifecycle-and-reload) and [Red Host API](../../architecture/plugins/red-host-api). It constrains future JSON-RPC work without changing the current Husk host API contract.
