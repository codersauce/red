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
  - id: arborium-decision
    type: file
    path: almanac/decisions/plugins/arborium-language-source.md
  - id: language-pack-release
    type: file
    path: almanac/guides/plugins/release-language-pack.md
  - id: preferences-store
    type: file
    path: almanac/architecture/preferences/preferences-store.md
  - id: host-api-reference
    type: file
    path: almanac/reference/plugins/host-api.md
  - id: plugin-host-requests
    type: file
    path: almanac/architecture/editor/plugin-host-requests.md
  - id: resource-ownership
    type: file
    path: almanac/architecture/plugins/resource-ownership.md
  - id: callback-dialogs
    type: file
    path: almanac/concepts/plugins/callback-scoped-dialogs.md
---

# Plugin Architecture

Red plugin work crosses runtime asset discovery, Husk execution, versioned host calls, editor-owned resources, and constrained process and filesystem access. The registry and runtime load plugin metadata and Husk entrypoints, while the host API schema and plugin documentation define the calls plugins can make back into Red [@registry] [@runtime] [@host-api] [@system-doc] [@api-doc]. Use this hub to choose the narrow page for the part of the plugin system you need to change.

## Reading Order

Start with [Plugin Lifecycle And Reload](lifecycle-and-reload) for discovery, activation, quarantine, callback delivery, and hot reload. Then read [Red Host API](red-host-api) for the versioned call boundary between Husk plugins and editor-owned operations.

Use [Resource Ownership](resource-ownership) when plugin work touches panels, workspaces, window bars, overlays, decorations, gutter signs, pickers, or composers. Use [Process And Filesystem Boundaries](process-and-filesystem-boundaries) when plugin code starts child processes or reads and writes workspace files.

[Command Discovery](../commands/command-discovery) covers plugin command metadata, palette rows, colon command collisions, keymap shortcuts, and panel-global command scope.

[Bundled Husk Plugins](../../concepts/plugins/bundled-husk-plugins) explains how shipped plugins relate to embedded runtime assets and pure Husk packages. [Callback-Scoped Dialogs](../../concepts/plugins/callback-scoped-dialogs) explains the handle-based picker and composer model used by plugin callbacks.

## Host API Reading Map

Use [Red Host API](red-host-api) for the architecture of `red::execute` and `red::request`: schema validation, runtime dispatch, compatibility checks, and why plugin effects return to editor-owned requests [@host-api] [@runtime]. Use [Plugin Host API](../../reference/plugins/host-api) when you need exact host API versions, call names, signatures, introduction versions, and accepted compatibility targets [@host-api-reference].

The host API does not make plugins direct editor mutators. [Plugin Host Requests](../editor/plugin-host-requests) explains the typed editor message boundary that receives runtime requests, while [Resource Ownership](resource-ownership) explains how editor managers own long-lived UI resources after plugins describe them [@plugin-host-requests] [@resource-ownership]. [Callback-Scoped Dialogs](../../concepts/plugins/callback-scoped-dialogs) is the concept page for handle-based picker, composer, input, and confirmation callbacks inside that broader host API surface [@callback-dialogs].

Use [Preferences Store](../preferences/preferences-store) when plugin work persists user or plugin JSON state, because plugin storage is a plugin-owned namespace inside the shared preferences file rather than crash-recovery state [@preferences-store].

Use [Official Language Pack Distribution](../../decisions/plugins/language-pack-distribution) when external plugin work touches first-party language-pack cataloging, release artifact boundaries, or native grammar approval. Use [Release A Language Pack](../../guides/plugins/release-language-pack) for the operational tag, packaging, catalog-publication, and Red install verification path [@language-pack-release]. Use [Arborium Language Pack Source](../../decisions/plugins/arborium-language-source) when the question is grammar-inventory import, generated query overlays, or why Arborium remains a build-time source rather than a runtime aggregate package [@arborium-decision].

For exact lookup, use [Host API](../../reference/plugins/host-api). For a task-oriented workflow, use [Write A Husk Plugin](../../guides/plugins/write-a-husk-plugin).
