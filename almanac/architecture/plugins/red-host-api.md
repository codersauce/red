---
title: "Red Host API"
summary: "The Red host API is the versioned Husk bridge that statically validates plugin calls and dispatches them into editor-owned requests."
topics: [plugins, host-api, husk]
sources:
  - id: schema
    type: file
    path: src/plugin/host_api.json
  - id: api
    type: file
    path: src/plugin/api.rs
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
  - id: api-doc
    type: file
    path: docs/PLUGIN_API.md
---

The Red host API is the versioned native module that Husk plugins call as `red::...`. Its machine-readable source of truth is `src/plugin/host_api.json`, which declares host API version `0.4.0` and lists each `execute` and `request` call with a name, kind, signature, and introduction version [@schema]. Red embeds that schema, validates literal host calls before activation, and dispatches accepted calls through the plugin runtime into editor-owned `PluginRequest` values or immediate helper functions [@api] [@runtime]. This boundary is why plugins can extend Red while the editor remains the owner of buffer mutation, UI resources, filesystem operations, and service state.

## Canonical Contract

`host_api.json` is the contract plugin authors and Red maintainers should check before changing host calls. It contains only `execute` and `request` entries, and tests assert that the schema version matches `RED_HOST_API_VERSION`, that every call has a non-empty signature and introduction version, and that runtime-dispatched host calls are present in the schema [@api]. The compatibility guide states the same rule in prose: runtime dispatch and the bundled plugin corpus are checked against the canonical schema [@api-doc].

The runtime separately declares the `red` module that Husk typechecking sees. `RED_HOST_DECLARATIONS` defines records such as `PickerItem`, `PickerHandlers`, `ComposerHandlers`, and `RuntimeAssetEntry`, then exposes host functions including `red::add_command`, `red::on`, `red::execute`, `red::request`, snapshots, state helpers, string helpers, color helpers, and internal bundled-core bridges [@runtime]. Compile-time host declarations make the Husk program typecheck, while `host_api.json` governs the literal action/request names and compatibility metadata [@api] [@runtime].

## Static Validation

The host API validator walks the parsed Husk AST looking for literal `red::execute("Action", ...)` and `red::request("Action", callback, ...)` call sites [@api]. For each literal action it checks presence in `HOST_API`, required and optional arity derived from the signature, and obvious literal argument categories such as strings, booleans, numbers, arrays, objects, and `Json` [@api]. Unknown calls produce `HUSK-A0001`, arity errors produce `HUSK-A0002`, and literal type mismatches produce `HUSK-A0003`, with diagnostics pointing back to the plugin source and `docs/PLUGIN_API.md` [@api].

Validation runs in `compile_plugin_source` after Red compiles the plugin with Red host declarations and the legacy plugin semantic profile [@runtime]. If typechecking is disabled for development, Red can compile through the legacy compatibility path, but the API guide calls `--no-typecheck` unsupported for compatibility guarantees [@api-doc].

## Dispatch Shape

The runtime host receives module calls and routes `red::execute` and `red::request` by the action string [@runtime]. `execute` calls are fire-and-forget unless the action intentionally returns an immediate value such as a picker handle, composer handle, process ID, or timer ID [@runtime]. `request` calls allocate an opaque `RequestId`, store the callback under the owning plugin, send an editor request with that ID, and remove the pending callback if request construction fails [@runtime].

Many host actions translate directly into `PluginRequest` messages, such as panel updates, picker updates, text panel updates, LSP symbol requests, runtime asset listings, workspace file operations, and agent actions [@runtime]. Immediate helper functions stay inside the host, including command registration, event subscription, plugin-local state helpers, string and JSON utilities, Unicode/display helpers, and the internal Git and Neo-tree core bridges [@runtime].

## Version And Compatibility Policy

The current host API version is `0.4.0` in both the schema and registry constant [@schema] [@api]. Plugin metadata may declare a semver `red_api_version` range; the registry rejects malformed or incompatible ranges before activation [@api-doc]. While Red remains pre-1.0, the documented policy is that patch releases fix behavior without intentional signature changes, minor releases may add or deprecate calls and fields, and removals or incompatible call changes require a host-API minor bump, a change manifest entry, and a migration note [@api-doc].

Callback-scoped pickers and composers illustrate this evolution. The schema records `OpenPicker` and `OpenComposer` as `0.3.0` calls and `OpenInput` and `OpenConfirm` as `0.4.0` calls [@schema]. The API guide keeps legacy numeric picker and composer calls available for compatibility while directing new plugins to handler-record APIs [@api-doc]. For lookup details, use [Plugin host API](../../reference/plugins/host-api); for lifecycle consequences of incompatible calls, use [Plugin lifecycle and reload](lifecycle-and-reload).
