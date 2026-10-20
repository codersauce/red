---
title: "Plugin Process And Filesystem Boundaries"
summary: "Plugin process and filesystem boundaries keep Husk plugin side effects permissioned, bounded, workspace-confined, and reconciled through editor-owned requests."
topics: [architecture, plugins, host-api, filesystem, safety]
sources:
  - id: process
    type: file
    path: src/plugin/process.rs
  - id: filesystem
    type: file
    path: src/plugin/filesystem.rs
  - id: config
    type: file
    path: src/config.rs
  - id: api-doc
    type: file
    path: docs/PLUGIN_API.md
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
---

Plugin process and filesystem boundaries are Red's guardrails for host actions that can affect the user's machine outside ordinary editor memory. Process launch is an immediate runtime action checked against per-plugin permissions, bounded output limits, environment filtering, and plugin-owned process IDs [@process] [@runtime]. Filesystem mutation is a structured request that resolves paths inside the current workspace and returns an outcome for editor reconciliation [@filesystem]. These boundaries sit below the [Red host API](red-host-api) and above [plugin lifecycle and reload](lifecycle-and-reload), so plugin failures stay local and high-risk side effects fail closed.

## Process Permission Is Exact And Per Plugin

Process permissions come from `Config.plugin_permissions`, whose `PluginPermissions` struct currently contains a `process` allowlist of executable names [@config]. The config comment states the important rule: entries are matched exactly against the requested command, and Red does not invoke a shell when launching plugin processes [@config]. `ProcessManager::spawn` enforces that rule by calling `require_command_permission` before constructing `std::process::Command` [@process].

A permitted command is not a general shell grant. `ProcessSpawnOptions` separates `command`, `args`, optional `cwd`, optional `stdin`, explicit environment additions, and `raw_output`; the process manager calls `Command::new(&options.command)` and passes arguments directly [@process]. The compatibility guide states the same operational guarantee for plugin authors: allowed executables are bounded subprocesses, not shell syntax expansion [@api-doc].

## Child IO Is Bounded

Each plugin may have at most 16 active processes, and process events are queued through a bounded sync channel with at most 16 pending events [@process]. Stdin is limited to 16 MiB, raw output is limited to 2 MiB, and line-oriented output is limited to 256 KiB per line [@process]. Oversized output becomes a `ProcessEvent::Error` instead of unbounded editor memory growth [@process].

The process manager clears the child's inherited environment, restores only a fixed set of standard execution, locale, temporary-directory, platform, and SSH-agent keys, and accepts explicit overrides only for an allowlisted set such as Git editor/pager variables, locale variables, `NO_COLOR`, and `RED_PROCESS_EDITOR_CONTENT` [@process]. That filtering matters because plugin subprocesses can observe environment variables even when they do not otherwise touch Red state.

## Process IDs Are Owner Capabilities

`spawn` returns a UUID process ID and records the plugin name with its kill sender [@process]. `kill` first requires process permissions, then silently ignores missing IDs or IDs owned by another plugin [@process]. `shutdown_plugin` kills all active processes for one plugin and drops its pending process events, while `Drop` shuts down all remaining processes for the manager [@process].

Reload staging adds another boundary. `SpawnProcess` and `KillProcess` are rejected while a plugin reload is staged, so a replacement VM cannot leak or terminate a subprocess before it successfully replaces the live plugin [@runtime]. The API guide repeats that operational rule and directs plugin authors to manage processes from commands or event callbacks rather than from reload-time lifecycle hooks [@api-doc].

## Filesystem Operations Are Structured Requests

The filesystem boundary is intentionally not a generic path API. `FileOperation` accepts a JSON operation whose `kind` must be one of create, create file, create directory, rename, move, copy, delete, trash, restore, undo trash, or stat [@filesystem]. The public API guide documents the same supported kinds, required fields, and result shape for plugin authors [@api-doc].

All file operation paths are interpreted relative to the active editor workspace. `apply_file_operation_inner` canonicalizes the workspace root and dispatches each supported operation through helpers that call `resolve_workspace_path` [@filesystem]. That resolver rejects absolute paths, parent traversal, root and prefix components, and existing parent directories that canonicalize outside the workspace [@filesystem]. Mutation helpers also refuse to modify the workspace root, refuse implicit overwrites, and reject self or descendant copy and move destinations [@filesystem].

## Mutation Results Feed Editor Reconciliation

`apply_file_operation` never lets a filesystem error escape as a panic. It returns a `FileOperationOutcome` with a JSON payload containing `ok: false` and an error string when validation or mutation fails [@filesystem]. Successful rename and delete-like operations also carry native `renames` and `removals` lists for the editor side to reconcile open buffers and rendered state after the filesystem change [@filesystem].

Create supports bounded brace expansion with a maximum of 256 expanded paths, and duplicate expanded destinations are rejected [@filesystem]. Trash restoration is platform-dependent because only some system trash APIs expose stable identities; unsupported platforms return a structured error [@filesystem]. These details make the request usable for project-tree plugins while preserving a narrow failure surface.

## How The Boundaries Fit Together

Configuration grants the minimal process allowlist, runtime dispatch enforces command ownership and reload restrictions, and the filesystem helper confines structured operations to the workspace [@config] [@runtime] [@filesystem]. Plugin authors should use [Write a Husk plugin](../../guides/plugins/write-a-husk-plugin) for the authoring workflow, [Plugin host API](../../reference/plugins/host-api) for exact call lookup, and [Default Config](../../reference/configuration/default-config) for the default permissions that ship with Red.
