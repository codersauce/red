# JSON-RPC 2.0 Stdio Plugins Plan

## Overview

This plan specifies first-class support for plugins written in any language that run as external child processes communicating with Red over `stdin` and `stdout` using **JSON-RPC 2.0** (newline-delimited UTF-8 JSON frames).

JSON-RPC plugins coexist seamlessly with existing Husk (`.hk` / `Husk.toml`) plugins and native companions:
- They share the same `PluginRegistry` (`src/plugin/registry.rs`) discovery, dependency ordering, lazy activation (`onCommand:*`, `onEvent:*`), status reporting (`Pending`, `Active`, `ActiveWithReloadError`, `Disabled`, `Quarantined`), and transactional hot reload.
- They share the same `RedPluginPolicy` and `RedHost` boundary (`src/plugin/runtime.rs`), translating RPC host calls (`red/execute`, `red/request`, `red/addCommand`, `red/on`, `red/setTimeout`, `red/cancelTimeout`, `red/log`, `red/snapshot`) into the exact same `PluginRequest` queue drained by `Editor::service_background` (`src/editor.rs`).
- The editor remains the sole mutator of buffers, undo history, UI surfaces, LSP state, agent state, and filesystem reconciliation.

---

## 1. Manifest & Configuration

### External Package Manifest (`red-plugin.toml`, `src/plugin/package.rs`)

A package may declare a `[rpc]` table in `red-plugin.toml`:

```toml
schema_version = 1

[plugin]
id = "my-rpc-plugin"
name = "My RPC Plugin"
version = "0.1.0"
red_api = "^0.18.0"

[rpc]
command = "bin/my-plugin"
args = ["--stdio"]

[rpc.commands]
x86_64-pc-windows-msvc = "bin/my-plugin.cmd"

[activation]
commands = ["MyRpcCommand"]
events = ["buffer:saved"]
```

- `PluginPackageManifest::validate` accepts `rpc` as a valid package entrypoint (`husk_manifest || entry || rpc || companion || !languages.is_empty()`), validates package-relative paths, verifies file existence, and validates optional per-target `artifacts` (HTTPS GitHub URLs + SHA-256 digests).
- `InstalledPlugin` includes `has_rpc: bool`.
- `finalize_runtime_config` (`src/main.rs`) and `PluginPackageManager` (`src/plugin/package.rs`) register enabled JSON-RPC plugin entrypoints into `config.plugins`.

### Direct User Config (`config.toml`, `src/config.rs`)

User `[plugins]` entries can point to:
- `.hk` single-file Husk plugins or `Husk.toml` packages,
- `red-plugin.toml` package manifests (Husk or JSON-RPC), or
- standalone executable files (outside `.hk` / `.toml`) launched directly as JSON-RPC stdio plugins.

---

## 2. Wire Protocol (JSON-RPC 2.0 over Newline-Delimited Stdio)

- **Framing**: One UTF-8 JSON-RPC 2.0 object per line (`\n`), bounded by `MAX_FRAME_BYTES` (8 MiB).
- **Streams**:
  - `stdin`: Red host $\rightarrow$ plugin process.
  - `stdout`: Plugin process $\rightarrow$ Red host.
  - `stderr`: Captured into a bounded 16 KiB diagnostic ring buffer and logged with `[PLUGIN:RPC]`.
- **Environment**:
  - `RED_PLUGIN_ID`: Plugin name/identifier.
  - `RED_PLUGIN_PROTOCOL`: `"jsonrpc-2.0"`.
  - `RED_HOST_API_VERSION`: Current `RED_HOST_API_VERSION`.
  - `RED_PLUGIN_DATA_DIR`: Namespaced plugin data directory.

### Host $\rightarrow$ Plugin Messages

| Method | Kind | Params | Result / Notes |
| :--- | :--- | :--- | :--- |
| `initialize` | Request | `{ plugin_id, red_api_version, data_dir, snapshots }` | Returns `{ commands?, events?, config_keys? }` or `{}` |
| `lifecycle/activate` | Request | `{ state?: Json }` | Runs activation (and imports state when `state` is non-null) |
| `lifecycle/deactivate` | Request | `{}` | Graceful teardown before unload or reload replacement |
| `lifecycle/exportState` | Request | `{}` | Returns `{ state: Json }` during hot-reload staging |
| `lifecycle/beforeExit` | Request | `{ snapshot: EditorStateSnapshot }` | Invoked during `PluginRegistry::before_exit` |
| `command/execute` | Request | `{ name, args, raw_args }` | Executes a registered plugin command |
| `event/notify` | Notification | `{ event, payload }` | Delivers subscribed editor/surface/process/timer events |
| `picker/event` | Notification | `{ handle, kind, item?, query?, action? }` | Delivers callback-scoped picker/confirm events (`selected`, `cancelled`, `changed`, `query`, `action`) |
| `composer/event` | Notification | `{ handle, kind, text? }` | Delivers callback-scoped composer/input events (`submitted`, `cancelled`) |

### Plugin $\rightarrow$ Host Messages

