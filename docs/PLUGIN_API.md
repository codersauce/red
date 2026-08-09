# Husk plugin compatibility

Red host API version `0.8.0` is defined by
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
(`HUSK-A0003`) against the machine-readable signature. Invalid static plugin
annotations use `HUSK-A0004`. `--no-typecheck` is an unsupported development
escape hatch; compatibility guarantees do not apply while it is enabled.

Red `0.8.0` retains the complete `0.4.0`, `0.6.0`, and `0.7.0` contracts, so existing
packages that declare those minors continue to load. New packages should target
`"red_api_version": "^0.8.0"`.

## Scratch-buffer workflows

`OpenScratchBuffer(callback, name, text, commands?)` accepts optional `submit` and
`cancel` plugin command names. In a managed scratch buffer, `:w` and `:wq` invoke the
submit command without writing the display name to disk, while `:q` and `:q!` invoke
the cancel command without quitting Red. `Save`, `Quit`, and configured key bindings
follow the same routing. The options were added in host API `0.8.0`; calls using the
original three required arguments remain compatible.

## External package primitives

`CompanionCall(callback, method, params, timeout_ms?)` lazily starts the calling
package's declared native companion and exchanges bounded JSON-lines RPC
messages. Red owns the process, matches responses to requests, enforces the
timeout, and stops the process when the package or editor shuts down.

`DocumentSnapshot(callback, path?)` returns the current text and revision for an
open document. `DocumentApply(callback, options)` atomically applies non-overlapping
edits after checking the expected revision and optional preimages. The result
contains an attributed transaction ID. `DocumentUndo(callback, options)` reverts
that exact transaction only while it is still the most recent transaction from
the same plugin. Red remains responsible for buffer state, dirty tracking, LSP
notifications, and undo history.

Plugin-owned storage is captured as an opaque namespaced extension in Red session
snapshots. Unknown extensions survive load/save cycles, allowing a package to
restore its workflow without Red understanding the payload.

These calls were introduced in host API `0.6.0`.

## Workspace file operations

`FileOperation(callback: fn(Json), operation: Json)` applies a structured filesystem
operation inside the active workspace. Supported `kind` values are `create`,
`create_file`, `create_directory`, `rename`, `move`, `copy`, `delete`, `trash`,
`restore`, `undo_trash`, and `stat`. Mutation paths must be workspace-relative; the host
rejects absolute paths, parent traversal, workspace-root mutation, symlink escapes,
self/descendant copies, and implicit overwrites.

Create requests accept `path`; `create` treats a trailing slash as a directory and
supports bounded Bash-style list and range brace expansion. Rename, move, and copy
accept `source` and `destination`. Delete, trash, restore, and undo accept `paths:
[String]`. Results contain `ok`, an optional `error`, and operation-specific path data.
Trash restoration is available only on platforms whose system trash API exposes stable
item identities.

`FileOperation` was introduced in host API `0.4.0`.

## Compact plugin dialogs

`OpenInput(title: String, initial: String, handlers: ComposerHandlers)` opens the same
compact, single-line input used by LSP rename. It submits through
`ComposerHandlers.submitted` and cancels through `ComposerHandlers.cancelled`.

`OpenConfirm(title: String, message: String, handlers: PickerHandlers)` opens a compact
Accept/Cancel dialog. Cancel is selected by default; Left or `y` selects Accept, Right or
`n` selects Cancel, Enter confirms the selection, and Escape cancels. Accept invokes
`PickerHandlers.selected` with an item whose `id` is `accept`; cancellation invokes
`PickerHandlers.cancelled`.

Both calls were introduced in host API `0.4.0`, and their callback handles remain owned
and released by the calling plugin.

## Declarative plugin authoring

Top-level functions can declare commands, event listeners, state, configuration,
and lifecycle hooks directly:

```husk
struct SymbolsState {
    request: i32,
    enabled: bool,
}

#[red::state]
fn initial_state() -> SymbolsState {
    return SymbolsState { request: -1, enabled: true };
}

#[red::config("plugin_config")]
fn configuration_loaded(event: Json) {
    red::state_patch(SymbolsState {
        enabled: event.value.symbols.enabled,
    });
}

#[red::command(
    name = "LspDocumentSymbols",
    title = "Show document symbols",
    category = "LSP",
    description = "Find symbols in the current document",
    aliases = ["outline", "symbols"],
    scope = "global",
)]
fn document_symbols() {
    red::request("DocumentSymbols", show_document_symbols);
}

#[red::on("timeout:callback")]
fn symbol_batch_timeout(event: TimerEvent) {
    // Handle a host-owned timer notification.
}

#[red::lifecycle("deactivate")]
fn stop_background_work() {
    // Cancel timers or release plugin-owned resources.
}
```

