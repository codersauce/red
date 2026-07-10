# Husk plugin compatibility

Red host API version `0.1.0` is defined by
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
literal host call absent from the canonical schema. `--no-typecheck` is an unsupported
development escape hatch; compatibility guarantees do not apply while it is enabled.

## Quarantine and self-check

Plugins load independently. Source, version, dependency, compile, activation, and runtime
failures quarantine only their owner. `red --self-check` prints every bundled plugin's
status. Required plugin dependencies must be active or the dependent plugin is
quarantined with the dependency chain.

## Transactional reload and state

User plugin files are polled with a 250 ms debounce. A replacement VM is parsed,
typechecked, activated, and migrated before it replaces the live program. A bad save
leaves the previous callbacks and program active and records an `active_with_reload_error`
status.

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
