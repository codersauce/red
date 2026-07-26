---
title: "Write A Husk Plugin"
summary: "This guide shows how to add or update a Red Husk plugin with metadata, host API calls, resources, permissions, and validation."
topics: [guides, plugins, husk, host-api]
sources:
  - id: example-source
    type: file
    path: examples/example-plugin/index.hk
  - id: example-package
    type: file
    path: examples/example-plugin/package.json
  - id: system-doc
    type: file
    path: docs/PLUGIN_SYSTEM.md
  - id: api-doc
    type: file
    path: docs/PLUGIN_API.md
  - id: schema
    type: file
    path: src/plugin/host_api.json
---

Use this guide when adding or revising a Red plugin written in Husk. A complete plugin has Husk source that registers commands or events, optional package metadata that declares compatibility, host calls that match the current Red API, and any process permissions needed by its runtime behavior [@system-doc] [@api-doc]. By the end, the plugin should load through Red's plugin lifecycle, expose only the resources it needs, and pass the same validation commands used for bundled plugin work.

## Start From A Minimal Source File

Create or update the plugin's `.hk` entry file first. The example plugin defines `pub fn activate()`, registers `ExampleCommand` with `red::add_command`, subscribes to `editor:ready` with `red::on`, and implements callbacks that call `red::execute("Print", ...)` and `red::log(...)` [@example-source]. That shape matches the lifecycle described in the plugin system guide: `activate` runs when Red initializes plugins, while `before_exit` and `deactivate` are optional hooks [@system-doc].

Keep the first version small enough to prove the lifecycle. Register one command, one event subscription, or one request callback, then expand. Direct `:Name` invocation requires the exact, case-sensitive registered command name, and built-in commands take precedence over plugin commands with the same name [@system-doc].

## Add Metadata And API Compatibility

Add a `package.json` beside filesystem-backed plugin source when the plugin needs metadata. The example metadata includes `name`, `version`, `description`, `author`, `license`, `main`, `keywords`, repository information, Red engine information, `red_api_version`, capabilities, activation events, and a simple configuration schema [@example-package]. Red checks `red_api_version` before activation and quarantines malformed or incompatible packages while startup and unrelated plugins continue [@api-doc].

For this codebase, target the current host API unless there is a clear compatibility reason not to. `src/plugin/host_api.json` declares version `0.4.0`, and the API guide recommends `"red_api_version": "^0.4.0"` for plugins targeting the current release [@schema] [@api-doc]. Use [Plugin host API](../../reference/plugins/host-api) for exact schema lookup and [Red host API](../../architecture/plugins/red-host-api) for how validation and dispatch work.

## Choose Host Calls Deliberately

Use `red::execute` for fire-and-forget host actions and `red::request` for actions that return a value through a callback [@system-doc]. The compatibility guide is the author-facing behavior reference, but the machine-readable schema is the exact list of action names, request names, signatures, and introduction versions [@schema] [@api-doc].

Prefer callback-scoped dialogs for new picker and composer work. `OpenPicker`, `OpenComposer`, `OpenInput`, and `OpenConfirm` store callback handles with the owning plugin and avoid global synthetic event names [@api-doc]. Use panels, text panels, workspaces, overlays, window bars, decorations, and gutter signs when the plugin needs persistent UI resources; those surfaces are editor-owned resources keyed by IDs or namespaces, as described in [Plugin resource ownership](../../architecture/plugins/resource-ownership).

## Add Process Or Filesystem Access Only When Needed

If the plugin launches external commands, add a narrow process allowlist under `[plugin_permissions.<plugin>]` in configuration. Process permissions match the requested command exactly, and Red does not invoke a shell [@api-doc]. Plugin subprocess output, stdin, and pending process events are bounded, and only allowlisted environment overrides are accepted [@api-doc].

Use `FileOperation` for project-tree mutation instead of inventing ad hoc file access. It supports structured create, rename, move, copy, delete, trash, restore, undo trash, and stat operations inside the active workspace [@api-doc]. Mutation paths must be workspace-relative, and the host rejects absolute paths, parent traversal, workspace-root mutation, symlink escapes, self or descendant copies, and implicit overwrites [@api-doc]. The runtime boundary is detailed in [Plugin process and filesystem boundaries](../../architecture/plugins/process-and-filesystem-boundaries).

## Preserve Reload Safety

Design reload behavior before adding long-lived state. Red reloads filesystem plugins transactionally: a replacement VM is parsed, typechecked, activated, and migrated before it replaces the live program, and a bad save leaves the previous program active with an `active_with_reload_error` status [@api-doc]. If a plugin needs state to survive a successful reload, implement `state_export()` and `state_import(saved: Json)` and validate or migrate the saved payload there [@api-doc].

Do not spawn or kill processes from reload-time `activate`, `state_import`, or `deactivate`. Red rejects process starts and kills during staged reload so a failed replacement cannot leak or terminate a subprocess [@api-doc]. Use a command callback, event callback, or request callback after activation for process management.

## Validate The Plugin

Run the validation commands that match the plugin's location and risk. The plugin system guide lists the broad project checks:

```shell
cargo test --workspace
cargo run -p husk-cli -- test --locked plugins/git_core
cargo run -p husk-cli -- test --locked plugins/neotree_core
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --self-check
cargo run -- --runtime-files
```

`red --self-check` reports bundled plugin status, and `red --runtime-files` should list `.hk` plugins only [@system-doc]. For a new user plugin, also open Red with the plugin configured, run the registered command, exercise any event or UI surface, and check that incompatible metadata or source errors quarantine only that plugin as described in [Plugin lifecycle and reload](../../architecture/plugins/lifecycle-and-reload).
