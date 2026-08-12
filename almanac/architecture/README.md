---
title: "Architecture"
summary: "Architecture pages map Red's runtime owners, boundaries, and cross-file flows, with subsystem hubs for editor, agent, Husk, LSP, plugins, sessions, startup, runtime assets, configuration, themes, and commands."
topics: [architecture, navigation]
sources:
  - id: topics
    type: file
    path: almanac/topics.yaml
  - id: editor-hub
    type: file
    path: almanac/architecture/editor/README.md
  - id: agent-hub
    type: file
    path: almanac/architecture/agent/README.md
  - id: husk-hub
    type: file
    path: almanac/architecture/husk/README.md
  - id: lsp-hub
    type: file
    path: almanac/architecture/lsp/README.md
  - id: plugin-hub
    type: file
    path: almanac/architecture/plugins/README.md
  - id: sessions-hub
    type: file
    path: almanac/architecture/sessions/README.md
---

# Architecture

Architecture pages explain Red's runtime ownership, subsystem boundaries, and cross-file flows. The topic graph treats architecture as the parent neighborhood for editor core, startup, configuration, runtime assets, plugins, Husk, LSP, agent integration, sessions, persistence, and validation-adjacent behavior [@topics]. Use this hub when you know the kind of code you are changing but not the specific subsystem page.

## Product And Editor Core

Start with [Red editor](../concepts/red-editor) for the product model, then read [Editor architecture](editor) when the change touches buffers, windows, input handling, rendering, plugin dispatch, LSP polling, recovery snapshots, or agent events [@editor-hub]. The editor subpages split that core into [Buffers and windows](editor/buffers-and-windows), [Rendering pipeline](editor/rendering-pipeline), [Text mutation boundary](editor/text-mutation-boundary), [LSP document sync](editor/lsp-document-sync), [Plugin host requests](editor/plugin-host-requests), [Syntax services](editor/syntax-services), and [Event loop](editor/event-loop).

Use [Runtime lifecycle](startup/runtime-lifecycle) and [Red command](../reference/cli/red-command) for startup branches, utility modes, file opening, crash resume, and detach or attach decisions. Use [Layered config recovery](configuration/layered-config-recovery) and [Default config](../reference/configuration/default-config) when startup behavior is shaped by defaults, user TOML, recovery diagnostics, or strict command-line overrides.

## Integrations

[Plugin architecture](plugins) is the entry point for bundled Husk plugins, runtime loading, the Red host API, plugin-owned resources, and constrained process or filesystem access [@plugin-hub]. From there, move to [Plugin lifecycle and reload](plugins/lifecycle-and-reload), [Red host API](plugins/red-host-api), [Resource ownership](plugins/resource-ownership), or [Process and filesystem boundaries](plugins/process-and-filesystem-boundaries) depending on the side effect being changed.

[Husk architecture](husk) covers the embedded and standalone scripting workspace: public embedding, packages and locks, extension tiers, and the Husk language server [@husk-hub]. [LSP architecture](lsp) covers server routing, process transport, editor document synchronization, completion, workspace edits, capabilities, configuration, and Husk LSP integration [@lsp-hub].

[Agent architecture](agent) is the entry point for Codex app-server integration, dynamic tools, proposal workspaces, reviewable edits, and proposal review operations [@agent-hub]. Keep it connected to [Reviewable agent edits](../concepts/reviewable-agent-edits), because the architecture is built around proposal-first mutation rather than direct agent writes.

## State, Assets, And Sessions

[Sessions architecture](sessions) separates detachable live owner processes from crash-recovery snapshots [@sessions-hub]. Use it with [Detach versus recovery](../concepts/sessions/detach-vs-recovery), [Detach IPC protocol](../reference/sessions/detach-ipc-protocol), [Detach and reattach](../guides/sessions/detach-reattach), and [Resume after crash](../guides/sessions/resume-after-crash).

[Runtime assets](runtime/runtime-assets) explains user, `RED_RUNTIME`, and embedded plugin/theme resolution. [Theme import](themes/theme-import) covers VS Code-compatible theme JSON. [Command discovery](commands/command-discovery) explains colon parsing, command palette rows, keymap-derived commands, and plugin command scope.
