# Husk plugin compatibility

Red host API version `0.2.0` is defined by
[`src/plugin/host_api.json`](../src/plugin/host_api.json). That file is the canonical,
machine-readable list of execute actions, request actions, signatures, and introduction
versions. Runtime dispatch and the bundled-plugin corpus are checked against it in tests.

Plugin packages may declare a semver range in `red_api_version`. Red checks that range
before activation. A malformed or incompatible range quarantines that plugin and reports
the source path, stage, current API version, and this migration guide; editor startup and
unrelated plugins continue. While Red is pre-1.0:

- patch API releases fix behavior without intentionally changing signatures;
- minor API releases may add calls and fields, and may deprecate calls;
- removing or incompatibly changing a call requires a host-API minor bump, a change
  manifest entry, and a migration note.

Load runs parse, name resolution, and type checking against Red's host declarations
before activation. Diagnostics retain source spans and use stable families:
`HUSK-P0001` for parsing, `HUSK-T0001` for semantic/type errors, and `HUSK-A0001` for a
literal host call absent from the canonical schema. Literal host calls also check
required/optional arity (`HUSK-A0002`) and obvious literal argument types
(`HUSK-A0003`) against the machine-readable signature. `--no-typecheck` is an unsupported
development escape hatch; compatibility guarantees do not apply while it is enabled.

## Agent composer

Plugins that collect an agent request should call `OpenAgentComposer(title: String, id: i32, query: String, history: [String])`. The host owns multiline editing, wrapping, cursor movement, and history navigation; it does not send a callback for each keystroke. On submit it emits `composer:submitted:<id>` with the complete prompt as a JSON string, and on cancellation it emits `composer:cancelled:<id>`. These callbacks are delivered only to the plugin that opened the composer. Input is limited to 128 KiB so an escaping-heavy prompt remains within the ACP frame limit; an oversized paste leaves the current draft intact and shows a validation message. Enter submits, `Ctrl-j` or Shift-Enter inserts a newline, Escape or `Ctrl-c` cancels, and `Ctrl-p` / `Ctrl-n` moves through the supplied history while preserving the current draft.

`OpenAgentComposer` and its composer events were introduced in host API `0.2.0`. Plugins migrating from a picker-based prompt should declare `"red_api_version": "^0.2.0"`, replace the one-item `OpenDynamicPicker` call and its per-keystroke query callback with `OpenAgentComposer`, and handle the complete `composer:submitted:<id>` payload. A `^0.1.0` requirement intentionally does not match the new pre-1.0 minor API.

## Text panels

`CreateTextPanel`, `UpdateTextPanel`, and `AppendTextPanel` provide a source-backed conversation surface. `TextPanelBlock` accepts an `id`, `kind` (`user`, `agent`, `error`, or `text`), `format` (`plain` or `markdown`), and `text`; the host preserves the source while wrapping and rendering it for the current panel width. These calls were introduced in host API `0.2.0`.

## Quarantine and self-check

Plugins load independently. Source, version, dependency, compile, activation, and runtime
failures quarantine only their owner. `red --self-check` prints every bundled plugin's
status. Required plugin dependencies must be active or the dependent plugin is
quarantined with the dependency chain.

Plugin subprocesses inherit only the standard execution, locale, temporary-directory,
platform, and SSH-agent environment keys. Explicit environment overrides remain
allowlisted. Process stdin is limited to 16 MiB, raw output to 2 MiB, individual
streaming lines to 256 KiB, and pending process events to 16 (at most roughly 32 MiB
of payload); oversized output is
reported without letting an untrusted process grow editor memory indefinitely.

## Transactional reload and state

User plugin files are polled with a 250 ms debounce. A replacement VM is parsed,
typechecked, activated, and migrated before it replaces the live program. A bad save
leaves the previous callbacks and program active and records an `active_with_reload_error`
status. Host requests, editor actions, logs, and timers produced while staging are
published only after a successful swap. Starting or killing a process from reload-time
`activate`, `state_import`, or `deactivate` is rejected so a failed reload cannot leak
or terminate a subprocess; manage processes from an event or command callback instead.

State is intentionally explicit. A plugin that wants state carried across a successful
reload implements:

```husk
fn state_export() -> Json { /* return versioned state */ }
fn state_import(saved: Json) { /* validate or migrate saved state */ }
```

If either hook fails, the replacement is discarded. Successful replacement removes old
commands, event callbacks, pending requests, and VM state before the new registry becomes
authoritative. Plugins should clean up host-owned panels, timers, watchers, and processes
from `deactivate`.
