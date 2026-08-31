---
title: "Plugin Lifecycle And Reload"
summary: "The plugin lifecycle is the registry-controlled path from discovery through activation, quarantine, callback delivery, and transactional reload."
topics: [architecture, plugins, husk, runtime]
sources:
  - id: registry
    type: file
    path: src/plugin/registry.rs
  - id: package
    type: file
    path: src/plugin/package.rs
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
  - id: system-doc
    type: file
    path: docs/PLUGIN_SYSTEM.md
  - id: api-doc
    type: file
    path: docs/PLUGIN_API.md
---

Plugin lifecycle and reload is the boundary between configured Husk plugin sources and the live editor. `PluginRegistry` owns discovery status, metadata, dependency ordering, activation, quarantine, command routing, callback failure isolation, and hot reload selection [@registry]. `Runtime` owns the Red-specific Husk VM host: it compiles plugin source, validates host calls, loads or reloads VM programs, records callback registrations, translates `red::execute` and `red::request` into editor-owned requests, and stages host effects during reload [@runtime]. The split lets Red keep plugin failures local while preserving the editor as the owner of buffers, UI resources, and service state.

## Discovery And Compatibility

Configured plugins enter the registry through `add(name, path)`. The registry records the source path, marks the plugin `Pending`, snapshots source and metadata modification times, and then discovers metadata from the nearest matching external `red-plugin.toml` package before falling back to legacy adjacent `package.json` metadata or minimal single-file metadata [@registry] [@package]. A metadata load failure quarantines that plugin immediately, but the registry still inserts minimal metadata so discovery of unrelated plugins can continue [@registry].

Activation checks are staged before Husk code runs. Dependencies must exist, required dependency versions must satisfy the dependent's semver requirements, and the compatibility range must match at least one Red host API version supported by this release [@registry]. Current packages express that range as `[plugin].red_api`; legacy `package.json` metadata expresses it as `red_api_version` [@package] [@registry]. The compatibility guide documents the same policy for plugin packages: malformed or incompatible ranges quarantine the owner while editor startup and unrelated plugins continue [@api-doc].

## Activation Order And States

Initialization sorts pending plugins by name and path, then defers a plugin until its declared dependencies are no longer pending [@registry]. If no progress is possible, the remaining dependency cycle is quarantined [@registry]. A plugin that passes metadata and dependency checks but declares lazy activation events or commands remains `Pending` until that trigger asks the registry to load it [@registry]. Eager plugins are read from embedded contents or the filesystem and loaded through `runtime.load_plugin_at`; success marks them `Active`, while source, compile, or activation failures produce a diagnostic `Quarantined` status [@registry].

The observable lifecycle states are `Pending`, `Active`, `ActiveWithReloadError`, `Disabled`, and `Quarantined` [@registry]. `ActiveWithReloadError` is used only when an already active plugin rejects a hot reload; the old VM, callbacks, commands, metadata, and state remain authoritative while the status records the failed replacement path and diagnostic [@registry].

## Runtime Failure Isolation

Runtime callback failures do not escape as global editor failures. Registry command execution first asks the runtime which plugin owns the command, then quarantines that owner if `execute_command` fails [@registry]. Event broadcast uses `runtime.notify_isolated`, gathers per-plugin errors, and quarantines only the failing owners [@registry]. Targeted plugin notifications, request callbacks, picker callbacks, and composer callbacks follow the same owner-specific quarantine pattern [@registry].

Callback-scoped picker and composer delivery is intentionally consumed on terminal paths. The registry comments and runtime implementation treat a failing terminal picker, composer, or request callback as resolved because the callback handle or request ID was already consumed and must not be retried accidentally [@registry] [@runtime]. The dialog details are covered by [Callback-scoped dialogs](../../concepts/plugins/callback-scoped-dialogs).

## Transactional Hot Reload

Hot reload polls filesystem-backed plugin source and metadata with a 250 ms debounce and skips private bundled plugin specifiers [@registry]. When a changed plugin is found, the registry expands the reload set to include dependents and reloads the selected set in dependency order [@registry]. The compatibility guide states the operational contract: a replacement VM is parsed, typechecked, activated, and migrated before it replaces the live program; a bad save leaves the previous program active and records `active_with_reload_error` [@api-doc].

The runtime makes that contract transactional. `load_plugin_at` compiles the replacement, calls `host.begin_reload()`, asks the Husk VM to reload the compiled plugin, then commits staged host effects only if reload succeeds; on failure it rolls back staged requests, logs, and timers, and removes the VM entry when the plugin was not previously loaded [@runtime]. During staging, process starts and kills are rejected, so a failed reload cannot leak or terminate a subprocess from `activate`, `state_import`, or `deactivate` [@runtime] [@api-doc].

## Shutdown And Teardown

The plugin system guide defines `activate`, optional `before_exit`, and optional `deactivate` as the lifecycle functions a plugin may expose [@system-doc]. At process exit, the registry calls `before_exit` only after initialization and passes a final editor snapshot to the runtime [@registry]. `deactivate_all` asks the runtime to deactivate all plugin VMs, clears host policy, and marks the registry uninitialized [@registry] [@runtime]. Unloading one plugin removes its command callbacks, event listeners, pending request callbacks, picker handlers, composer handlers, plugin state, and subprocesses [@runtime].

The lifecycle page connects directly to [Red host API](red-host-api), because host API validation happens before activation and host dispatch is the runtime side of plugin effects. Runtime asset resolution is the source selection step before registry discovery; see [Runtime assets](../runtime/runtime-assets). Bundled plugin source and pure core packages are introduced in [Bundled Husk plugins](../../concepts/plugins/bundled-husk-plugins).