`#[red::command]` requires a nonempty `name`, a zero-argument function, and
optional string `title`, `category`, and `description` fields plus an optional
string-array `aliases` field. `visible = false` hides a command from the command
palette and colon completion without disabling direct invocation or keymaps.
The optional `scope` is `"editor"` by default. Set it to `"global"` only for
commands that are safe and meaningful when a plugin panel owns focus, such as
workspace pickers or pane toggles. Packages using command scope must declare
`"red_api_version": "^0.7.0"`.
`#[red::on]` takes exactly one nonempty event-name string and requires a
one-argument function. A function may subscribe to multiple distinct events by
repeating `#[red::on(...)]`.

`#[red::state]` marks one zero-argument initializer with an explicit named record
return type. Red runs it after static registration and before activation, stores
its concrete record privately for that plugin, and makes it available through
`red::state()`. Update individual named fields with a sparse record literal:

```husk
red::state_patch(SymbolsState { enabled: false });
```

Patches retain every omitted field and reject unknown fields or records that do
not belong to the current plugin. They avoid rebuilding unrelated collections,
which makes them preferable for state containing picker results, transcript
blocks, or other larger values. Use `red::state_set(state)` only when replacing
the complete record intentionally. Existing `red::state("key")` and
`red::state_set("key", value)` calls remain supported and independent of the
typed record; reserve them for explicit compatibility or high-volume payload
boundaries that cannot safely live inside the typed state snapshot.

`#[red::config("key")]` binds a one-argument callback to an initial `GetConfig`
request. `#[red::config]` requests the complete configuration. Configuration
requests are staged and delivered only after state initialization and activation
succeed. Each key may be bound once per plugin.

`#[red::lifecycle("hook")]` binds a function to `activate`, `deactivate`,
`before_exit`, `state_export`, or `state_import`; activation, deactivation, and
export callbacks take no arguments, while exit and import callbacks take one.
An explicitly annotated hook takes precedence over a conventionally named
function, which remains supported for compatibility.

Red validates these annotations before activation, retains source diagnostics,
and registers their generation-safe callbacks in source order. State
initialization, activation, configuration requests, and lifecycle registration
participate in the same transactional reload as imperative commands and
listeners: failed validation, ownership conflicts, initializer failures, or
activation failures cannot replace or leak the previous plugin's wiring.
Annotations are supported on top-level functions only, including functions in
package source modules. Other attribute namespaces are ignored by Red.

`red::add_command(name, callback[, metadata])` accepts an optional `Json` object
with `title`, `category`, `description`, `aliases: [String]`, `visible: bool`,
and `scope`. Red uses visible commands to populate the command palette; aliases
are search terms and do not create alternate colon commands. The palette shows
the exact, case-sensitive `:Name` invocation when it is available and resolves
keymaps from the user's effective configuration.

The optional scope is `"editor"` by default. Set it to `"global"` only for
commands that are safe and meaningful when a plugin panel owns focus, such as
workspace pickers or pane toggles. Global commands use the configured
normal-mode binding from focused panels, subject to keys reserved by that
panel's own input handling. Existing two-argument registrations and metadata
without a scope continue to work. Imperative `red::add_command` and `red::on`
remain supported for dynamic or conditional registrations, including
process-specific and filesystem-watch-specific event names.

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

`OpenPicker` was added in host API `0.3.0`. New plugins targeting this Red release should
declare `"red_api_version": "^0.7.0"`. The numeric-ID `OpenDynamicPicker` API remains
available for compatibility, but new plugins should not use it.

## Agent composer

Plugins that collect a multiline request should call `OpenComposer(title: String, query: String, history: [String], handlers: ComposerHandlers)`. `submitted` receives the complete prompt as a `String`; `cancelled` receives a `ComposerCancelled` record. Both are terminal one-shot callbacks scoped to the plugin that opened the composer, so plugins do not allocate IDs or register synthetic event names.

```husk
red::execute("OpenComposer", "Agent prompt", draft, history, ComposerHandlers {
    submitted: prompt_submitted,
    cancelled: prompt_cancelled,
});
```

The host owns multiline editing, wrapping, cursor movement, and history navigation; it does not send a callback for each keystroke. Input is limited to 128 KiB so an escaping-heavy prompt remains within the Codex app-server frame limit; an oversized paste leaves the current draft intact and shows a validation message. Enter submits, `Ctrl-j` or Shift-Enter inserts a newline, Escape or `Ctrl-c` cancels, and `Ctrl-p` / `Ctrl-n` moves through the supplied history while preserving the current draft.

`OpenComposer` was introduced in host API `0.3.0`. The numeric-ID `OpenAgentComposer` API and its `composer:submitted:<id>` / `composer:cancelled:<id>` events remain available for compatibility with `0.2.0` plugins.

`AgentArchiveSession(session_id: String)` was also introduced in host API `0.2.0`. Use it when Codex app-server has already stopped: pending proposals remain reviewable, and the host does not send an interrupt to a replacement process that may reuse the same session ID. Use `AgentCloseSession(session_id: String)` for a live session that should be closed normally.

