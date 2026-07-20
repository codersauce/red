# Husk plugin compatibility

Red host API version `0.3.0` is defined by
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

## Command discovery metadata

`red::add_command(name, callback[, metadata])` accepts an optional `Json` object
with `title`, `category`, `description`, and `aliases: [String]`. Red uses these
fields to populate the command palette; aliases are search terms and do not
create alternate colon commands. The palette shows the exact, case-sensitive
`:Name` invocation when it is available and resolves keymaps from the user's
effective configuration. Existing two-argument registrations continue to work.

## Callback-scoped pickers

New pickers should use
`OpenPicker(title: String, items: [PickerItem], options: PickerOptions, handlers: PickerHandlers)`.
The host returns an opaque integer handle that may be passed to `UpdatePickerItems`,
`UpdatePickerQuery`, `UpdatePickerStatus`, and `ClosePicker`. Plugins
must not assign or interpret this handle.

```husk
red::execute("OpenPicker", "Themes", items, PickerOptions {
    placeholder: "Filter themes",
}, PickerHandlers {
    changed: theme_changed,
    cancelled: theme_cancelled,
    selected: theme_selected,
});
```

`PickerHandlers` accepts `selected`, `cancelled`, `changed`, `query`, and `action`
callbacks; unused handlers may be omitted. `changed`, `query`, and `action` can run
repeatedly. Selection and cancellation are terminal: the host consumes every handler for
that picker before invoking the terminal callback. Closing or replacing the dialog,
reloading its plugin, or unloading its plugin also releases the handlers. Stale handles
are ignored.

Callbacks are retained by the runtime and delivered only to the plugin that opened the
picker. They do not use global `picker:*:<id>` subscriptions. Picker items and callback
payloads use the declared `PickerItem`, `PickerCancelled`, and `PickerActionEvent` records;
the `PickerItem.data` field remains `Json` so a plugin can attach its own payload.

`OpenPicker` was added in host API `0.3.0`. Plugins using it should declare
`"red_api_version": "^0.3.0"`. The numeric-ID `OpenDynamicPicker` API remains
available for compatibility, but new plugins should not use it.

## Agent composer

Plugins that collect an agent request should call `OpenAgentComposer(title: String, id: i32, query: String, history: [String])`. The host owns multiline editing, wrapping, cursor movement, and history navigation; it does not send a callback for each keystroke. On submit it emits `composer:submitted:<id>` with the complete prompt as a JSON string, and on cancellation it emits `composer:cancelled:<id>`. These callbacks are delivered only to the plugin that opened the composer. Input is limited to 128 KiB so an escaping-heavy prompt remains within the Codex app-server frame limit; an oversized paste leaves the current draft intact and shows a validation message. Enter submits, `Ctrl-j` or Shift-Enter inserts a newline, Escape or `Ctrl-c` cancels, and `Ctrl-p` / `Ctrl-n` moves through the supplied history while preserving the current draft.

`OpenAgentComposer` and its composer events were introduced in host API `0.2.0`. Plugins migrating from a picker-based prompt should declare `"red_api_version": "^0.2.0"`, replace the one-item `OpenDynamicPicker` call and its per-keystroke query callback with `OpenAgentComposer`, and handle the complete `composer:submitted:<id>` payload. A `^0.1.0` requirement intentionally does not match the new pre-1.0 minor API.

`AgentArchiveSession(session_id: String)` was also introduced in host API `0.2.0`. Use it when Codex app-server has already stopped: pending proposals remain reviewable, and the host does not send an interrupt to a replacement process that may reuse the same session ID. Use `AgentCloseSession(session_id: String)` for a live session that should be closed normally.

`AgentPrompt` automatically attaches bounded editor context containing the active visual selection or a roughly 80-line cursor excerpt, unsaved-state metadata, cursor/range, and intersecting diagnostics. Files outside the workspace, ignored paths, common credential/secret filenames, and binary buffers are omitted. Plugins that need to inspect or explicitly override this context can call `GetAgentContext(callback)` and `AgentPromptWithContext(session_id: String, text: String, context: Json)`; the context object accepts `uri` and `text` fields and is included in the direct Codex turn.

## Text panels

`CreateTextPanel`, `UpdateTextPanel`, and `AppendTextPanel` provide a source-backed conversation surface. `TextPanelBlock` accepts an `id`, `kind` (`user`, `agent`, `error`, or `text`), `format` (`plain` or `markdown`), and `text`; the host preserves the source while wrapping and rendering it for the current panel width. These calls were introduced in host API `0.2.0`.

`PanelConfig` may include `composer: Json { placeholder: String, rows: i32 }` for a persistent footer composer and `header_actions: [Json { id: String, label: String, compact_label?: String }]` for clickable, right-aligned header controls. Header actions emit `panel:event:<id>` using their configured `id`; compact labels are selected automatically on narrow panels, with the rightmost actions retained when space is especially limited. Focus the footer with `FocusTextPanelComposer(id)`, update its enabled/status state with `SetTextPanelComposerState(id, enabled, status?)`, and clear its draft with `ClearTextPanelComposer(id)`. A focused composer supports Unicode-safe editing, paste, wrapping, click-to-position cursor movement, `Ctrl-p`/`Ctrl-n` local history, Enter to submit, and `Ctrl-j` or Shift-Enter for a newline. It emits `panel:event:<id>` with `action: "submit"` and the complete `text`; other footer actions include `composer_focus`, `composer_blur`, `interrupt`, `clear`, `new`, `history`, and `close`. `SetPanelVisible(id, visible)` hides or restores a panel without discarding its blocks, scroll position, or draft. Replacing text-panel blocks with an empty list resets scrolling and restores tail-following. Footer panels shrink on narrow terminals while preserving an editor viewport.

Codex app-server updates other than assistant text chunks are forwarded to plugins as `agent:activity` with the normalized `update` payload. Core editor-tool calls also emit this event with `session_update: "editor_tool"`, `status: "in_progress"`, and a concise `title` such as `Opening src/main.rs` or `Proposing 2 edit(s) in src/main.rs`. This allows status/tool/plan progress to be displayed without treating it as transcript text.

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

## Dynamic JSON boundary

`Json` remains intentional for persisted plugin state, arbitrary user configuration,
external process data, and plugin-defined payloads such as `PickerItem.data`. Values with
a host-defined shape should use nominal records instead. Picker callbacks are the first
migrated slice; request results, editor events, styles, panel values, and the remaining
bundled-plugin helpers will move incrementally as their host schemas become canonical.

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
