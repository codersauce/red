---
title: "Plugin Architecture"
summary: "Plugin architecture routes readers through runtime loading, the Red host API, editor-owned resources, process and filesystem limits, bundled plugins, and maintainer workflows."
topics: [architecture, plugins, host-api, husk]
sources:
  - id: registry
    type: file
    path: src/plugin/registry.rs
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
  - id: host-api
    type: file
    path: src/plugin/host_api.json
  - id: system-doc
    type: file
    path: docs/PLUGIN_SYSTEM.md
  - id: api-doc
    type: file
    path: docs/PLUGIN_API.md
---

# Plugin Architecture

Red plugin work crosses runtime asset discovery, Husk execution, versioned host calls, editor-owned resources, and constrained process and filesystem access. The registry and runtime load plugin metadata and Husk entrypoints, while the host API schema and plugin documentation define the calls plugins can make back into Red [@registry] [@runtime] [@host-api] [@system-doc] [@api-doc]. Use this hub to choose the narrow page for the part of the plugin system you need to change.

## Reading Order

Start with [Plugin Lifecycle And Reload](lifecycle-and-reload) for discovery, activation, quarantine, callback delivery, and hot reload. Then read [Red Host API](red-host-api) for the versioned call boundary between Husk plugins and editor-owned operations.

Use [Resource Ownership](resource-ownership) when plugin work touches panels, workspaces, window bars, overlays, decorations, gutter signs, pickers, or composers. Use [Process And Filesystem Boundaries](process-and-filesystem-boundaries) when plugin code starts child processes or reads and writes workspace files.

[Command Discovery](../commands/command-discovery) covers plugin command metadata, palette rows, colon command collisions, keymap shortcuts, and panel-global command scope.

[Bundled Husk Plugins](../../concepts/plugins/bundled-husk-plugins) explains how shipped plugins relate to embedded runtime assets and pure Husk packages. [Callback-Scoped Dialogs](../../concepts/plugins/callback-scoped-dialogs) explains the handle-based picker and composer model used by plugin callbacks.

Use [Official Language Pack Distribution](../../decisions/plugins/language-pack-distribution) when external plugin work touches first-party language-pack cataloging, release artifact boundaries, or native grammar approval.

For exact lookup, use [Host API](../../reference/plugins/host-api). For a task-oriented workflow, use [Write A Husk Plugin](../../guides/plugins/write-a-husk-plugin).