`AgentPrompt` automatically attaches bounded editor context containing the active visual selection or a roughly 80-line cursor excerpt, unsaved-state metadata, cursor/range, and intersecting diagnostics. Files outside the workspace, ignored paths, common credential/secret filenames, and binary buffers are omitted. Plugins that need to inspect or explicitly override this context can call `GetAgentContext(callback)` and `AgentPromptWithContext(session_id: String, text: String, context: Json)`; the context object accepts `uri` and `text` fields and is included in the direct Codex turn.

## Text panels

`CreateTextPanel`, `UpdateTextPanel`, and `AppendTextPanel` provide a source-backed conversation surface. `TextPanelBlock` accepts an `id`, `kind` (`user`, `agent`, `error`, or `text`), `format` (`plain` or `markdown`), and `text`; the host preserves the source while wrapping and rendering it for the current panel width. These calls were introduced in host API `0.2.0`.

Both row and text panels accept `side: "left"`, `"right"`, `"top"`, or
`"bottom"` in `PanelConfig`. `width` is measured in columns for left/right panes
and rows for top/bottom panes. Users can move a focused pane using
`Ctrl-w H/J/K/L`, resize it using `Ctrl-w <` / `>` or `Ctrl-w -` / `+`, or drag
its divider. The host remembers widths and heights independently and preserves
the pane's stable ID, source blocks, scroll state, focus, and composer draft.
While dragging, the host highlights only the captured divider. It prefers the
theme's `sash.hoverBorder`, `panelTitle.activeBorder`, or `focusBorder`, then
derives a readable accent when those colors are absent or insufficiently
contrasted. Plugins do not need to configure a border to make resizing visible.

`PanelConfig` may include `composer: Json { placeholder: String, rows: i32 }` for a persistent footer composer and `header_actions: [Json { id: String, label: String, compact_label?: String }]` for clickable, right-aligned header controls. Row panels can also set `surface: ThemeStyleSpec` and `border: ThemeStyleSpec` to resolve theme-aware panel foreground, background, and separator colors without affecting other panels. Header actions emit `panel:event:<id>` using their configured `id`; compact labels are selected automatically on narrow panels, with the rightmost actions retained when space is especially limited. Focus the footer with `FocusTextPanelComposer(id)`, update its enabled/status state with `SetTextPanelComposerState(id, enabled, status?)`, and clear its draft with `ClearTextPanelComposer(id)`. A focused composer supports Unicode-safe editing, paste, wrapping, click-to-position cursor movement, `Ctrl-p`/`Ctrl-n` local history, Enter to submit, and `Ctrl-j` or Shift-Enter for a newline. It emits `panel:event:<id>` with `action: "submit"` and the complete `text`; other footer actions include `composer_focus`, `composer_blur`, `interrupt`, `clear`, `new`, `history`, and `close`. `SetPanelVisible(id, visible)` hides or restores a panel without discarding its blocks, scroll position, or draft. Replacing text-panel blocks with an empty list resets scrolling and restores tail-following. Footer panels shrink on narrow terminals while preserving an editor viewport.

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

Event listeners and request callbacks decode host payloads according to their declared
parameter types. Named records, arrays, tuples, and nested `Option<T>` values are
decoded recursively. Both JSON `null` and omitted optional fields become `None`; a
present value becomes `Some(value)`. Existing callbacks declared as `Json` retain their
dynamic behavior, and additional object fields are preserved for compatibility.

Tagged JSON objects also decode into nominal Husk enum variants. The host recognizes
`type`, `session_update`, `kind`, and `$case` discriminators and maps snake-case wire tags to
PascalCase variants. Declare an `Unknown(Json)` case to preserve forward-compatible
payloads when the host adds a new variant. Plugin process events share this host enum:

```husk
fn process_finished(event: ProcessEvent) {
    match event {
        ProcessEvent::Exit { process_id, code, plugin_name } => {
            if let Some(exit_code) = code {
                red::execute("Print", process_id + " exited with " + exit_code);
            }
        }
        ProcessEvent::Stdout { line, process_id, plugin_name } => {}
        ProcessEvent::Stderr { line, process_id, plugin_name } => {}
        ProcessEvent::Error { message, process_id, plugin_name } => {}
    }
}
```

When typed records cross back into host actions or plugin storage, `None` serializes to
JSON `null` and `Some(value)` serializes to the value itself, including inside arrays and
nested records. `Json` remains intentional for persisted user-defined state, arbitrary
configuration, genuinely open-ended process output, and plugin-defined payloads such as
`PickerItem.data`. Prefer nominal records and enums whenever the host owns the shape.

## Transactional reload and state

User plugin files are polled with a 250 ms debounce. A replacement VM is parsed,
typechecked, statically registered, activated, and migrated before it replaces
the live program. A bad save
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