| Method | Kind | Params | Result / Effect |
| :--- | :--- | :--- | :--- |
| `red/execute` | Request / Notification | `{ action: String, args?: [Json] }` | Dispatches through `RedHost::execute`. For `OpenPicker`, `OpenConfirm`, `OpenComposer`, and `OpenInput`, allocates a host handle owned by the plugin and returns `handle` (`i32`). |
| `red/request` | Request | `{ action: String, args?: [Json] }` | Allocates `RequestId` in `RedPluginPolicy`, enqueues `PluginRequest`, and resolves the JSON-RPC request asynchronously when `Editor` calls `resolve_request`. |
| `red/addCommand` | Request / Notification | `{ name: String, metadata?: CommandMetadata }` | Registers a command in `RedPluginPolicy::commands`. |
| `red/on` | Request / Notification | `{ event: String }` | Subscribes the plugin to `event` in `RedPluginPolicy::event_listeners`. |
| `red/config` | Request / Notification | `{ key?: String }` | Binds a startup/reload `GetConfig` request delivered via `event/notify` (`config:loaded`) or `red/request`. |
| `red/snapshot` | Request | `{ name: String }` | Reads `viewport_layout`, `windows`, or `editor_info` from cached `RedHost` snapshots. |
| `red/setTimeout` | Request | `{ delay_ms: u64 }` | Schedules a host timer and returns `timer_id: String`. |
| `red/cancelTimeout` | Request / Notification | `{ timer_id: String }` | Cancels a scheduled host timer. |
| `red/log` | Notification | `{ message: String }` | Logs `[PLUGIN:RPC]` message. |

---

## 3. Runtime & Policy Unification (`src/plugin/runtime.rs`)

1. **Unified Handler Representation**:
   - Generalize `RedPluginPolicy` command, event, picker, composer, and pending-request entries to support both Husk `Callback`s and JSON-RPC plugin targets (`PluginCallbackTarget::Husk(Callback)` vs `PluginCallbackTarget::Rpc { plugin: String }`).
   - Preserve all ownership checks (`ensure_picker_owner`, `command_plugin`, `picker_plugin`, `composer_plugin`, `request_plugin`) and transactional reload staging (`staged_policy`, `teardown_policy`, `staged_effects`).
2. **Shared Host Action & Request Translation**:
   - Reuse `RedHost::execute` and `RedHost::request` for JSON-RPC `red/execute` and `red/request` calls by converting JSON arguments into `husk_runtime::Value` (`Value::from_json`), while allowing `OpenPicker`, `OpenConfirm`, `OpenComposer`, and `OpenInput` to register RPC-backed `PickerHandlers` and `ComposerHandlers` without requiring Husk callback closures.

---

## 4. JSON-RPC Process Supervisor (`src/plugin/rpc.rs`)

- Implement `RpcPluginHost` managing active JSON-RPC child processes per plugin.
- **Synchronous / Non-Blocking Bridge**:
  - During `initialize`, `activate`, `reload`, `execute_command`, `before_exit`, and `deactivate`, the host awaits bounded RPC request/response exchanges (and services any re-entrant Plugin $\rightarrow$ Host calls such as `red/addCommand`, `red/on`, `red/execute`, `red/request`, `red/log` issued while the plugin handles `initialize`, `activate`, or `command/execute`).
  - Background Plugin $\rightarrow$ Host messages emitted asynchronously (e.g., from plugin timers or async I/O inside the plugin process) are queued on an inbound channel and drained during `Runtime::poll_rpc_events` in `Editor::service_background`.
  - Unexpected process exit or protocol errors are reported and quarantined through `PluginRegistry::quarantine`, cleaning up all owned commands, listeners, pickers, composers, and subprocesses.

---

## 5. Extension Points Covered

1. **Static Package Contributions**: Lazy activation (`commands`, `events`), keymaps, languages/grammars/formatters, legacy session migrations.
2. **Lifecycle & Transactional Reload**: `initialize`, `lifecycle/activate`, `lifecycle/deactivate`, `lifecycle/exportState`, `lifecycle/beforeExit`.
3. **Commands**: `red/addCommand`, palette metadata, `editor`/`global` scope, `arguments` & `completions`.
4. **Events**: `red/on` for all editor, buffer, cursor, viewport, mode, theme, panel, workspace, window-bar, timer, process, filesystem-watcher, and agent events.
5. **UI Surfaces**: Row/tree panels, markdown/conversation text panels + modal composer, pickers, confirmations, inputs, composers, decorations, gutter signs, overlays, window bars, workspaces, messages, themes.
6. **Documents & Buffers**: `DocumentSnapshot`, `DocumentApply`, `DocumentUndo`, `GetBufferText`, `GetSelection`, `GetCursorPosition`, `SetCursorPosition`, `OpenLocation`, `OpenBuffer`, `OpenBufferById`, `OpenScratchBuffer`, `CloseScratchBuffer`.
7. **LSP**: `DocumentSymbols`, `WorkspaceSymbols`, `References`, `InlayHints`, `GoToDefinition`, `Hover`, `ShowLineDiagnostics`, `RefreshDiagnostics`.
8. **Filesystem & Git**: `FileOperation`, `ListDirectory`, `SearchWorkspacePaths`, `InvalidateWorkspacePaths`, `WatchDirectory`, `UnwatchDirectory`, `GetGitStatus`, `CancelGitStatus`.
9. **Agent**: Session lifecycle, prompts, context, commit message generation, model selection.
10. **Storage, Config, Timers & Subprocesses**: `GetStorage`/`SetStorage`, `GetConfig`, `SetTimeout`/`CancelTimeout`/`MonotonicTime`, `SpawnProcess`/`KillProcess`.
